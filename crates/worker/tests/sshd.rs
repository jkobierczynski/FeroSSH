//! End-to-end tests against a real OpenSSH server.
//!
//! Each test starts its own `sshd` on a free local port with a throwaway
//! host key and user key, then drives the worker over its message channels
//! the way the UI does: it checks the host key, offers the user key, checks
//! and answers the sign request through the auth crate, and runs a command.
//! Without `sshd` and `ssh-keygen` (Windows CI) the tests pass without
//! running; the Linux CI job has both.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ferossh_auth::keyfile::KeyFile;
use ferossh_auth::userauth;
use ferossh_ipc::{Message, PROTOCOL_VERSION};
use russh::keys::PublicKey;
use tokio::sync::mpsc;

const SSHD: &str = "/usr/sbin/sshd";

struct Server {
    dir: PathBuf,
    port: u16,
    child: Child,
    host_key: Vec<u8>,
    user_key: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn keygen(path: &Path, kind: &str) {
    let ok = Command::new("ssh-keygen")
        .args(["-q", "-t", kind, "-N", "", "-C", "test", "-f"])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

fn public_blob(path: &Path) -> Vec<u8> {
    let text = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    PublicKey::from_openssh(text.trim())
        .unwrap()
        .to_bytes()
        .unwrap()
}

fn available() -> bool {
    let found = Path::new(SSHD).exists()
        && Command::new("ssh-keygen")
            .arg("-?")
            .stderr(Stdio::null())
            .status()
            .is_ok();
    if !found {
        // CI sets this so a missing sshd fails instead of passing quietly.
        assert!(
            std::env::var_os("FEROSSH_REQUIRE_SSHD").is_none(),
            "FEROSSH_REQUIRE_SSHD is set but sshd or ssh-keygen is missing"
        );
        eprintln!("sshd or ssh-keygen not found; skipping");
    }
    found
}

/// Starts sshd with `extra` appended to a minimal config.
fn start(name: &str, extra: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("ferossh-sshd-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = dir.join("host_ed25519");
    let user = dir.join("user_ed25519");
    keygen(&host, "ed25519");
    keygen(&user, "ed25519");
    std::fs::copy(user.with_extension("pub"), dir.join("authorized_keys")).unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = format!(
        "ListenAddress 127.0.0.1\nPort {port}\nHostKey {host}\nAuthorizedKeysFile {keys}\n\
         PidFile none\nStrictModes no\nUsePAM no\nPermitRootLogin yes\n\
         PasswordAuthentication no\nKbdInteractiveAuthentication no\n{extra}\n",
        host = host.display(),
        keys = dir.join("authorized_keys").display(),
    );
    std::fs::write(dir.join("sshd_config"), config).unwrap();
    // sshd running as root wants its privilege separation directory.
    let _ = std::fs::create_dir_all("/run/sshd");
    let child = Command::new(SSHD)
        .args(["-D", "-e", "-f"])
        .arg(dir.join("sshd_config"))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Server {
        host_key: public_blob(&host),
        user_key: user,
        dir,
        port,
        child,
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| {
            let out = Command::new("id").arg("-un").output().unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        })
}

/// What the fake UI saw.
#[derive(Default, Debug)]
struct Seen {
    output: Vec<u8>,
    exit: Option<u32>,
    connected: Option<(String, String, String)>,
    disconnected: String,
    signed: usize,
}

/// Plays the UI for one connection until the worker reports it is gone.
async fn run_ui(server: &Server, command: &str, trust: bool) -> Seen {
    let (to_worker, worker_in) = mpsc::channel(64);
    let (worker_out, mut from_worker) = mpsc::channel(64);
    tokio::spawn(ferossh_worker::serve(worker_in, worker_out));
    let user = whoami();
    let key = KeyFile::load(&server.user_key).unwrap();
    let send = |m| {
        let tx = to_worker.clone();
        async move { tx.send(m).await.unwrap() }
    };
    send(Message::Hello {
        version: PROTOCOL_VERSION,
    })
    .await;
    send(Message::Connect {
        channel: 1,
        host: "127.0.0.1".into(),
        port: server.port,
        user: user.clone(),
        cols: 80,
        rows: 24,
        host_key_algs: vec![],
    })
    .await;
    let mut seen = Seen::default();
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    loop {
        let msg = tokio::select! {
            m = from_worker.recv() => m.expect("worker hung up"),
            () = &mut deadline => panic!("timed out; so far {seen:?}"),
        };
        match msg {
            Message::Hello { version } => assert_eq!(version, PROTOCOL_VERSION),
            Message::HostKey { key, .. } => {
                assert_eq!(key, server.host_key);
                send(Message::HostKeyVerdict {
                    channel: 1,
                    accept: trust,
                })
                .await;
            }
            Message::IdentitiesRequest { .. } => {
                send(Message::Identities {
                    channel: 1,
                    keys: vec![key.public.clone()],
                })
                .await;
            }
            Message::SignRequest { key: k, data, .. } => {
                assert_eq!(k, key.public);
                userauth::check(&data, &user, &k).expect("a user-auth request");
                seen.signed += 1;
                send(Message::Signature {
                    channel: 1,
                    signature: key.sign(None, &data).unwrap(),
                })
                .await;
            }
            Message::Connected {
                kex,
                cipher,
                host_key,
                ..
            } => {
                seen.connected = Some((kex, cipher, host_key));
                send(Message::Data {
                    channel: 1,
                    bytes: command.as_bytes().to_vec(),
                })
                .await;
            }
            Message::Data { bytes, .. } => seen.output.extend_from_slice(&bytes),
            Message::ExitStatus { code, .. } => seen.exit = Some(code),
            Message::Disconnected { reason, .. } => {
                seen.disconnected = reason;
                return seen;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn logs_in_with_a_key_and_runs_a_shell() {
    if !available() {
        return;
    }
    let server = start("login", "");
    let seen = run_ui(&server, "echo fero$((6*7)); exit 3\n", true).await;
    let output = String::from_utf8_lossy(&seen.output);
    assert!(output.contains("fero42"), "{seen:?}");
    assert_eq!(seen.exit, Some(3));
    assert_eq!(seen.signed, 1);
    let (kex, cipher, host_key) = seen.connected.unwrap();
    // OpenSSH 9.9 and later offer ML-KEM; older ones stop at curve25519.
    assert!(
        kex == "mlkem768x25519-sha256" || kex == "curve25519-sha256",
        "{kex}"
    );
    assert_eq!(cipher, "chacha20-poly1305@openssh.com");
    assert_eq!(host_key, "ssh-ed25519");
}

#[tokio::test]
async fn rejected_host_key_stops_before_authentication() {
    if !available() {
        return;
    }
    let server = start("reject", "");
    let seen = run_ui(&server, "", false).await;
    assert_eq!(seen.signed, 0);
    assert!(seen.disconnected.contains("not accepted"), "{seen:?}");
}

#[tokio::test]
async fn refuses_servers_with_only_banned_algorithms() {
    if !available() {
        return;
    }
    let cases = [
        ("cbc", "Ciphers aes128-cbc,aes256-ctr"),
        (
            "sha1kex",
            "KexAlgorithms diffie-hellman-group14-sha1,diffie-hellman-group-exchange-sha1",
        ),
        ("nistkex", "KexAlgorithms ecdh-sha2-nistp256"),
        (
            "etm",
            "Ciphers aes256-ctr\nMACs hmac-sha2-256-etm@openssh.com",
        ),
    ];
    for (name, config) in cases {
        let server = start(name, config);
        let seen = run_ui(&server, "", true).await;
        assert!(seen.connected.is_none(), "{name}: {seen:?}");
        assert!(
            seen.disconnected.contains("FeroSSH allows"),
            "{name}: {}",
            seen.disconnected
        );
    }
}

#[tokio::test]
async fn falls_back_to_aes_gcm() {
    if !available() {
        return;
    }
    let server = start(
        "gcm",
        "Ciphers aes256-gcm@openssh.com\nKexAlgorithms curve25519-sha256",
    );
    let seen = run_ui(&server, "exit\n", true).await;
    let (kex, cipher, _) = seen.connected.unwrap();
    assert_eq!(kex, "curve25519-sha256");
    assert_eq!(cipher, "aes256-gcm@openssh.com");
}
