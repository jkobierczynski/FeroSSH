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
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ferossh_auth::keyfile::KeyFile;
use ferossh_auth::userauth;
use ferossh_ipc::{Message, PROTOCOL_VERSION, Timing};
use ferossh_term::{Action, Settings, Terminal};
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

const fn login(kex: &'static str, cipher: &'static str) -> Expect {
    Login { kex, cipher }
}

const MLKEM: &str = "mlkem768x25519-sha256";
const C25519: &str = "curve25519-sha256";
const C25519_LIBSSH: &str = "curve25519-sha256@libssh.org";
const NISTP256: &str = "ecdh-sha2-nistp256";
const CHACHA: &str = "chacha20-poly1305@openssh.com";
const GCM: &str = "aes256-gcm@openssh.com";
const CTR: &str = "aes256-ctr";
const NO_ALGO: &str = "FeroSSH allows";
const HINT: &str = "LegacyAlgorithms yes";

/// (image tag, server, legacy set on, expected outcome).
const CASES: &[(&str, &str, bool, Expect)] = &[
    // OpenSSH 7.6 has no strict key exchange, so ChaCha20 is out and
    // FeroSSH connects again with AES-GCM.
    ("18.04", "openssh", false, login(C25519, GCM)),
    // Ubuntu backported strict key exchange to 8.2 and later.
    ("20.04", "openssh", false, login(C25519, CHACHA)),
    ("22.04", "openssh", false, login(C25519, CHACHA)),
    ("24.04", "openssh", false, login(C25519, CHACHA)),
    ("26.04", "openssh", false, login(MLKEM, CHACHA)),
    // Dropbear 2017.75: P-521 and SHA-1 RSA host keys, CTR ciphers.
    ("18.04", "dropbear", false, Refused(HINT)),
    ("18.04", "dropbear", true, login(C25519, CTR)),
    // Dropbear 2019.78: CTR and CBC ciphers only.
    ("20.04", "dropbear", false, Refused(HINT)),
    ("20.04", "dropbear", true, login(C25519, CTR)),
    ("22.04", "dropbear", false, login(C25519, CHACHA)),
    ("24.04", "dropbear", false, login(C25519, CHACHA)),
    ("26.04", "dropbear", false, login(MLKEM, CHACHA)),
    // A switch- or router-like algorithm set: refused with the hint, and
    // reached with the legacy set. On OpenSSH 7.6 (no strict key exchange)
    // the EtM MAC it offers is open to Terrapin, so FeroSSH connects again
    // with the plain one.
    ("24.04", "gear", false, Refused(HINT)),
    ("24.04", "gear", true, login(NISTP256, CTR)),
    ("18.04", "gear", true, login(NISTP256, CTR)),
    // Only CBC: nothing FeroSSH allows, and no hint either.
    ("24.04", "cbc", true, Refused(NO_ALGO)),
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

/// Throwaway Ed25519 and P-256 user keys (Dropbear before 2020.79 has no
/// Ed25519), their public halves in `authorized_keys`.
fn user_keys(dir: &Path) -> Vec<KeyFile> {
    let mut authorized = String::new();
    let mut keys = Vec::new();
    for (kind, bits) in [("ed25519", "256"), ("ecdsa", "256")] {
        let path = dir.join(format!("id_{kind}"));
        let ok = Command::new("ssh-keygen")
            .args([
                "-q", "-t", kind, "-b", bits, "-N", "", "-C", "interop", "-f",
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        authorized.push_str(&std::fs::read_to_string(path.with_extension("pub")).unwrap());
        keys.push(KeyFile::load(&path).unwrap());
    }
    std::fs::write(dir.join("authorized_keys"), authorized).unwrap();
    keys
}

#[derive(Debug, Default)]
struct Outcome {
    connected: Option<(String, String, String)>,
    output: Vec<u8>,
    disconnected: String,
    host_key_prompts: usize,
    /// From logging in to the worker reporting the connection gone.
    lasted: Option<Duration>,
}

/// Plays the UI for one connection: accepts the host key, signs with the
/// user key, runs `command` once logged in.
async fn connect(port: u16, keys: &[KeyFile], legacy: bool, command: &str) -> Outcome {
    session(port, keys, legacy, Timing::default(), command, || {}).await
}

/// [`connect`] with `timing`, calling `on_login` once logged in.
async fn session(
    port: u16,
    keys: &[KeyFile],
    legacy: bool,
    timing: Timing,
    command: &str,
    on_login: impl FnOnce(),
) -> Outcome {
    let mut on_login = Some(on_login);
    let mut since: Option<Instant> = None;
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
        legacy,
        timing,
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
                seen.host_key_prompts += 1;
                send(Message::HostKeyVerdict {
                    channel: 1,
                    accept: true,
                })
                .await;
            }
            Message::IdentitiesRequest { .. } => {
                send(Message::Identities {
                    channel: 1,
                    keys: keys.iter().map(|k| k.public.clone()).collect(),
                })
                .await;
            }
            Message::SignRequest { key: k, data, .. } => {
                let key = keys
                    .iter()
                    .find(|key| key.public == k)
                    .expect("one of ours");
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
                since = Some(Instant::now());
                if let Some(f) = on_login.take() {
                    f();
                }
            }
            Message::Data { bytes, .. } => seen.output.extend_from_slice(&bytes),
            // The keys were refused and the server fell back to a password.
            Message::Prompt { .. } => panic!("asked for a password; so far {seen:?}"),
            Message::Disconnected { reason, .. } => {
                seen.disconnected = reason;
                seen.lasted = since.map(|t| t.elapsed());
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
    let keys = user_keys(&dir);
    let mut failures = Vec::new();
    for &(tag, server, legacy, expect) in CASES {
        let name = format!(
            "{server} on ubuntu {tag}{}",
            if legacy { " (legacy)" } else { "" }
        );
        let container = Server::start(tag, server, &dir);
        let seen = connect(container.port, &keys, legacy, "echo fero$((6*7)); exit\n").await;
        let output = String::from_utf8_lossy(&seen.output);
        let ok = match (expect, &seen.connected) {
            (Login { kex, cipher }, Some((k, c, _))) => {
                // Older OpenSSH and Dropbear name curve25519 the libssh way.
                let kex_ok = k == kex || (kex == C25519 && k == C25519_LIBSSH);
                // A retry without ChaCha20 must not ask about the key again.
                kex_ok && c == cipher && output.contains("fero42") && seen.host_key_prompts == 1
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

/// A server that stops answering (frozen, or the network gone) is noticed
/// by the keepalives within interval x count, and the reason says so; one
/// that restarts ends the session at once. Either way a new connection
/// works afterwards.
#[tokio::test]
async fn dead_and_restarted_servers() {
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-interop-dead-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let keys = user_keys(&dir);
    let container = Server::start("24.04", "openssh", &dir);
    let timing = Timing {
        connect_timeout: 5,
        alive_interval: 1,
        alive_max: 2,
    };

    let seen = session(container.port, &keys, false, timing, "", || {
        docker(&["pause", &container.id]).expect("docker pause");
    })
    .await;
    let _ = docker(&["unpause", &container.id]);
    let lasted = seen.lasted.expect("logged in");
    assert!(lasted < Duration::from_secs(10), "{seen:?}");
    assert!(seen.disconnected.contains("stopped responding"), "{seen:?}");

    // Silent keepalives keep an idle session open.
    let seen = session(
        container.port,
        &keys,
        false,
        timing,
        "sleep 4; exit\n",
        || {},
    )
    .await;
    assert!(
        seen.lasted.expect("logged in") >= Duration::from_secs(4),
        "{seen:?}"
    );

    let seen = session(container.port, &keys, false, timing, "", || {
        docker(&["restart", "-t", "0", &container.id]).expect("docker restart");
    })
    .await;
    assert!(
        seen.lasted.expect("logged in") < Duration::from_secs(10),
        "{seen:?}"
    );
    assert!(
        seen.disconnected.contains("closed the connection") || seen.disconnected.contains("reset"),
        "{seen:?}"
    );

    // The published port can change on restart.
    let port = docker(&["port", &container.id, "22/tcp"])
        .and_then(|p| p.lines().next()?.rsplit(':').next()?.parse().ok())
        .unwrap();
    let restarted = Server {
        id: container.id.clone(),
        port,
    };
    restarted.wait_for_banner();
    std::mem::forget(restarted);
    let seen = connect(port, &keys, false, "echo fero$((6*7)); exit\n").await;
    assert!(
        String::from_utf8_lossy(&seen.output).contains("fero42"),
        "{seen:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A full-screen app that sets the title and quits gives the title back,
/// whether the host's terminfo saves and restores it around the alternate
/// screen (xterm-256color) or not (screen, like mc on some Debian hosts).
#[tokio::test]
async fn full_screen_apps_give_the_title_back() {
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-interop-title-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let keys = user_keys(&dir);
    let container = Server::start("24.04", "openssh", &dir);
    for term in ["xterm-256color", "screen"] {
        let command = format!(
            "printf '\\033]2;shell\\007'; export TERM={term}; tput smcup; \
             printf '\\033]0;mc [fero]:~\\007'; tput rmcup; echo fero$((6*7)); exit\n"
        );
        let seen = connect(container.port, &keys, false, &command).await;
        let output = String::from_utf8_lossy(&seen.output);
        assert!(output.contains("fero42"), "{term}: {seen:?}");
        let mut t = Terminal::new(80, 24, &Settings::default());
        t.feed(&seen.output);
        let titles: Vec<Option<String>> = t
            .take_actions()
            .into_iter()
            .filter_map(|a| match a {
                Action::Title(t) => Some(t),
                _ => None,
            })
            .collect();
        assert!(
            titles.contains(&Some("mc [fero]:~".to_owned())),
            "{term}: {titles:?}"
        );
        assert_eq!(
            titles.last(),
            Some(&Some("shell".to_owned())),
            "{term}: {titles:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
