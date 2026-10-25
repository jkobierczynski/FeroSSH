//! Interoperability against real servers of many ages, in containers.
//!
//! `interop/build.sh` builds one image per Ubuntu release, each with that
//! release's OpenSSH, Dropbear and tmux. Each case here starts one
//! container, logs in with a throwaway key through the worker the way the
//! UI does, and checks what was negotiated, or that FeroSSH refused the
//! server with a message that says why.
//!
//! Without docker or the images the tests pass without running;
//! the Linux CI job sets `FEROSSH_REQUIRE_INTEROP=1` to fail instead.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::Read;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ferossh_auth::keyfile::KeyFile;
use ferossh_auth::userauth;
use ferossh_ipc::{Message, PROTOCOL_VERSION};
use tokio::sync::mpsc;

/// What a case should end with.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// Logged in with this kex and cipher.
    Login {
        kex: &'static str,
        cipher: &'static str,
    },
    /// Refused before login, with a reason containing this.
    Refused(&'static str),
}

use Expect::{Login, Refused};

const MLKEM: &str = "mlkem768x25519-sha256";
const C25519: &str = "curve25519-sha256";
const C25519_LIBSSH: &str = "curve25519-sha256@libssh.org";
const CHACHA: &str = "chacha20-poly1305@openssh.com";
const NO_ALGO: &str = "FeroSSH allows";
const TERRAPIN: &str = "Terrapin";

/// (image tag, server, expected outcome).
const CASES: &[(&str, &str, Expect)] = &[
    // OpenSSH 7.6 has neither strict key exchange nor ML-KEM. It offers
    // AES-GCM too, but FeroSSH picks ChaCha20 first and then refuses it.
    ("18.04", "openssh", Refused(TERRAPIN)),
    // Ubuntu backported strict key exchange to 8.2 and later.
    (
        "20.04",
        "openssh",
        Login {
            kex: C25519,
            cipher: CHACHA,
        },
    ),
    (
        "22.04",
        "openssh",
        Login {
            kex: C25519,
            cipher: CHACHA,
        },
    ),
    (
        "24.04",
        "openssh",
        Login {
            kex: C25519,
            cipher: CHACHA,
        },
    ),
    (
        "26.04",
        "openssh",
        Login {
            kex: MLKEM,
            cipher: CHACHA,
        },
    ),
    // Dropbear 2017.75: no Ed25519 host key, P-521 and SHA-1 RSA only.
    ("18.04", "dropbear", Refused(NO_ALGO)),
    // Dropbear 2019.78: CTR and CBC ciphers only.
    ("20.04", "dropbear", Refused(NO_ALGO)),
    (
        "22.04",
        "dropbear",
        Login {
            kex: C25519,
            cipher: CHACHA,
        },
    ),
    (
        "24.04",
        "dropbear",
        Login {
            kex: C25519,
            cipher: CHACHA,
        },
    ),
    (
        "26.04",
        "dropbear",
        Login {
            kex: MLKEM,
            cipher: CHACHA,
        },
    ),
    // A switch- or router-like algorithm set.
    ("24.04", "gear", Refused(NO_ALGO)),
];

fn docker(args: &[&str]) -> Option<String> {
    let out = Command::new("docker")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn available() -> bool {
    let found = CASES.iter().all(|(tag, ..)| {
        docker(&["image", "inspect", &format!("ferossh-interop:{tag}")]).is_some()
    }) && Command::new("ssh-keygen")
        .arg("-?")
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if !found {
        assert!(
            std::env::var_os("FEROSSH_REQUIRE_INTEROP").is_none(),
            "FEROSSH_REQUIRE_INTEROP is set but docker, the interop images \
             (interop/build.sh) or ssh-keygen is missing"
        );
        eprintln!("docker, interop images or ssh-keygen missing; skipping");
    }
    found
}

/// One running container; removed on drop.
struct Server {
    id: String,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = docker(&["rm", "-f", &self.id]);
    }
}

impl Server {
    fn start(tag: &str, server: &str, keys: &Path) -> Server {
        let mount = format!("{}:/keys:ro", keys.display());
        let image = format!("ferossh-interop:{tag}");
        let id = docker(&[
            "run",
            "-d",
            "-p",
            "127.0.0.1:0:22",
            "-v",
            &mount,
            &image,
            server,
        ])
        .expect("docker run");
        let port = docker(&["port", &id, "22/tcp"])
            .and_then(|p| p.lines().next()?.rsplit(':').next()?.parse().ok())
            .expect("published port");
        let server = Server { id, port };
        server.wait_for_banner();
        server
    }

    /// Docker's proxy accepts before the server listens, so wait for the
    /// SSH identification line itself.
    fn wait_for_banner(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = [0u8; 4];
                if s.read_exact(&mut buf).is_ok() && &buf == b"SSH-" {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let logs = Command::new("docker").args(["logs", &self.id]).output();
        panic!("container {} never spoke SSH: {logs:?}", self.id);
    }
}

/// A throwaway user key, its public half as `authorized_keys`.
fn user_key(dir: &Path) -> PathBuf {
    let key = dir.join("id_ed25519");
    let ok = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "interop", "-f"])
        .arg(&key)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    std::fs::copy(key.with_extension("pub"), dir.join("authorized_keys")).unwrap();
    key
}

#[derive(Debug, Default)]
struct Outcome {
    connected: Option<(String, String, String)>,
    output: Vec<u8>,
    disconnected: String,
}

/// Plays the UI for one connection: accepts the host key, signs with the
/// user key, runs `command` once logged in.
async fn connect(port: u16, key: &KeyFile, command: &str) -> Outcome {
    let (to_worker, worker_in) = mpsc::channel(64);
    let (worker_out, mut from_worker) = mpsc::channel(64);
    tokio::spawn(ferossh_worker::serve(worker_in, worker_out));
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
        port,
        user: "fero".into(),
        cols: 80,
        rows: 24,
        host_key_algs: vec![],
        x11: None,
    })
    .await;
    let mut seen = Outcome::default();
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    loop {
        let msg = tokio::select! {
            m = from_worker.recv() => m.expect("worker hung up"),
            () = &mut deadline => panic!("timed out; so far {seen:?}"),
        };
        match msg {
            Message::HostKey { .. } => {
                send(Message::HostKeyVerdict {
                    channel: 1,
                    accept: true,
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
                userauth::check(&data, "fero", &k).expect("a user-auth request");
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
            Message::Disconnected { reason, .. } => {
                seen.disconnected = reason;
                return seen;
            }
            _ => {}
        }
    }
}

/// Runs every case and reports all mismatches at once, so one CI run
/// shows the whole matrix.
#[tokio::test]
async fn servers_of_every_age() {
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-interop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let key_path = user_key(&dir);
    let key = KeyFile::load(&key_path).unwrap();
    let mut failures = Vec::new();
    for &(tag, server, expect) in CASES {
        let name = format!("{server} on ubuntu {tag}");
        let container = Server::start(tag, server, &dir);
        let seen = connect(container.port, &key, "echo fero$((6*7)); exit\n").await;
        let output = String::from_utf8_lossy(&seen.output);
        let ok = match (expect, &seen.connected) {
            (Login { kex, cipher }, Some((k, c, _))) => {
                // Older OpenSSH and Dropbear name curve25519 the libssh way.
                let kex_ok = k == kex || (kex == C25519 && k == C25519_LIBSSH);
                kex_ok && c == cipher && output.contains("fero42")
            }
            (Refused(why), None) => seen.disconnected.contains(why),
            _ => false,
        };
        eprintln!(
            "{name}: {}",
            match &seen.connected {
                Some((k, c, h)) => format!("logged in, {k} / {c} / {h}"),
                None => format!("refused: {}", seen.disconnected),
            }
        );
        if !ok {
            failures.push(format!("{name}: expected {expect:?}, got {seen:?}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
