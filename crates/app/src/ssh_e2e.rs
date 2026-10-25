//! The window's SSH path end to end, minus the window: [`SshSession`]
//! talks to a real worker over OS pipes ([`Link`] and
//! `ferossh_worker::run_blocking`, as on Windows), signs through the real
//! broker with a passphrase-protected key file, and logs in to a real
//! `sshd`. Skipped where `sshd` or `ssh-keygen` is missing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ferossh_auth::Broker;

use crate::link::{Link, LinkEvent};
use crate::ssh::{Effect, Env, SshSession, State, Target};

const SSHD: &str = "/usr/sbin/sshd";

struct Server {
    dir: PathBuf,
    child: Child,
    port: u16,
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

fn start_sshd() -> Option<Server> {
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
    let dir = std::env::temp_dir().join(format!("ferossh-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".ssh")).unwrap();
    let host = dir.join("host_ed25519");
    keygen(&host, "");
    let user = dir.join(".ssh").join("id_ed25519");
    keygen(&user, "secret");
    std::fs::copy(user.with_extension("pub"), dir.join("authorized_keys")).unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = format!(
        "ListenAddress 127.0.0.1\nPort {port}\nHostKey {}\nAuthorizedKeysFile {}\nPidFile none\n\
         StrictModes no\nUsePAM no\nPermitRootLogin yes\nPasswordAuthentication no\n\
         KbdInteractiveAuthentication no\n",
        host.display(),
        dir.join("authorized_keys").display()
    );
    std::fs::write(dir.join("sshd_config"), config).unwrap();
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
    Some(Server { dir, child, port })
}

fn local_user() -> String {
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

/// Connects to the alias "testhost" and runs `command` once logged in,
/// answering prompts from `answers` by their text.
fn connect(env: &Env, command: &str, answers: &[(&str, &str)]) -> (Run, SshSession) {
    let (ui_read, worker_write) = std::io::pipe().unwrap();
    let (worker_read, ui_write) = std::io::pipe().unwrap();
    std::thread::spawn(move || ferossh_worker::run_blocking(worker_read, worker_write));
    let link = Link::start(ui_read, ui_write, || {}).unwrap();
    let broker = Broker::spawn(|| {}).unwrap();

    let target = Target::resolve("testhost", env).unwrap();
    let mut session = SshSession::new(1, target, env.known_hosts());
    let mut run = Run::default();
    let mut effects = session.start(80, 24);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        for effect in std::mem::take(&mut effects) {
            match effect {
                Effect::Send(m) => link.send(m),
                Effect::Show(s) => run.shown.push_str(&s),
                Effect::Output(b) => run.output.push_str(&String::from_utf8_lossy(&b)),
                Effect::Broker(job) => broker.submit(job),
                Effect::Connected => effects.extend(session.input(command.as_bytes())),
                Effect::Ended => return (run, session),
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
        match link.try_recv() {
            Some(LinkEvent::Message(m)) => effects.extend(session.on_worker(m)),
            Some(LinkEvent::Closed(why)) => panic!("worker closed: {why}"),
            None => {
                assert!(Instant::now() < deadline, "timed out:\n{}", run.shown);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn trusts_a_new_host_signs_with_a_key_file_and_runs_a_shell() {
    let Some(server) = start_sshd() else {
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
