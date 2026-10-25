//! Signing in through a real `ssh-agent` with every key type FeroSSH
//! takes from an agent, and a user certificate, against a real `sshd`:
//! the broker lists the agent's keys (skipping ones the policy refuses),
//! the worker logs in with each, and every sign request goes through the
//! UI's check and the broker, as in the window.
//!
//! Its own test binary, because it points `SSH_AUTH_SOCK` at its agent.
//! Pageant and the Windows OpenSSH agent speak the same protocol over a
//! named pipe; only CI on Windows and a person can try those.

#![cfg(unix)]
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ferossh_auth::{Broker, Identity, Job, Reply, userauth};
use ferossh_ipc::{Message, PROTOCOL_VERSION};
use tokio::sync::mpsc;

const SSHD: &str = "/usr/sbin/sshd";

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(cmd: &mut Command) {
    let ok = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "{cmd:?}");
}

fn available() -> bool {
    let found = Path::new(SSHD).exists()
        && ["ssh-agent", "ssh-add", "ssh-keygen"].iter().all(|c| {
            Command::new(c)
                .arg("-?")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        });
    if !found {
        assert!(
            std::env::var_os("FEROSSH_REQUIRE_SSHD").is_none(),
            "FEROSSH_REQUIRE_SSHD is set but sshd, ssh-agent or ssh-add is missing"
        );
        eprintln!("sshd or ssh-agent not found; skipping");
    }
    found
}

fn whoami() -> String {
    let out = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// sshd on a free port that accepts `authorized` keys, or certificates
/// from `ca`.
fn sshd(dir: &Path, name: &str, authorized: &str, ca: Option<&Path>) -> (Kill, u16) {
    let host = dir.join(format!("host-{name}"));
    if !host.exists() {
        run(Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&host));
    }
    let keys = dir.join(format!("authorized-{name}"));
    std::fs::write(&keys, authorized).unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = format!(
        "ListenAddress 127.0.0.1\nPort {port}\nHostKey {}\nAuthorizedKeysFile {}\n\
         PidFile none\nStrictModes no\nUsePAM no\nPasswordAuthentication no\n\
         KbdInteractiveAuthentication no\n\
         PubkeyAcceptedAlgorithms +ecdsa-sha2-nistp384,ecdsa-sha2-nistp521\n",
        host.display(),
        keys.display()
    );
    if let Some(ca) = ca {
        config.push_str(&format!("TrustedUserCAKeys {}\n", ca.display()));
    }
    let cfg = dir.join(format!("sshd-{name}"));
    std::fs::write(&cfg, config).unwrap();
    let _ = std::fs::create_dir_all("/run/sshd");
    let child = Command::new(SSHD)
        .args(["-D", "-e", "-f"])
        .arg(&cfg)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (Kill(child), port)
}

fn broker_reply(broker: &Broker) -> Reply {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(r) = broker.try_recv() {
            return r;
        }
        assert!(Instant::now() < end, "the broker did not answer");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Logs in offering only `identity`; returns the negotiated host key line
/// or the reason it failed.
async fn log_in(port: u16, user: &str, broker: &Broker, identity: &Identity) -> Result<(), String> {
    let (to_worker, worker_in) = mpsc::channel(64);
    let (worker_out, mut from_worker) = mpsc::channel(64);
    tokio::spawn(ferossh_worker::serve(worker_in, worker_out));
    for msg in [
        Message::Hello {
            version: PROTOCOL_VERSION,
        },
        Message::Connect {
            channel: 1,
            host: "127.0.0.1".into(),
            port,
            user: user.into(),
            cols: 80,
            rows: 24,
            host_key_algs: vec![],
            x11: None,
            legacy: false,
            timing: Default::default(),
        },
    ] {
        to_worker.send(msg).await.unwrap();
    }
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), from_worker.recv())
            .await
            .expect("timed out")
            .expect("worker hung up");
        let reply = match msg {
            Message::HostKey { channel, .. } => Message::HostKeyVerdict {
                channel,
                accept: true,
            },
            Message::IdentitiesRequest { channel } => Message::Identities {
                channel,
                keys: vec![identity.public.clone()],
            },
            Message::SignRequest {
                channel, key, data, ..
            } => {
                assert_eq!(key, identity.public);
                let target = userauth::check(&data, user, &key).expect("a login request");
                broker.submit(Job::Sign {
                    identity: identity.clone(),
                    data,
                    flags: target.flags,
                    passphrase: None,
                });
                let Reply::Signed(signed) = broker_reply(broker) else {
                    panic!("not a signature");
                };
                Message::Signature {
                    channel,
                    signature: signed.map_err(|e| format!("{e:?}"))?,
                }
            }
            Message::Connected { .. } => return Ok(()),
            Message::Disconnected { reason, .. } => return Err(reason),
            _ => continue,
        };
        to_worker.send(reply).await.unwrap();
    }
}

#[tokio::test]
async fn signs_in_with_every_agent_key_type_and_a_certificate() {
    if !available() {
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("ferossh-agent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let user = whoami();
    let sock = dir.join("agent.sock");
    let _agent = Kill(
        Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&sock)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if sock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: this test binary has no other threads reading the
    // environment yet.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };

    let kinds = [
        ("ed25519", "ed25519", "256"),
        ("p256", "ecdsa", "256"),
        ("p384", "ecdsa", "384"),
        ("p521", "ecdsa", "521"),
        ("rsa3072", "rsa", "3072"),
        ("rsa2048", "rsa", "2048"),
        ("certified", "ed25519", "256"),
    ];
    for (name, kind, bits) in kinds {
        run(Command::new("ssh-keygen")
            .args(["-q", "-t", kind, "-b", bits, "-N", "", "-C", name, "-f"])
            .arg(dir.join(name)));
    }
    let ca = dir.join("user-ca");
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&ca));
    run(Command::new("ssh-keygen")
        .args(["-q", "-s"])
        .arg(&ca)
        .args(["-I", "ferossh-agent-test", "-n", &user, "-V", "-5m:+1h"])
        .arg(dir.join("certified.pub")));
    for (name, ..) in kinds {
        // Adds `certified-cert.pub` along with its key.
        run(Command::new("ssh-add")
            .arg(dir.join(name))
            .env("SSH_AUTH_SOCK", &sock));
    }

    let broker = Broker::spawn(|| {}).unwrap();
    broker.submit(Job::Identities { files: vec![] });
    let Reply::Identities { keys, notes } = broker_reply(&broker) else {
        panic!("not identities");
    };
    let label = |i: &Identity| {
        let kind = userauth::key_type(&i.public).unwrap_or("?").to_owned();
        format!("{kind} {}", i.comment)
    };
    let mut offered: Vec<String> = keys.iter().map(label).collect();
    offered.sort();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("2048-bit RSA keys are too small")),
        "{notes:?}"
    );
    for want in [
        "ecdsa-sha2-nistp256 p256",
        "ecdsa-sha2-nistp384 p384",
        "ecdsa-sha2-nistp521 p521",
        "ssh-ed25519 ed25519",
        "ssh-ed25519-cert-v01@openssh.com certified",
        "ssh-rsa rsa3072",
    ] {
        assert!(
            offered.iter().any(|o| o == want),
            "{want} not in {offered:?}"
        );
    }
    assert!(
        !offered.iter().any(|o| o.ends_with("rsa2048")),
        "{offered:?}"
    );

    for identity in &keys {
        let what = label(identity);
        let certificate = userauth::is_certificate(&identity.public);
        let authorized = if certificate {
            String::new()
        } else {
            let line = format!(
                "{} {}\n",
                userauth::key_type(&identity.public).unwrap(),
                base64(&identity.public)
            );
            line
        };
        let (_server, port) = sshd(
            &dir,
            &identity.comment,
            &authorized,
            certificate.then(|| ca.with_extension("pub")).as_deref(),
        );
        log_in(port, &user, &broker, identity)
            .await
            .unwrap_or_else(|e| panic!("{what}: {e}"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
