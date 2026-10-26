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
use ferossh_auth::sk::soft::SoftAuthenticator;
use ferossh_auth::sk::{self, DEFAULT_APPLICATION, EnrollRequest, SkAlgorithm};
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
        tunnel: None,
        shell: true,
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
                    // Security keys sign through the software authenticator.
                    signature: key
                        .sign_with(None, &data, &SoftAuthenticator::new())
                        .unwrap(),
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

/// Servers that take FIDO2 security keys: OpenSSH from 8.2, Dropbear from
/// 2020.79 when built with them (Ubuntu's 2020.81 is not).
const SK_SERVERS: &[(&str, &str)] = &[
    ("20.04", "openssh"),
    ("22.04", "openssh"),
    ("24.04", "openssh"),
    ("26.04", "openssh"),
    ("24.04", "dropbear"),
    ("26.04", "dropbear"),
];

/// FeroSSH's own sk key files (as `ferossh --new-security-key` writes
/// them), their public halves in `authorized_keys`.
fn security_keys(dir: &Path) -> Vec<KeyFile> {
    let mut authorized = String::new();
    let mut keys = Vec::new();
    for (name, algorithm) in [
        ("id_ed25519_sk", SkAlgorithm::Ed25519),
        ("id_ecdsa_sk", SkAlgorithm::EcdsaP256),
    ] {
        let enrolled = sk::enroll(
            &SoftAuthenticator::new(),
            &EnrollRequest {
                algorithm,
                application: DEFAULT_APPLICATION,
                challenge: b"interop",
                platform: false,
                user_verification: false,
            },
            name,
        )
        .unwrap();
        let path = dir.join(name);
        std::fs::write(&path, &enrolled.private).unwrap();
        authorized.push_str(&enrolled.public);
        keys.push(KeyFile::load(&path).unwrap());
    }
    std::fs::write(dir.join("authorized_keys"), authorized).unwrap();
    keys
}

/// Logs in with each security key type on every server that takes them,
/// the signatures made by the software authenticator as Windows' WebAuthn
/// makes them in the window.
#[tokio::test]
async fn security_keys_on_servers_of_every_age() {
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-interop-sk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let keys = security_keys(&dir);
    let mut failures = Vec::new();
    for &(tag, server) in SK_SERVERS {
        let container = Server::start(tag, server, &dir);
        for key in &keys {
            let name = format!("{} on {server} {tag}", key.comment);
            let seen = connect(
                container.port,
                std::slice::from_ref(key),
                false,
                "echo fero$((6*7)); exit\n",
            )
            .await;
            let ok = String::from_utf8_lossy(&seen.output).contains("fero42");
            eprintln!("{name}: {}", if ok { "logged in" } else { "failed" });
            if !ok {
                failures.push(format!("{name}: {seen:?}"));
            }
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

/// Logs in and leaves the shell open; returns the worker's channels.
async fn log_in(port: u16, keys: &[KeyFile]) -> (mpsc::Sender<Message>, mpsc::Receiver<Message>) {
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
            user: "fero".into(),
            cols: 80,
            rows: 24,
            host_key_algs: vec![],
            x11: None,
            legacy: false,
            timing: Timing::default(),
            tunnel: None,
            shell: true,
        },
    ] {
        to_worker.send(msg).await.unwrap();
    }
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), from_worker.recv())
            .await
            .expect("timed out logging in")
            .expect("worker hung up");
        let reply = match msg {
            Message::HostKey { channel, .. } => Message::HostKeyVerdict {
                channel,
                accept: true,
            },
            Message::IdentitiesRequest { channel } => Message::Identities {
                channel,
                keys: keys.iter().map(|k| k.public.clone()).collect(),
            },
            Message::SignRequest {
                channel, key, data, ..
            } => {
                let k = keys.iter().find(|k| k.public == key).unwrap();
                Message::Signature {
                    channel,
                    signature: k.sign(None, &data).unwrap(),
                }
            }
            Message::Connected { .. } => return (to_worker, from_worker),
            Message::Disconnected { reason, .. } => panic!("login failed: {reason}"),
            _ => continue,
        };
        to_worker.send(reply).await.unwrap();
    }
}

/// Runs the transfer queue against the worker until `done` holds.
#[cfg(unix)]
async fn drive(
    remote: &mut ferossh_sftp::Remote,
    to_worker: &mpsc::Sender<Message>,
    from_worker: &mut mpsc::Receiver<Message>,
    done: impl Fn(&ferossh_sftp::Remote) -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        for msg in remote.take_outbox() {
            to_worker.send(msg).await.unwrap();
        }
        if done(remote) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out: {:?}",
            remote.jobs()
        );
        match tokio::time::timeout(Duration::from_millis(20), from_worker.recv()).await {
            Ok(Some(msg)) if msg.channel() == Some(2) => {
                if let Message::Disconnected { reason, .. } = &msg {
                    panic!("SFTP ended: {reason}");
                }
                remote.on_message(msg);
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("worker hung up"),
            Err(_) => remote.poll(),
        }
    }
}

/// What the edge-case folder holds on the server, made as `fero`.
#[cfg(unix)]
const EDGE_FILES: &str = r#"
set -e
mkdir -p ~/edge/sub && cd ~/edge
printf latin1 > "$(printf 'caf\351.txt')"
printf colon > 'a:b'
printf under > 'a_b'
printf device > CON
printf dot > 'trailing.'
printf pipe > 'x|y'
printf inside > sub/inner.txt
ln -s sub/inner.txt link-in
ln -s sub link-dir
ln -s /etc/passwd link-out
ln -s loop loop
ln -s missing dangling
printf readme > README
printf lower > readme
cd ~ && truncate -s 5G big.sparse && printf END >> big.sparse
"#;

/// SFTP against real sftp-servers (OpenSSH 7.6 and 10.2, and Dropbear,
/// which runs OpenSSH's): a folder with names Windows cannot hold or that
/// are not UTF-8, links inside, outside, to themselves and to nothing,
/// and a file past 4 GiB resumed near its end.
#[cfg(unix)]
#[tokio::test]
async fn sftp_edge_cases() {
    use ferossh_sftp::{Remote, State};
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-interop-sftp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let keys = user_keys(&dir);
    for (tag, server) in [
        ("18.04", "openssh"),
        ("26.04", "openssh"),
        ("26.04", "dropbear"),
    ] {
        let name = format!("{server} on ubuntu {tag}");
        let container = Server::start(tag, server, &dir);
        docker(&["exec", "-u", "fero", &container.id, "sh", "-c", EDGE_FILES])
            .unwrap_or_else(|| panic!("{name}: making the files failed"));
        let (to_worker, mut from_worker) = log_in(container.port, &keys).await;
        let mut remote = Remote::new("interop");
        remote.attach(2, 1);
        drive(&mut remote, &to_worker, &mut from_worker, |r| {
            r.is_ready() && !r.listing.loading
        })
        .await;
        let edge = b"/home/fero/edge".to_vec();
        remote.browse(&edge);
        drive(&mut remote, &to_worker, &mut from_worker, |r| {
            !r.listing.loading && r.listing.path == edge
        })
        .await;
        let entry = |remote: &Remote, n: &[u8]| {
            remote
                .listing
                .entries
                .iter()
                .find(|e| e.name == n)
                .unwrap_or_else(|| panic!("{name}: no {:?}", String::from_utf8_lossy(n)))
                .clone()
        };
        let local = dir.join(format!("{tag}-{server}"));
        std::fs::create_dir_all(&local).unwrap();
        let latin1 = entry(&remote, b"caf\xe9.txt");
        remote.download(&latin1, &local).unwrap();
        remote.browse(b"/home/fero");
        drive(&mut remote, &to_worker, &mut from_worker, |r| {
            !r.listing.loading && r.listing.path == b"/home/fero"
        })
        .await;
        let big = entry(&remote, b"big.sparse");
        assert_eq!(big.attrs.size, Some(5 * (1 << 30) + 3), "{name}");

        // Resume past 4 GiB: a sparse part file as if all but the end came.
        std::fs::File::create(local.join("big.sparse.part"))
            .unwrap()
            .set_len(5 * (1 << 30))
            .unwrap();
        remote.download(&big, &local).unwrap();
        // A whole folder of awkward names and links.
        let edge_dir = entry(&remote, b"edge");
        remote.download(&edge_dir, &local).unwrap();
        drive(&mut remote, &to_worker, &mut from_worker, |r| {
            r.jobs().iter().all(|j| j.state.finished())
        })
        .await;
        for job in remote.jobs().iter().filter(|j| j.parent.is_none()) {
            assert_eq!(
                job.state,
                State::Done,
                "{name}: {} {:?}",
                job.name,
                job.note
            );
            eprintln!("{name}: {} -> {:?}", job.name, job.note);
        }

        let big_local = std::fs::metadata(local.join("big.sparse")).unwrap();
        assert_eq!(big_local.len(), 5 * (1 << 30) + 3, "{name}");
        let mut tail = [0u8; 3];
        {
            use std::io::{Read as _, Seek, SeekFrom};
            let mut f = std::fs::File::open(local.join("big.sparse")).unwrap();
            f.seek(SeekFrom::End(-3)).unwrap();
            f.read_exact(&mut tail).unwrap();
        }
        assert_eq!(&tail, b"END", "{name}");

        let got = local.join("edge");
        let read = |n: &str| std::fs::read_to_string(got.join(n)).ok();
        let mut names: Vec<String> = std::fs::read_dir(&got)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        eprintln!("{name}: {names:?}");
        assert_eq!(
            read("caf_.txt").as_deref(),
            Some("latin1"),
            "{name}: {names:?}"
        );
        assert_eq!(read("_CON").as_deref(), Some("device"), "{name}: {names:?}");
        assert_eq!(
            read("trailing_").as_deref(),
            Some("dot"),
            "{name}: {names:?}"
        );
        assert_eq!(read("x_y").as_deref(), Some("pipe"), "{name}: {names:?}");
        assert_eq!(read("sub/inner.txt").as_deref(), Some("inside"), "{name}");
        // "a:b" and "a_b" both become a_b, and README and readme are one
        // name to Windows: neither may be lost or asked about. Which of
        // each pair gets the " (2)" depends on the server's listing order.
        for (stem, texts) in [("a_b", ["colon", "under"]), ("readme", ["readme", "lower"])] {
            let pair: Vec<&String> = names
                .iter()
                .filter(|n| n.to_lowercase().starts_with(stem))
                .collect();
            assert_eq!(pair.len(), 2, "{name}: {names:?}");
            let mut got: Vec<String> = pair.iter().filter_map(|n| read(n)).collect();
            got.sort();
            let mut want = texts.map(str::to_owned).to_vec();
            want.sort();
            assert_eq!(got, want, "{name}: {names:?}");
        }
        // Links out of the folder, to folders (no loops or second copies),
        // to themselves and to nothing are left behind.
        assert!(!got.join("link-out").exists(), "{name}: {names:?}");
        assert!(!got.join("link-dir").exists(), "{name}: {names:?}");
        assert!(
            !got.join("loop").exists() && !got.join("dangling").exists(),
            "{name}"
        );
        assert_eq!(
            read("link-in").as_deref(),
            Some("inside"),
            "{name}: {names:?}"
        );
        assert_eq!(
            std::fs::read_to_string(local.join("caf_.txt"))
                .ok()
                .as_deref(),
            Some("latin1"),
            "{name}"
        );
        let _ = std::fs::remove_dir_all(&local);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A terminal pane on a real shell: what the server sends goes into
/// FeroSSH's terminal, and its replies (cursor reports, device
/// attributes) go back, as in the window.
struct Pane {
    term: ferossh_term::Terminal,
    to_worker: mpsc::Sender<Message>,
    from_worker: mpsc::Receiver<Message>,
}

impl Pane {
    async fn open(port: u16, keys: &[KeyFile]) -> Pane {
        let (to_worker, from_worker) = log_in(port, keys).await;
        let term = ferossh_term::Terminal::new(80, 24, &ferossh_term::Settings::default());
        Pane {
            term,
            to_worker,
            from_worker,
        }
    }

    async fn send(&self, keys: &str) {
        self.to_worker
            .send(Message::Data {
                channel: 1,
                bytes: keys.as_bytes().to_vec(),
            })
            .await
            .unwrap();
    }

    async fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(cols.into(), rows.into());
        self.to_worker
            .send(Message::Resize {
                channel: 1,
                cols,
                rows,
            })
            .await
            .unwrap();
    }

    fn grid(&self) -> ferossh_render::Grid {
        let mut grid = ferossh_render::Grid::new(1, 1);
        self.term.draw_into(&mut grid, true);
        grid
    }

    /// The screen's rows as text, wide characters once.
    fn lines(&self) -> Vec<String> {
        let grid = self.grid();
        (0..grid.rows())
            .map(|y| {
                grid.row(y)
                    .unwrap()
                    .iter()
                    .filter(|c| !c.flags.contains(ferossh_render::Flags::WIDE_SPACER))
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// The character drawn in column `x` of row `y`.
    fn at(&self, x: usize, y: usize) -> char {
        self.grid().cell(x, y).unwrap().ch
    }

    /// Feeds the server's output in, answering the terminal's replies,
    /// until `done` holds or `limit` passes; whether it held.
    async fn wait(&mut self, limit: Duration, done: impl Fn(&Pane) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        while !done(self) {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, self.from_worker.recv()).await {
                Ok(Some(Message::Data { bytes, .. })) => {
                    self.term.feed(&bytes);
                    for action in self.term.take_actions() {
                        if let ferossh_term::Action::Reply(bytes) = action {
                            self.to_worker
                                .send(Message::Data { channel: 1, bytes })
                                .await
                                .unwrap();
                        }
                    }
                }
                Ok(Some(Message::Disconnected { reason, .. })) => panic!("disconnected: {reason}"),
                Ok(Some(_)) => {}
                Ok(None) => panic!("worker hung up"),
                Err(_) => return false,
            }
        }
        true
    }

    fn dump(&self) -> String {
        self.lines().join("\n")
    }

    /// Waits for `done`, then for the program to finish drawing.
    async fn until(&mut self, what: &str, done: impl Fn(&Pane) -> bool) {
        if !self.wait(Duration::from_secs(20), done).await {
            panic!("{what}: timed out; screen:\n{}", self.dump());
        }
        self.wait(Duration::from_millis(300), |_| false).await;
    }

    /// Presses Return until `done`: vttest pauses after every screen,
    /// and some screens (132 columns) are passed over unchecked.
    async fn next(&mut self, what: &str, done: impl Fn(&Pane) -> bool) {
        for _ in 0..12 {
            self.send("\r").await;
            if self.wait(Duration::from_secs(3), &done).await {
                self.wait(Duration::from_millis(300), |_| false).await;
                return;
            }
        }
        panic!("{what}: never came; screen:\n{}", self.dump());
    }

    fn has(&self, text: &str) -> bool {
        self.lines().iter().any(|l| l.contains(text))
    }

    /// The cell `text` starts at, counting a wide character as two.
    fn find(&self, text: &str) -> Option<(usize, usize)> {
        let want: Vec<char> = text.chars().collect();
        let grid = self.grid();
        (0..grid.rows()).find_map(|y| {
            let cells: Vec<(usize, char)> = grid
                .row(y)?
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.flags.contains(ferossh_render::Flags::WIDE_SPACER))
                .map(|(x, c)| (x, c.ch))
                .collect();
            cells
                .windows(want.len())
                .find(|w| w.iter().map(|(_, c)| *c).eq(want.iter().copied()))
                .map(|w| (w[0].0, y))
        })
    }

    fn flags(&self, x: usize, y: usize) -> ferossh_render::Flags {
        self.grid().cell(x, y).unwrap().flags
    }

    /// Waits for the shell prompt as the last thing on the screen.
    async fn prompt(&mut self, what: &str) {
        self.until(what, |p| {
            p.lines()
                .iter()
                .rev()
                .find(|l| !l.is_empty())
                .map(String::as_str)
                == Some("READY$")
        })
        .await;
    }
}

fn repeat(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

/// vttest's checks of cursor movement, wrapping, tabs, scroll regions,
/// origin mode, attributes, insert and delete, reports and DEC line
/// drawing, each screen compared with what a VT102 shows. The 132-column
/// screens are skipped: FeroSSH, like xterm by default, keeps the pane's
/// width.
async fn vttest(p: &mut Pane, name: &str) {
    use ferossh_render::Flags;
    let menu = |p: &Pane| p.has("Enter choice number (0 - 12)");
    let stars = repeat('*', 80);
    p.send("vttest\r").await;
    p.until(name, menu).await;

    p.send("1\r").await;
    p.until(name, |p| {
        p.lines()[23] == repeat('*', 80) && p.has("Push <RETURN>")
    })
    .await;
    let l = p.lines();
    let plus = format!("*{}*", repeat('+', 78));
    assert_eq!(
        (&l[0], &l[1], &l[22], &l[23]),
        (&stars, &plus, &plus, &stars),
        "{name}: border\n{}",
        p.dump()
    );
    assert!(
        l[2..22]
            .iter()
            .all(|r| r.starts_with("*+") && r.ends_with("+*")),
        "{name}\n{}",
        p.dump()
    );
    assert_eq!(
        l[8],
        format!("*+{0}{1}{0}+*", repeat(' ', 8), repeat('E', 60)),
        "{name}"
    );
    p.next(name, |p| {
        p.lines()[0].starts_with("Test of autowrap") && p.has("Push <RETURN>")
    })
    .await;
    let l = p.lines();
    for (y, c) in (2..20).zip('I'..='Z') {
        let want = format!("{c}{}{}", repeat(' ', 78), c.to_ascii_lowercase());
        assert_eq!(l[y], want, "{name}: autowrap\n{}", p.dump());
    }
    p.next(name, |p| {
        p.has("inside ESC sequences") && p.has("Push <RETURN>")
    })
    .await;
    assert!(
        p.lines()[3..7].iter().all(|r| r == "A B C D E F G H I"),
        "{name}\n{}",
        p.dump()
    );
    p.next(name, |p| p.has("leading zeros") && p.has("Push <RETURN>"))
        .await;
    assert!(p.has("This is a correct sentence"), "{name}\n{}", p.dump());
    p.next(name, menu).await;

    p.send("2\r").await;
    p.until(name, |p| p.has("WRAP AROUND") && p.has("Push <RETURN>"))
        .await;
    assert!(
        p.lines()[0..3].iter().all(|r| *r == stars),
        "{name}: wrap\n{}",
        p.dump()
    );
    p.next(name, |p| p.has("TAB setting") && p.has("Push <RETURN>"))
        .await;
    let l = p.lines();
    assert_eq!(l[0], l[1], "{name}: tabs");
    assert_eq!(l[0].find('*'), Some(6), "{name}: tabs");
    assert_eq!(l[0].matches('*').count(), 13, "{name}: tabs");
    p.next(name, |p| {
        p.has("Origin mode test") && p.has("one above the bottom")
    })
    .await;
    let l = p.lines();
    assert!(
        l[23].starts_with("Origin mode test."),
        "{name}: origin\n{}",
        p.dump()
    );
    assert!(
        l[22].starts_with("This line should be the one above the bottom"),
        "{name}\n{}",
        p.dump()
    );
    p.next(name, |p| {
        p.lines()[0].starts_with("This line should be at the top")
    })
    .await;
    assert!(
        p.lines()[23].starts_with("Origin mode test."),
        "{name}\n{}",
        p.dump()
    );
    p.next(name, |p| {
        p.has("Graphic rendition") && p.has("Dark background")
    })
    .await;
    // Bold, underlined, inverse.
    for (word, want) in [
        ("vanilla", (false, false, false)),
        ("bold underline blink negative", (true, true, true)),
        ("bold negative", (true, false, true)),
        ("underline negative", (false, true, true)),
        ("bold", (true, false, false)),
    ] {
        let (x, y) = p
            .find(word)
            .unwrap_or_else(|| panic!("{name}: {word}\n{}", p.dump()));
        let f = p.flags(x, y);
        let got = (
            f.contains(Flags::BOLD),
            f.contains(Flags::UNDERLINE),
            f.contains(Flags::INVERSE),
        );
        assert_eq!(got, want, "{name}: {word}");
    }
    p.next(name, menu).await;

    p.send("8\r").await;
    p.until(name, |p| {
        p.has("Screen accordion") && p.lines()[23] == repeat('X', 80)
    })
    .await;
    let l = p.lines();
    for (y, c) in (0..24).zip('A'..='X').filter(|(y, _)| *y != 3) {
        assert_eq!(
            l[y],
            repeat(c, 80),
            "{name}: insert/delete line\n{}",
            p.dump()
        );
    }
    p.next(name, |p| p.has("Top line: A's")).await;
    let l = p.lines();
    assert_eq!(
        (&l[0], &l[23]),
        (&repeat('A', 80), &repeat('X', 80)),
        "{name}\n{}",
        p.dump()
    );
    assert!(
        l[2..23].iter().all(String::is_empty),
        "{name}\n{}",
        p.dump()
    );
    p.next(name, |p| p.has("'Insert Mode'")).await;
    assert_eq!(
        p.lines()[0],
        format!("A{}B", repeat('*', 78)),
        "{name}: insert mode"
    );
    p.next(name, |p| p.has("'Delete Character'")).await;
    assert_eq!(p.lines()[0], "AB", "{name}: delete character");
    p.next(name, |p| {
        p.has("staggered") && p.lines()[23].starts_with('X')
    })
    .await;
    let l = p.lines();
    for (y, c) in (0..24).zip('A'..='X').filter(|(y, _)| *y > 4) {
        assert_eq!(
            l[y],
            repeat(c, 79 - y),
            "{name}: delete character\n{}",
            p.dump()
        );
    }
    p.next(name, |p| {
        p.has("staggered") && p.lines()[23] == repeat('X', 16)
    })
    .await;
    let l = p.lines();
    for (y, c) in (0..24).zip('A'..='X').filter(|(y, _)| *y > 4) {
        assert_eq!(
            l[y],
            repeat(c, 39 - y),
            "{name}: delete character\n{}",
            p.dump()
        );
    }
    p.next(name, |p| p.has("'Insert Character'")).await;
    let row = "  A B C D E F G H I J K L M N O P Q R S T U V W X Y Z";
    assert_eq!(
        p.lines().iter().filter(|l| *l == row).count(),
        2,
        "{name}: insert character\n{}",
        p.dump()
    );
    p.next(name, menu).await;

    p.send("6\r").await;
    let reports = |p: &Pane| p.has("Enter choice number (0 - 7)");
    p.until(name, reports).await;
    p.send("4\r").await;
    p.until(name, |p| p.has("Report is:") && p.has("Push <RETURN>"))
        .await;
    assert!(
        p.has("-- means VT102"),
        "{name}: device attributes\n{}",
        p.dump()
    );
    p.next(name, reports).await;
    p.send("3\r").await;
    p.until(name, |p| {
        p.has("report cursor position") && p.has("Push <RETURN>")
    })
    .await;
    assert!(
        p.has("means \"TERMINAL OK\""),
        "{name}: status\n{}",
        p.dump()
    );
    assert!(
        p.has("<27> [ 5 ; 1 R  -- OK"),
        "{name}: cursor report\n{}",
        p.dump()
    );
    p.next(name, reports).await;
    p.send("5\r").await;
    p.until(name, |p| {
        p.has("Secondary Device Attributes") && p.has("Push <RETURN>")
    })
    .await;
    assert!(
        p.has("<27> [ > "),
        "{name}: secondary attributes\n{}",
        p.dump()
    );
    p.next(name, reports).await;
    p.send("0\r").await;
    p.until(name, menu).await;

    // vttest before 2.7 (2017) shows the character sets straight away,
    // later ones from a menu.
    let sets = |p: &Pane| p.has("These are the installed character sets");
    let charsets_menu = |p: &Pane| p.has("Enter choice number") && !menu(p);
    p.send("3\r").await;
    p.until(name, |p| sets(p) || charsets_menu(p)).await;
    if !sets(p) {
        p.send("8\r").await;
        p.until(name, sets).await;
    }
    let drawing = "◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
    assert!(
        p.lines().iter().any(|l| l.matches(drawing).count() == 2),
        "{name}: line drawing in G0 and G1\n{}",
        p.dump()
    );
    p.next(name, |p| menu(p) || charsets_menu(p)).await;
    if !menu(p) {
        p.send("0\r").await;
        p.until(name, menu).await;
    }
    p.send("0\r").await;
    p.prompt(name).await;
}

/// Real programs on real servers through the worker into FeroSSH's
/// terminal, as in a tab: vttest, wide and combining characters, less
/// and vi on the alternate screen, and a resize the server's pty sees.
#[tokio::test]
async fn real_programs_draw_correctly() {
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-interop-vt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let keys = user_keys(&dir);
    for (tag, server) in [
        ("18.04", "openssh"),
        ("26.04", "openssh"),
        ("26.04", "dropbear"),
    ] {
        let label = format!("{server} on {tag}");
        let name = label.as_str();
        let running = Server::start(tag, server, &dir);
        let mut p = Pane::open(running.port, &keys).await;
        p.send("export LANG=C.UTF-8 PS1='READY$ '; clear\r").await;
        p.prompt(name).await;

        // vttest checks the terminal, not the server: one old and one new
        // build of it is enough.
        if server == "openssh" {
            vttest(&mut p, name).await;
        }

        // Typed as octal so the line editor sees only ASCII.
        p.send("clear; printf '\\346\\227\\245\\346\\234\\254|\\n\\360\\237\\231\\202|\\ne\\314\\201|\\n'\r")
            .await;
        p.prompt(name).await;
        let (x, y) = p
            .find("日本|")
            .unwrap_or_else(|| panic!("{name}\n{}", p.dump()));
        assert_eq!((x, p.at(4, y)), (0, '|'), "{name}: wide characters");
        assert_eq!(
            p.at(2, y + 1),
            '|',
            "{name}: emoji is two cells\n{}",
            p.dump()
        );
        assert_eq!(
            (p.at(0, y + 2), p.at(1, y + 2)),
            ('e', '|'),
            "{name}: combining accent"
        );

        p.send("seq 1 200 | less\r").await;
        p.until(name, |p| {
            let l = p.lines();
            l[0] == "1" && l[22] == "23" && l[23].starts_with(':')
        })
        .await;
        p.send(" ").await;
        p.until(name, |p| p.lines()[0] == "24").await;
        p.send("G").await;
        p.until(name, |p| p.lines()[22] == "200").await;
        p.send("q").await;
        p.prompt(name).await;
        // Back from the alternate screen: what was there before is again.
        assert!(
            p.find("日本|").is_some() && !p.lines().iter().any(|l| l == "200"),
            "{name}\n{}",
            p.dump()
        );

        p.send("clear; printf 'alpha\\nbeta\\n' > f.txt; vim.tiny -u NONE f.txt\r")
            .await;
        p.until(name, |p| {
            let l = p.lines();
            l[0] == "alpha" && l[1] == "beta" && l[2] == "~" && l[23].contains("f.txt")
        })
        .await;
        p.send("Gowide 日本\x1b:wq\r").await;
        p.prompt(name).await;
        p.send("clear; cat f.txt\r").await;
        p.prompt(name).await;
        assert!(p.has("wide 日本"), "{name}: vi wrote\n{}", p.dump());

        p.resize(100, 30).await;
        p.send("clear; stty size\r").await;
        p.prompt(name).await;
        assert!(p.has("30 100"), "{name}: resize\n{}", p.dump());
        p.send("printf '%099d|\\n' 0\r").await;
        p.prompt(name).await;
        assert_eq!(
            p.find("0|").map(|(x, _)| x),
            Some(98),
            "{name}: 100 columns\n{}",
            p.dump()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// (jump host image, jump host server, target image, target server,
/// expected cipher on the target). The target's connection runs inside
/// the jump host's `direct-tcpip` channel, so it cannot be tried again:
/// it starts with AES-GCM, which an OpenSSH without strict key exchange
/// takes the first time. Dropbear has no AES-GCM but has strict key
/// exchange, so ChaCha20 is safe there.
const JUMP_CASES: &[(&str, &str, &str, &str, &str)] = &[
    ("24.04", "dropbear", "18.04", "openssh", GCM),
    ("18.04", "openssh", "24.04", "dropbear", CHACHA),
    ("24.04", "openssh", "18.04", "openssh", GCM),
];

/// The container's address on docker's network, where the jump host
/// reaches it.
fn address(server: &Server) -> String {
    docker(&[
        "inspect",
        "-f",
        "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
        &server.id,
    ])
    .filter(|a| !a.is_empty())
    .expect("container address")
}

/// Plays the UI for a login to `target` (an address on docker's
/// network) through the jump host on local port `jump`: two workers, the
/// jump host's without a shell, the target's SSH connection relayed
/// between them over the jump host's `direct-tcpip` stream.
async fn through(jump: u16, target: &str, keys: &[KeyFile], command: &str) -> Outcome {
    const TUNNEL: u32 = 100;
    let (events_tx, mut events) = mpsc::channel::<(usize, Message)>(256);
    let mut to = Vec::new();
    for i in 0..2 {
        let (to_worker, worker_in) = mpsc::channel(64);
        let (worker_out, mut from_worker) = mpsc::channel(64);
        tokio::spawn(ferossh_worker::serve(worker_in, worker_out));
        let events_tx = events_tx.clone();
        tokio::spawn(async move {
            while let Some(m) = from_worker.recv().await {
                if events_tx.send((i, m)).await.is_err() {
                    return;
                }
            }
        });
        to_worker
            .send(Message::Hello {
                version: PROTOCOL_VERSION,
            })
            .await
            .unwrap();
        to.push(to_worker);
    }
    let connect = |i: usize| Message::Connect {
        channel: 1,
        host: if i == 0 {
            "127.0.0.1".into()
        } else {
            target.into()
        },
        port: if i == 0 { jump } else { 22 },
        user: "fero".into(),
        cols: 80,
        rows: 24,
        host_key_algs: vec![],
        x11: None,
        legacy: false,
        timing: Timing::default(),
        tunnel: (i == 1).then_some(TUNNEL),
        shell: i == 1,
    };
    to[0].send(connect(0)).await.unwrap();
    let mut seen = Outcome::default();
    let deadline = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(deadline);
    loop {
        let (i, msg) = tokio::select! {
            m = events.recv() => m.expect("workers hung up"),
            () = &mut deadline => panic!("timed out; so far {seen:?}"),
        };
        // The tunnel's bytes go straight to the other worker.
        if msg.channel() == Some(TUNNEL) {
            match msg {
                Message::Data { .. } | Message::Window { .. } | Message::Eof { .. } => {
                    to[1 - i].send(msg).await.unwrap();
                }
                Message::Disconnected { channel, .. } => {
                    let _ = to[1 - i].send(Message::Close { channel }).await;
                }
                _ => {}
            }
            continue;
        }
        let reply = match msg {
            Message::HostKey { channel, .. } => {
                seen.host_key_prompts += 1;
                Message::HostKeyVerdict {
                    channel,
                    accept: true,
                }
            }
            Message::IdentitiesRequest { channel } => Message::Identities {
                channel,
                keys: keys.iter().map(|k| k.public.clone()).collect(),
            },
            Message::SignRequest {
                channel,
                key: k,
                data,
            } => {
                let key = keys.iter().find(|key| key.public == k).expect("ours");
                Message::Signature {
                    channel,
                    signature: key.sign(None, &data).unwrap(),
                }
            }
            Message::Connected { .. } if i == 0 => {
                to[0]
                    .send(Message::ForwardOpen {
                        channel: TUNNEL,
                        connection: 1,
                        target: Some((target.into(), 22)),
                        origin_host: "127.0.0.1".into(),
                        origin_port: 0,
                        keep: true,
                    })
                    .await
                    .unwrap();
                to[1].send(connect(1)).await.unwrap();
                continue;
            }
            Message::Connected {
                kex,
                cipher,
                host_key,
                ..
            } => {
                seen.connected = Some((kex, cipher, host_key));
                Message::Data {
                    channel: 1,
                    bytes: command.as_bytes().to_vec(),
                }
            }
            Message::Data { bytes, .. } if i == 1 => {
                seen.output.extend_from_slice(&bytes);
                continue;
            }
            Message::Prompt { .. } => panic!("asked for a password; so far {seen:?}"),
            Message::Disconnected { reason, .. } => {
                assert_eq!(i, 1, "the jump host ended first: {reason}; so far {seen:?}");
                seen.disconnected = reason;
                return seen;
            }
            _ => continue,
        };
        to[i].send(reply).await.unwrap();
    }
}

/// `ProxyJump` through OpenSSH and Dropbear jump hosts of several ages,
/// to targets of other ages: each hop checks its own host key, and the
/// target, which cannot be tried again, logs in the first time.
#[tokio::test]
async fn jump_hosts_of_every_age() {
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-interop-jump-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let keys = user_keys(&dir);
    let mut failures = Vec::new();
    for &(jump_tag, jump_server, tag, server, cipher) in JUMP_CASES {
        let name = format!("{server} on ubuntu {tag} through {jump_server} on ubuntu {jump_tag}");
        let jump = Server::start(jump_tag, jump_server, &dir);
        let target = Server::start(tag, server, &dir);
        let seen = through(
            jump.port,
            &address(&target),
            &keys,
            "echo fero$((6*7)); exit\n",
        )
        .await;
        let output = String::from_utf8_lossy(&seen.output);
        eprintln!("{name}: {:?}", seen.connected);
        let ok = matches!(&seen.connected, Some((_, c, _)) if c == cipher)
            && output.contains("fero42")
            && seen.host_key_prompts == 2;
        if !ok {
            failures.push(format!("{name}: expected {cipher}, got {seen:?}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
