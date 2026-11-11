//! Kerberos logins (`gssapi-with-mic`) against a real KDC and OpenSSH.
//!
//! The test starts MIT's KDC for a throwaway realm and an `sshd` that
//! takes Kerberos logins, both on free local ports, and drives the worker
//! the way the window does: the window's side of each GSS-API step runs
//! through the auth crate's [`Contexts`], here with MIT's library standing
//! in for Windows' SSPI, after checking what the MIC covers. Without the
//! KDC programs the test passes without running; the Linux CI job sets
//! `FEROSSH_REQUIRE_SSHD` and has them. Windows has no MIT library: its
//! SSPI side is the window's, type-checked there.

#![cfg(unix)]
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use common::{Kill, SSHD, whoami};
use ferossh_auth::gss::{self, Contexts, GssOutput, GssStepJob, mit::Mit};
use ferossh_ipc::{GssReply, Message, PROTOCOL_VERSION};
use tokio::sync::mpsc;

const REALM: &str = "FERO.TEST";

fn tool(name: &str) -> Option<PathBuf> {
    ["/usr/sbin", "/usr/bin"]
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.exists())
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for(port: u16) {
    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("nothing listens on {port}");
}

fn quiet(cmd: &mut Command) {
    let out = cmd.stdin(Stdio::null()).output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A realm with a KDC, the host's service key and the user's key.
struct Realm {
    _kdc: Kill,
    dir: PathBuf,
}

impl Realm {
    fn start(dir: &Path, user: &str) -> Realm {
        let kdc_port = free_port();
        std::fs::write(
            dir.join("krb5.conf"),
            format!(
                "[libdefaults]\n  default_realm = {REALM}\n  dns_lookup_kdc = false\n  \
                 dns_lookup_realm = false\n  dns_canonicalize_hostname = false\n  rdns = false\n  \
                 udp_preference_limit = 1\n  forwardable = true\n\
                 [realms]\n  {REALM} = {{\n    kdc = 127.0.0.1:{kdc_port}\n  }}\n"
            ),
        )
        .unwrap();
        let d = dir.display();
        std::fs::write(
            dir.join("kdc.conf"),
            format!(
                "[kdcdefaults]\n  kdc_tcp_listen = 127.0.0.1:{kdc_port}\n  kdc_listen = 127.0.0.1:{}\n\
                 [realms]\n  {REALM} = {{\n    database_name = {d}/principal\n    \
                 key_stash_file = {d}/stash\n    acl_file = {d}/kadm5.acl\n  }}\n\
                 [logging]\n  kdc = FILE:{d}/kdc.log\n",
                free_port()
            ),
        )
        .unwrap();
        let env = |c: &mut Command| {
            c.env("KRB5_CONFIG", dir.join("krb5.conf"))
                .env("KRB5_KDC_PROFILE", dir.join("kdc.conf"));
        };
        let mut c = Command::new(tool("kdb5_util").unwrap());
        c.args(["create", "-s", "-P", "master-password", "-r", REALM]);
        env(&mut c);
        quiet(&mut c);
        let admin = |q: &str| {
            let mut c = Command::new(tool("kadmin.local").unwrap());
            c.args(["-r", REALM, "-q", q]);
            env(&mut c);
            quiet(&mut c);
        };
        admin("addprinc -randkey host/127.0.0.1");
        admin(&format!("addprinc -randkey {user}"));
        admin(&format!("ktadd -k {d}/host.keytab host/127.0.0.1"));
        admin(&format!("ktadd -k {d}/user.keytab {user}"));
        let mut c = Command::new(tool("krb5kdc").unwrap());
        c.args(["-n", "-P"]).arg(dir.join("kdc.pid"));
        env(&mut c);
        let kdc = Kill(
            c.stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait_for(kdc_port);
        Realm {
            _kdc: kdc,
            dir: dir.to_path_buf(),
        }
    }

    /// sshd taking Kerberos logins, and keys when `authorized` is given.
    fn sshd(&self, authorized: Option<&str>) -> (Kill, u16) {
        let dir = &self.dir;
        let host = dir.join("host-key");
        if !host.exists() {
            quiet(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(&host),
            );
        }
        let port = free_port();
        let keys = dir.join("authorized_keys");
        std::fs::write(&keys, authorized.unwrap_or_default()).unwrap();
        let config = format!(
            "ListenAddress 127.0.0.1\nPort {port}\nHostKey {}\nPidFile none\nStrictModes no\n\
             UsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             PubkeyAuthentication {}\nAuthorizedKeysFile {}\nGSSAPIAuthentication yes\n\
             GSSAPIStrictAcceptorCheck no\n",
            host.display(),
            if authorized.is_some() { "yes" } else { "no" },
            keys.display(),
        );
        let cfg = dir.join(format!("sshd-{port}"));
        std::fs::write(&cfg, config).unwrap();
        let _ = std::fs::create_dir_all("/run/sshd");
        let child = Command::new(SSHD)
            .args(["-D", "-e", "-f"])
            .arg(&cfg)
            .env("KRB5_CONFIG", dir.join("krb5.conf"))
            .env("KRB5_KTNAME", dir.join("host.keytab"))
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for(port);
        (Kill(child), port)
    }
}

fn available() -> bool {
    let found = Path::new(SSHD).exists()
        && ["kdb5_util", "kadmin.local", "krb5kdc"]
            .iter()
            .all(|t| tool(t).is_some())
        && Mit::load().is_ok();
    if !found {
        assert!(
            std::env::var_os("FEROSSH_REQUIRE_SSHD").is_none(),
            "FEROSSH_REQUIRE_SSHD is set but sshd, the MIT KDC (krb5-kdc, \
             krb5-admin-server) or libgssapi_krb5 is missing"
        );
        eprintln!("sshd or the MIT KDC not found; skipping");
    }
    found
}

/// How the window's side answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ui {
    /// Through MIT Kerberos, for the service on `host`.
    Tickets { host: &'static str, delegate: bool },
    /// As a PC without tickets does.
    NoTickets,
}

#[derive(Debug, Default)]
struct Seen {
    gss_steps: u32,
    key_requests: u32,
    connected: bool,
    output: String,
    reason: String,
}

async fn log_in(
    port: u16,
    user: &str,
    gssapi: bool,
    ui: Ui,
    key: Option<&ferossh_auth::keyfile::KeyFile>,
    command: &str,
) -> Seen {
    let mit = Mit::load().unwrap();
    let mut contexts = Contexts::default();
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
            options: ferossh_ipc::ShellOptions {
                command: Some(command.into()),
                pty: false,
                ..Default::default()
            },
            family: Default::default(),
            gssapi,
            proxy: None,
        },
    ] {
        to_worker.send(msg).await.unwrap();
    }
    let mut seen = Seen::default();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), from_worker.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out; so far {seen:?}"))
            .expect("worker hung up");
        let reply = match msg {
            Message::HostKey { channel, .. } => Message::HostKeyVerdict {
                channel,
                accept: true,
            },
            Message::GssapiStep {
                channel,
                first,
                token,
                mic_data,
            } => {
                seen.gss_steps += 1;
                gss::check_mic_data(&mic_data, user).expect("a gssapi-with-mic request");
                let reply = match ui {
                    Ui::NoTickets => GssReply::Failed,
                    Ui::Tickets { host, delegate } => {
                        let job = GssStepJob {
                            login: channel,
                            host: host.into(),
                            delegate,
                            first,
                            token,
                            mic_data,
                        };
                        match contexts.step(&mit, job) {
                            Ok(GssOutput::Continue(t)) => GssReply::Continue(t),
                            Ok(GssOutput::Complete { token, mic }) => {
                                GssReply::Complete { token, mic }
                            }
                            Err(why) => {
                                eprintln!("no Kerberos login: {why}");
                                GssReply::Failed
                            }
                        }
                    }
                };
                Message::GssapiReply { channel, reply }
            }
            Message::IdentitiesRequest { channel } => {
                seen.key_requests += 1;
                Message::Identities {
                    channel,
                    keys: key.iter().map(|k| k.public.clone()).collect(),
                }
            }
            Message::SignRequest { channel, data, .. } => Message::Signature {
                channel,
                signature: key.unwrap().sign(None, &data).unwrap(),
            },
            Message::Prompt { .. } => panic!("asked for a password; so far {seen:?}"),
            Message::Connected { .. } => {
                seen.connected = true;
                continue;
            }
            Message::Data { bytes, .. } => {
                seen.output.push_str(&String::from_utf8_lossy(&bytes));
                continue;
            }
            Message::Disconnected { reason, .. } => {
                seen.reason = reason;
                return seen;
            }
            _ => continue,
        };
        to_worker.send(reply).await.unwrap();
    }
}

#[test]
fn kerberos_logins() {
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-krb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let user = whoami();
    let realm = Realm::start(&dir, &user);
    // MIT finds its tickets through these. Set before any thread starts:
    // this is the test binary's only test.
    // SAFETY: no other thread is running yet.
    unsafe {
        std::env::set_var("KRB5_CONFIG", dir.join("krb5.conf"));
        std::env::set_var("KRB5_CLIENT_KTNAME", dir.join("user.keytab"));
        std::env::set_var("KRB5CCNAME", "MEMORY:ferossh-test");
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (_sshd, port) = realm.sshd(None);

        // Logged in with the user's ticket alone: no keys, no password.
        let ok = Ui::Tickets {
            host: "127.0.0.1",
            delegate: false,
        };
        let seen = log_in(port, &user, true, ok, None, "echo FERO-KRB-$((6*7))").await;
        assert!(seen.connected, "{seen:?}");
        assert!(seen.output.contains("FERO-KRB-42"), "{seen:?}");
        assert_eq!(seen.key_requests, 0, "{seen:?}");
        assert!((1..=3).contains(&seen.gss_steps), "{seen:?}");

        // Kerberos off: the worker never asks for a step, and with no
        // other method the server refuses.
        let seen = log_in(port, &user, false, ok, None, "true").await;
        assert_eq!(seen.gss_steps, 0, "{seen:?}");
        assert!(!seen.connected, "{seen:?}");
        assert!(seen.reason.contains("Permission denied"), "{seen:?}");

        // A ticket for another service: the KDC knows no such principal.
        let other = Ui::Tickets {
            host: "elsewhere.test",
            delegate: false,
        };
        let seen = log_in(port, &user, true, other, None, "true").await;
        assert!(!seen.connected, "{seen:?}");
        assert!(seen.reason.contains("gssapi-with-mic"), "{seen:?}");

        // Delegation hands the server a ticket of the user's own.
        let deleg = Ui::Tickets {
            host: "127.0.0.1",
            delegate: true,
        };
        let seen = log_in(
            port,
            &user,
            true,
            deleg,
            None,
            "echo CC=${KRB5CCNAME:-none}",
        )
        .await;
        assert!(seen.connected, "{seen:?}");
        assert!(seen.output.contains("CC="), "{seen:?}");
        assert!(!seen.output.contains("CC=none"), "{seen:?}");
        let seen = log_in(port, &user, true, ok, None, "echo CC=${KRB5CCNAME:-none}").await;
        assert!(seen.output.contains("CC=none"), "{seen:?}");

        // No tickets on this PC: the worker drops Kerberos mid-exchange and
        // logs in with a key.
        let key_path = dir.join("user-key");
        quiet(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key_path),
        );
        let key = ferossh_auth::keyfile::KeyFile::load(&key_path).unwrap();
        let authorized = std::fs::read_to_string(key_path.with_extension("pub")).unwrap();
        let (_sshd2, port2) = realm.sshd(Some(&authorized));
        let seen = log_in(port2, &user, true, Ui::NoTickets, Some(&key), "echo KEY-OK").await;
        assert!(seen.connected, "{seen:?}");
        assert!(seen.output.contains("KEY-OK"), "{seen:?}");
        assert_eq!(seen.gss_steps, 1, "{seen:?}");
        assert_eq!(seen.key_requests, 1, "{seen:?}");
        // So does a ticket for the wrong service.
        let seen = log_in(port2, &user, true, other, Some(&key), "echo KEY-OK").await;
        assert!(seen.output.contains("KEY-OK"), "{seen:?}");
    });
    drop(realm);
    let _ = std::fs::remove_dir_all(&dir);
}
