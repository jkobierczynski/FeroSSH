//! The window's SSH path end to end, minus the window: [`SshSession`]
//! talks to a real worker over OS pipes ([`Link`] and
//! `ferossh_worker::run_blocking`, as on Windows), signs through the real
//! broker with a passphrase-protected key file, and logs in to a real
//! `sshd`. Skipped where `sshd` or `ssh-keygen` is missing. Jump hosts
//! log in through a worker each, with the stream between them relayed by
//! the links as the window does.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ferossh_auth::Broker;

use ferossh_ipc::ChannelId;

use crate::link::{Link, LinkEvent};
use crate::proxycmd::ProxyRun;
use crate::ssh::{Effect, Env, SshSession, State, Target};

const SSHD: &str = "/usr/sbin/sshd";

pub(crate) struct Server {
    pub(crate) dir: PathBuf,
    child: Child,
    pub(crate) port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn keygen(path: &Path, passphrase: &str) {
    let ok = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", passphrase, "-C", "e2e", "-f"])
        .arg(path)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok);
}

/// Starts a throwaway sshd whose config ends with `extra`; the user key
/// `~/.ssh/id_ed25519` under its folder has the passphrase "secret".
pub(crate) fn start_sshd(name: &str, extra: &str) -> Option<Server> {
    if !Path::new(SSHD).exists()
        || Command::new("ssh-keygen")
            .arg("-?")
            .stderr(Stdio::null())
            .status()
            .is_err()
    {
        // CI sets this so a missing sshd fails instead of passing quietly.
        assert!(
            std::env::var_os("FEROSSH_REQUIRE_SSHD").is_none(),
            "FEROSSH_REQUIRE_SSHD is set but sshd or ssh-keygen is missing"
        );
        eprintln!("sshd or ssh-keygen not found; skipping");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ferossh-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".ssh")).unwrap();
    let host = dir.join("host_ed25519");
    keygen(&host, "");
    let user = dir.join(".ssh").join("id_ed25519");
    keygen(&user, "secret");
    std::fs::copy(user.with_extension("pub"), dir.join("authorized_keys")).unwrap();
    let _ = std::fs::create_dir_all("/run/sshd");
    // A free port can be taken again before sshd binds it (tests run in
    // parallel), and a connect can reach another test's listener, so wait
    // for sshd's own "listening" line and try a fresh port if it exits.
    for _ in 0..10 {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = format!(
            "ListenAddress 127.0.0.1\nPort {port}\nHostKey {}\nAuthorizedKeysFile {}\nPidFile none\n\
             StrictModes no\nUsePAM no\nPermitRootLogin yes\nPasswordAuthentication no\n\
             KbdInteractiveAuthentication no\n{extra}\n",
            host.display(),
            dir.join("authorized_keys").display()
        );
        std::fs::write(dir.join("sshd_config"), config).unwrap();
        let mut child = Command::new(SSHD)
            .args(["-D", "-e", "-f"])
            .arg(dir.join("sshd_config"))
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Keeps reading after the line, so sshd never blocks on a full
            // pipe; ends when sshd exits.
            for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                if line.contains("Server listening on") {
                    let _ = tx.send(());
                }
            }
        });
        if rx.recv_timeout(Duration::from_secs(10)).is_ok() {
            return Some(Server { dir, child, port });
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("sshd did not start");
}

pub(crate) fn local_user() -> String {
    let out = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// What one connection showed.
#[derive(Default)]
struct Run {
    shown: String,
    output: String,
    prompts: Vec<String>,
}

/// A worker of its own, over OS pipes.
fn worker() -> Link {
    let (ui_read, worker_write) = std::io::pipe().unwrap();
    let (worker_read, ui_write) = std::io::pipe().unwrap();
    std::thread::spawn(move || ferossh_worker::run_blocking(worker_read, worker_write));
    Link::start(ui_read, ui_write, || {}, |_| {}).unwrap()
}

/// Connects to the alias "testhost" and runs `command` once logged in,
/// answering prompts from `answers` by their text.
fn connect(env: &Env, command: &str, answers: &[(&str, &str)]) -> (Run, SshSession) {
    connect_to(env, "testhost", command, answers)
}

/// Connects to `alias` as the window does, with a worker per login (the
/// jump hosts' too), and runs `command` once logged in.
fn connect_to(
    env: &Env,
    alias: &str,
    command: &str,
    answers: &[(&str, &str)],
) -> (Run, SshSession) {
    let broker = Broker::spawn(|| {}).unwrap();
    let target = Target::resolve_route(alias, env).unwrap();
    let mut last = 1;
    let mut session = SshSession::new(1, target, env.known_hosts()).with_route(&mut || {
        last += 1;
        last
    });
    let mut links: HashMap<ChannelId, Link> = HashMap::new();
    for c in std::iter::once(session.connection).chain(session.hop_connections()) {
        links.insert(c, worker());
    }
    let mut proxies: Vec<ProxyRun> = Vec::new();
    let mut run = Run::default();
    let mut effects = session.start(80, 24);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        for effect in std::mem::take(&mut effects) {
            match effect {
                Effect::Send(m) => {
                    let c = session
                        .worker_for(m.channel())
                        .expect("a channel of the session");
                    links[&c].send(m);
                }
                Effect::Tunnel { from, to, channel } => {
                    links[&from].relay_with(&links[&to], channel)
                }
                Effect::ProxyCommand {
                    connection,
                    tunnel,
                    command,
                } => {
                    let link = &links[&connection];
                    let (sink, input) = crate::proxycmd::input();
                    link.relays().insert(tunnel, sink);
                    let proxy = ProxyRun::spawn(
                        &command.program,
                        &command.args,
                        tunnel,
                        input,
                        link.sender(),
                    )
                    .unwrap();
                    proxies.push(proxy);
                }
                Effect::Show(s) => run.shown.push_str(&s),
                Effect::Output(b) => run.output.push_str(&String::from_utf8_lossy(&b)),
                Effect::Broker(job) => broker.submit(job),
                Effect::Connected => effects.extend(session.input(command.as_bytes())),
                Effect::Ended => return (run, session),
                Effect::Reconnect => {}
                // No password source in these tests.
                Effect::FetchPassword | Effect::StorePassword { .. } => {}
            }
        }
        if session.prompt_active() {
            let last = run
                .shown
                .rsplit("\r\n")
                .next()
                .unwrap_or_default()
                .to_owned();
            run.prompts.push(last.clone());
            let answer = answers
                .iter()
                .find(|(q, _)| last.contains(q))
                .map_or_else(|| panic!("unexpected prompt {last:?}"), |(_, a)| *a);
            effects.extend(session.prompt_input(answer.as_bytes()));
            effects.extend(session.prompt_input(b"\r"));
            continue;
        }
        if let Some(reply) = broker.try_recv() {
            effects.extend(session.on_broker(reply));
            continue;
        }
        let event = links
            .iter()
            .find_map(|(&c, l)| l.try_recv().map(|e| (c, e)));
        match event {
            Some((c, LinkEvent::Message(m))) => {
                // As the window does: only from the worker it belongs to.
                if session.worker_for(m.channel()) == Some(c) {
                    effects.extend(session.on_worker(m));
                }
            }
            Some((_, LinkEvent::Closed(why))) => panic!("worker closed: {why}"),
            None => {
                assert!(Instant::now() < deadline, "timed out:\n{}", run.shown);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn trusts_a_new_host_signs_with_a_key_file_and_runs_a_shell() {
    let Some(server) = start_sshd("login", "") else {
        return;
    };
    let user = local_user();
    let env = Env {
        home: server.dir.clone(),
        local_user: user.clone(),
        config: format!(
            "Host testhost\n  HostName 127.0.0.1\n  Port {}\n  User {user}\n  IdentityFile ~/.ssh/id_ed25519\n",
            server.port
        ),
    };

    let (run, session) = connect(
        &env,
        "echo ok$((1+1)); exit 5\n",
        &[("to trust this key", "yes"), ("Passphrase", "secret")],
    );
    assert!(run.output.contains("ok2"), "{}\n{}", run.shown, run.output);
    assert_eq!(session.exit_code, Some(5));
    assert_eq!(session.state(), State::Ended);
    assert!(
        run.shown
            .contains("Signing in to 127.0.0.1 with ED25519 key SHA256:")
    );
    assert_eq!(run.prompts.len(), 2, "{:?}", run.prompts);

    // Known now: only the passphrase is asked.
    let (run, _) = connect(&env, "exit\n", &[("Passphrase", "secret")]);
    assert_eq!(run.prompts.len(), 1, "{:?}", run.prompts);
    assert!(run.shown.contains("Connected ("));
}

/// Two servers whose users share the key under the first one's folder.
fn two_servers(name: &str) -> Option<(Server, Server)> {
    let jump = start_sshd(&format!("{name}-jump"), "")?;
    let target = start_sshd(&format!("{name}-target"), "")?;
    std::fs::copy(
        jump.dir.join(".ssh").join("id_ed25519.pub"),
        target.dir.join("authorized_keys"),
    )
    .unwrap();
    Some((jump, target))
}

fn jump_env(jump: &Server, target_port: u16, proxy: &str) -> Env {
    let user = local_user();
    Env {
        home: jump.dir.clone(),
        local_user: user.clone(),
        config: format!(
            "Host jump\n  HostName 127.0.0.1\n  Port {}\n  User {user}\n  IdentityFile ~/.ssh/id_ed25519\n\
             Host inner\n  HostName 127.0.0.1\n  Port {target_port}\n  User {user}\n  IdentityFile ~/.ssh/id_ed25519\n  {proxy}\n",
            jump.port
        ),
    }
}

#[test]
fn logs_in_through_jump_hosts_each_in_a_worker_of_its_own() {
    let Some((jump, target)) = two_servers("hops") else {
        return;
    };
    let env = jump_env(&jump, target.port, "ProxyJump jump");
    let trust = [("to trust this key", "yes"), ("Passphrase", "secret")];
    let (run, session) = connect_to(&env, "inner", "echo in$((40+2)); exit\n", &trust);
    assert!(run.output.contains("in42"), "{}\n{}", run.shown, run.output);
    assert_eq!(session.hop_connections().len(), 1);
    assert!(run.shown.contains(" through "), "{}", run.shown);
    assert!(
        run.shown.contains("Logged in to jump host"),
        "{}",
        run.shown
    );
    // Each hop's own host key, and its own login.
    let trusts = run.prompts.iter().filter(|p| p.contains("trust")).count();
    assert_eq!(trusts, 2, "{:?}", run.prompts);
    let known = std::fs::read_to_string(env.known_hosts()).unwrap();
    assert_eq!(known.lines().count(), 2, "{known}");

    // Known now; and through the jump host twice, as `-J jump,jump` would.
    let env = jump_env(&jump, target.port, "ProxyJump jump,jump");
    let (run, session) = connect_to(&env, "inner", "echo two$((1+1)); exit\n", &trust);
    assert!(run.output.contains("two2"), "{}\n{}", run.shown, run.output);
    assert_eq!(session.hop_connections().len(), 2);
    assert!(
        run.prompts.iter().all(|p| !p.contains("trust")),
        "{:?}",
        run.prompts
    );
}

#[test]
fn a_jump_host_that_cannot_reach_the_target_says_so() {
    let Some(jump) = start_sshd("unreachable", "") else {
        return;
    };
    // Not a freed ephemeral port: another test's sshd could take that one.
    // These tests run unprivileged, so nothing listens on 1023.
    let gone: u16 = 1023;
    let env = jump_env(&jump, gone, "ProxyJump jump");
    let trust = [("to trust this key", "yes"), ("Passphrase", "secret")];
    let (run, session) = connect_to(&env, "inner", "", &trust);
    assert_eq!(session.state(), State::Ended);
    assert!(run.shown.contains("could not reach"), "{}", run.shown);
    assert!(run.shown.contains(&gone.to_string()), "{}", run.shown);
}

#[test]
fn logs_in_through_a_proxy_command() {
    let Some((jump, target)) = two_servers("command") else {
        return;
    };
    if std::process::Command::new("nc").arg("-h").output().is_err() {
        eprintln!("nc not found; skipping");
        return;
    }
    let env = jump_env(&jump, target.port, "ProxyCommand nc %h %p");
    let trust = [("to trust this key", "yes"), ("Passphrase", "secret")];
    let (run, _) = connect_to(&env, "inner", "echo nc$((2*3)); exit\n", &trust);
    assert!(run.output.contains("nc6"), "{}\n{}", run.shown, run.output);
    assert!(
        run.shown.contains("through the command nc"),
        "{}",
        run.shown
    );
}
