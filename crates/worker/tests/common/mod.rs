//! What the agent and security key tests share: a throwaway `sshd`, and
//! logging in through the worker with one identity the way the window
//! does, every sign request checked and sent to the broker.

#![allow(dead_code)]

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ferossh_auth::{Broker, Identity, Job, Reply, userauth};
use ferossh_ipc::{Message, PROTOCOL_VERSION};
use tokio::sync::mpsc;

pub const SSHD: &str = "/usr/sbin/sshd";

pub struct Kill(pub Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn run(cmd: &mut Command) {
    let ok = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "{cmd:?}");
}

pub fn available() -> bool {
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

pub fn whoami() -> String {
    let out = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// sshd on a free port that accepts `authorized` keys, or certificates
/// from `ca`.
pub fn sshd(dir: &Path, name: &str, authorized: &str, ca: Option<&Path>) -> (Kill, u16) {
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

pub fn broker_reply(broker: &Broker) -> Reply {
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
pub async fn log_in(
    port: u16,
    user: &str,
    broker: &Broker,
    identity: &Identity,
) -> Result<(), String> {
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
            tunnel: None,
            shell: true,
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

pub fn base64(bytes: &[u8]) -> String {
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
