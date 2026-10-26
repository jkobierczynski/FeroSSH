//! Port and X11 forwarding end to end: the forwarding engine and an
//! [`SshSession`] talk to a real worker over OS pipes, logged in to a real
//! `sshd`, as on Windows. Local, SOCKS 4a and SOCKS 5 forwards are used
//! from this process; a remote forward and X11 from the shell on the
//! server, with a stand-in X server here that checks it gets the real
//! cookie. A remote desktop tunnel goes to the echo server as if it were
//! an RDP server, after one to a closed port is refused, and still works
//! after the shell exited. A second login starts with no forwards, then
//! adds, moves and stops a local and a remote one without reconnecting,
//! checking that the old ports close on both sides. Skipped where
//! `sshd` or `ssh-keygen` is missing; the X11 part also needs `xauth`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use ferossh_auth::Broker;
use ferossh_config::ForwardKind;
use ferossh_forward::x11::{Display, MIT_COOKIE};
use ferossh_ipc::X11Auth;

use crate::forwards::{DesktopEvent, Forwarder, FwdState, Setup};
use crate::link::{Link, LinkEvent};
use crate::ssh::{Effect, Env, SshSession, State, Target};
use crate::ssh_e2e::{local_user, start_sshd};
use ferossh_ipc::ChannelId;

const XAUTH: &str = "/usr/bin/xauth";
const REAL_COOKIE: [u8; 16] = [0x5a; 16];

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Echoes every connection back, for as long as the test runs.
fn echo_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = s.try_clone().unwrap();
                let mut w = s;
                let _ = std::io::copy(&mut r, &mut w);
            });
        }
    });
    port
}

/// A stand-in X server on a free display number: answers `XOK` to a
/// setup that carries the real cookie, and `BAD` to anything else.
fn x_server() -> Display {
    let (l, number) = (50..200)
        .find_map(|n| {
            TcpListener::bind(("127.0.0.1", 6000 + n))
                .ok()
                .map(|l| (l, n))
        })
        .unwrap();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut buf = vec![0u8; 12 + 20 + 16];
            let ok = s.read_exact(&mut buf).is_ok()
                && buf[6..8] == [18, 0]
                && buf[8..10] == [16, 0]
                && &buf[12..30] == MIT_COOKIE.as_bytes()
                && buf[32..48] == REAL_COOKIE;
            let _ = s.write_all(if ok { b"XOK" } else { b"BAD" });
        }
    });
    Display::parse(&format!("127.0.0.1:{number}")).unwrap()
}

fn echo_through(mut s: TcpStream, data: &[u8]) -> Vec<u8> {
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut r = s.try_clone().unwrap();
    let len = data.len();
    let reader = std::thread::spawn(move || {
        let mut got = vec![0u8; len];
        r.read_exact(&mut got).map(|()| got)
    });
    s.write_all(data).unwrap();
    reader.join().unwrap().expect("echo")
}

fn socks5(port: u16, target: u16) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.write_all(&[5, 1, 0]).unwrap();
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).unwrap();
    assert_eq!(reply, [5, 0]);
    let mut req = vec![5, 1, 0, 1, 127, 0, 0, 1];
    req.extend(target.to_be_bytes());
    s.write_all(&req).unwrap();
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).unwrap();
    assert_eq!(reply[..2], [5, 0], "SOCKS 5 refused");
    s
}

fn socks4a(port: u16, target: u16) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut req = vec![4, 1];
    req.extend(target.to_be_bytes());
    req.extend([0, 0, 0, 1, b'm', b'e', 0]);
    req.extend(b"localhost\0");
    s.write_all(&req).unwrap();
    let mut reply = [0u8; 8];
    s.read_exact(&mut reply).unwrap();
    assert_eq!(reply[1], 0x5a, "SOCKS 4a refused");
    s
}

/// Uses the local and dynamic forwards from this side.
fn local_side(local: u16, dynamic: u16, echo: u16, closed_target: u16) {
    let s = TcpStream::connect(("127.0.0.1", local)).unwrap();
    assert_eq!(echo_through(s, b"abc"), b"abc");
    // Several times the stream window, both ways at once.
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let s = TcpStream::connect(("127.0.0.1", local)).unwrap();
    assert!(echo_through(s, &big) == big, "bulk data came back changed");
    assert_eq!(
        echo_through(socks5(dynamic, echo), b"via socks5"),
        b"via socks5"
    );
    assert_eq!(echo_through(socks4a(dynamic, echo), b"via 4a"), b"via 4a");
    // A target nobody listens on: the SOCKS client hears so.
    let mut s = TcpStream::connect(("127.0.0.1", dynamic)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.write_all(&[5, 1, 0]).unwrap();
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).unwrap();
    let mut req = vec![5, 1, 0, 1, 127, 0, 0, 1];
    req.extend(closed_target.to_be_bytes());
    s.write_all(&req).unwrap();
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).unwrap();
    assert_ne!(reply[1], 0, "connecting to a closed port succeeded");
}

fn desktop_event(forwarder: &Forwarder) -> DesktopEvent {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(e) = forwarder.take_desktop_events().into_iter().next() {
            return e;
        }
        assert!(Instant::now() < deadline, "no remote desktop answer");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Opens a remote desktop on a closed port, then on the echo server, and
/// returns the desktop's open stream.
fn desktop_side(forwarder: &Forwarder, connection: ChannelId, echo: u16, closed: u16) -> TcpStream {
    forwarder.open_desktop(connection, "localhost", closed, 0);
    match desktop_event(forwarder) {
        DesktopEvent::Failed { why, .. } => {
            assert!(why.contains("no remote desktop service"), "{why}");
        }
        e => panic!("a closed port opened a desktop: {e:?}"),
    }
    forwarder.open_desktop(connection, "localhost", echo, free_port());
    let DesktopEvent::Ready { addr, .. } = desktop_event(forwarder) else {
        panic!("the desktop tunnel failed");
    };
    let s = TcpStream::connect(addr.as_str()).unwrap();
    assert_eq!(echo_through(s.try_clone().unwrap(), b"rdp"), b"rdp");
    s
}

#[test]
fn local_remote_dynamic_and_x11_forwards() {
    let x11 = Path::new(XAUTH).exists();
    // CI installs xauth with sshd; there the X11 part must run.
    assert!(
        x11 || std::env::var_os("FEROSSH_REQUIRE_SSHD").is_none(),
        "FEROSSH_REQUIRE_SSHD is set but xauth is missing"
    );
    let extra = if x11 {
        format!("X11Forwarding yes\nX11UseLocalhost yes\nXAuthLocation {XAUTH}")
    } else {
        String::new()
    };
    let Some(server) = start_sshd("forwards", &extra) else {
        return;
    };
    let user = local_user();
    let echo = echo_server();
    let (local, dynamic, remote) = (free_port(), free_port(), free_port());
    let closed_target = free_port();
    let env = Env {
        home: server.dir.clone(),
        local_user: user.clone(),
        config: format!(
            "Host testhost\n  HostName 127.0.0.1\n  Port {}\n  User {user}\n  \
             IdentityFile ~/.ssh/id_ed25519\n  LocalForward {local} 127.0.0.1:{echo}\n  \
             DynamicForward {dynamic}\n  RemoteForward {remote} 127.0.0.1:{echo}\n  \
             ForwardX11 yes\n",
            server.port
        ),
    };
    let target = Target::resolve("testhost", &env).unwrap();
    assert_eq!(target.forwards.len(), 3);
    assert!(target.forward_x11);

    let (ui_read, worker_write) = std::io::pipe().unwrap();
    let (worker_read, ui_write) = std::io::pipe().unwrap();
    std::thread::spawn(move || ferossh_worker::run_blocking(worker_read, worker_write));
    let (forwarder, runner) = Forwarder::new();
    let link = Link::start(ui_read, ui_write, || {}, forwarder.sink()).unwrap();
    runner.spawn(link.sender(), || {}).unwrap();
    let broker = Broker::spawn(|| {}).unwrap();

    let display = x_server();
    let mut session = SshSession::new(1, target.clone(), env.known_hosts());
    if x11 {
        session.x11 = Some(X11Auth {
            screen: 0,
            protocol: MIT_COOKIE.into(),
            cookie: REAL_COOKIE.to_vec(),
        });
    }
    let setup = Setup {
        forwards: target.forwards.clone(),
        gateway_ports: false,
        x11: x11.then(|| display.clone()),
    };
    let x_check = if x11 {
        // The client on the server sends a setup with the fake cookie
        // xauth holds there; the X server here must get the real one.
        "c=$(xauth list \"$DISPLAY\" | awk '{print $3}'); \
         d=${DISPLAY#*:}; d=${d%.*}; \
         exec 4<>/dev/tcp/127.0.0.1/$((6000+d)); \
         printf 'l\\0\\13\\0\\0\\0\\22\\0\\20\\0\\0\\0MIT-MAGIC-COOKIE-1\\0\\0' >&4; \
         printf \"$(echo $c | sed 's/../\\\\x&/g')\" >&4; \
         echo X=$(head -c3 <&4); "
    } else {
        ""
    };
    let command = format!(
        "exec 3<>/dev/tcp/127.0.0.1/{remote}; printf hello >&3; echo R=$(head -c5 <&3); \
         {x_check}echo DO''NE\n"
    );

    let mut effects = session.start(80, 24);
    let (mut shown, mut output) = (String::new(), String::new());
    let mut client: Option<std::thread::JoinHandle<TcpStream>> = None;
    let (mut sent_command, mut sent_exit) = (false, false);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        for effect in std::mem::take(&mut effects) {
            match effect {
                Effect::Send(m) => link.send(m),
                Effect::Show(s) => shown.push_str(&s),
                Effect::Output(b) => output.push_str(&String::from_utf8_lossy(&b)),
                Effect::Broker(job) => broker.submit(job),
                Effect::Connected => {
                    forwarder.start(session.connection, target.label(), setup.clone());
                }
                Effect::Ended => {}
                // No password source in these tests.
                Effect::FetchPassword | Effect::StorePassword { .. } => {}
                Effect::Tunnel { .. } | Effect::ProxyCommand { .. } => {}
            }
        }
        if session.state() == State::Ended {
            // Only the exit sent below may end the shell: opening forwards
            // or a desktop on the connection must leave it alone.
            assert!(sent_exit, "the shell ended early:\n{shown}\n{output}");
            break;
        }
        if session.prompt_active() {
            let last = shown.rsplit("\r\n").next().unwrap_or_default().to_owned();
            let answer = if last.contains("to trust this key") {
                "yes"
            } else if last.contains("Passphrase") {
                "secret"
            } else {
                panic!("unexpected prompt {last:?}")
            };
            effects.extend(session.prompt_input(answer.as_bytes()));
            effects.extend(session.prompt_input(b"\r"));
            continue;
        }
        if !sent_command && session.state() == State::Connected {
            let view = forwarder.view();
            let listening = view.first().is_some_and(|c| {
                c.forwards.len() == 3
                    && c.forwards
                        .iter()
                        .all(|f| matches!(f.state, FwdState::Listening(_)))
            });
            if let Some(failed) = view
                .iter()
                .flat_map(|c| &c.forwards)
                .find(|f| matches!(f.state, FwdState::Failed(_)))
            {
                panic!("forward failed: {failed:?}");
            }
            if listening {
                sent_command = true;
                let (fw, conn) = (forwarder.clone(), session.connection);
                client = Some(std::thread::spawn(move || {
                    local_side(local, dynamic, echo, closed_target);
                    desktop_side(&fw, conn, echo, closed_target)
                }));
                effects.extend(session.input(command.as_bytes()));
                continue;
            }
        }
        // The last shell's exit ends the connection and its forwards, so
        // only once this side is done too.
        if !sent_exit && output.contains("DONE") && client.as_ref().is_some_and(|c| c.is_finished())
        {
            sent_exit = true;
            effects.extend(session.input(b"exit\n"));
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
                assert!(
                    Instant::now() < deadline,
                    "timed out:\n{shown}\n{output}\n{:?}",
                    forwarder.view()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    assert!(output.contains("R=hello"), "{shown}\n{output}");
    if x11 {
        assert!(output.contains("X=XOK"), "{shown}\n{output}");
    }
    let desktop = client.expect("forwards never listened").join().unwrap();
    // The shell is gone, but the remote desktop keeps the connection.
    assert_eq!(echo_through(desktop, b"still here"), b"still here");

    let view = forwarder.view();
    let conn = view.first().expect("the connection's forwards");
    let by_kind = |k: ForwardKind| conn.forwards.iter().find(|f| f.spec.kind == k).unwrap();
    assert_eq!(by_kind(ForwardKind::Local).total, 2);
    assert_eq!(by_kind(ForwardKind::Dynamic).total, 3);
    assert_eq!(by_kind(ForwardKind::Remote).total, 1);
    assert!(conn.inbound());
    if x11 {
        assert_eq!(conn.x11.as_ref().unwrap().total, 1);
    }
    forwarder.stop(session.connection);
}

/// A logged-in shell on the test sshd, driven a step at a time.
struct Shell {
    session: SshSession,
    link: Link,
    broker: Broker,
    forwarder: Forwarder,
    effects: Vec<crate::ssh::Effect>,
    shown: String,
    output: String,
}

impl Shell {
    /// Runs the session until `done` holds, answering the host key and
    /// passphrase prompts; the forwards start, empty, on login.
    fn until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            for effect in std::mem::take(&mut self.effects) {
                match effect {
                    Effect::Send(m) => self.link.send(m),
                    Effect::Show(s) => self.shown.push_str(&s),
                    Effect::Output(b) => self.output.push_str(&String::from_utf8_lossy(&b)),
                    Effect::Broker(job) => self.broker.submit(job),
                    Effect::Connected => {
                        let label = self.session.target.label();
                        self.forwarder
                            .start(self.session.connection, label, Setup::default());
                    }
                    Effect::Ended | Effect::FetchPassword | Effect::StorePassword { .. } => {}
                    Effect::Tunnel { .. } | Effect::ProxyCommand { .. } => {}
                }
            }
            if done(self) {
                return;
            }
            assert_ne!(
                self.session.state(),
                State::Ended,
                "the shell ended:\n{}",
                self.shown
            );
            if self.session.prompt_active() {
                let last = self.shown.rsplit("\r\n").next().unwrap_or_default();
                let answer = if last.contains("to trust this key") {
                    "yes"
                } else if last.contains("Passphrase") {
                    "secret"
                } else {
                    panic!("unexpected prompt {last:?}")
                };
                let mut e = self.session.prompt_input(answer.as_bytes());
                e.extend(self.session.prompt_input(b"\r"));
                self.effects.extend(e);
                continue;
            }
            if let Some(reply) = self.broker.try_recv() {
                let e = self.session.on_broker(reply);
                self.effects.extend(e);
                continue;
            }
            match self.link.try_recv() {
                Some(LinkEvent::Message(m)) => {
                    let e = self.session.on_worker(m);
                    self.effects.extend(e);
                }
                Some(LinkEvent::Closed(why)) => panic!("worker closed: {why}"),
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for {what}:\n{}\n{}\n{:?}",
                        self.shown,
                        self.output,
                        self.forwarder.view()
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    /// Runs `command` in the shell and waits for it to print
    /// `marker=...`, which it writes as `marker''=` so that the echo of
    /// the command line does not count.
    fn run(&mut self, command: &str, marker: &str) -> String {
        let e = self.session.input(format!("{command}\n").as_bytes());
        self.effects.extend(e);
        let key = format!("{marker}=");
        let find = |s: &Self| {
            s.output
                .lines()
                .filter_map(|l| Some(l.get(l.find(&key)?..)?.trim_end().to_owned()))
                .next_back()
        };
        self.until(marker, |s| find(s).is_some());
        let line = find(self).unwrap_or_default();
        self.output.clear();
        line
    }

    /// The forwards once each is listening or has stopped.
    fn settled(&mut self, count: usize) -> Vec<crate::forwards::FwdView> {
        self.until("the forwards to settle", |s| {
            let view = s.forwarder.view();
            view.first().is_some_and(|c| {
                c.forwards.len() == count
                    && c.forwards
                        .iter()
                        .all(|f| !matches!(f.state, FwdState::Starting))
            })
        });
        self.forwarder.view().remove(0).forwards
    }
}

/// Whether the server still listens on `port`, asked from the shell: a
/// cancelled remote forward may take a moment to close there.
fn server_listens(shell: &mut Shell, port: u16, marker: &str) -> String {
    shell.run(
        &format!(
            "for i in $(seq 100); do (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null || \
             break; sleep 0.05; done; (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null && \
             echo {marker}''=open || echo {marker}''=closed"
        ),
        marker,
    )
}

#[test]
fn forwards_change_on_a_live_connection() {
    let Some(server) = start_sshd("forwards-live", "") else {
        return;
    };
    let user = local_user();
    let echo = echo_server();
    let env = Env {
        home: server.dir.clone(),
        local_user: user.clone(),
        config: format!(
            "Host testhost\n  HostName 127.0.0.1\n  Port {}\n  User {user}\n  \
             IdentityFile ~/.ssh/id_ed25519\n",
            server.port
        ),
    };
    let target = Target::resolve("testhost", &env).unwrap();
    assert!(target.forwards.is_empty());

    let (ui_read, worker_write) = std::io::pipe().unwrap();
    let (worker_read, ui_write) = std::io::pipe().unwrap();
    std::thread::spawn(move || ferossh_worker::run_blocking(worker_read, worker_write));
    let (forwarder, runner) = Forwarder::new();
    let link = Link::start(ui_read, ui_write, || {}, forwarder.sink()).unwrap();
    runner.spawn(link.sender(), || {}).unwrap();
    let mut session = SshSession::new(1, target, env.known_hosts());
    let effects = session.start(80, 24);
    let mut shell = Shell {
        session,
        link,
        broker: Broker::spawn(|| {}).unwrap(),
        forwarder: forwarder.clone(),
        effects,
        shown: String::new(),
        output: String::new(),
    };
    shell.until("the login", |s| {
        s.session.state() == State::Connected && !s.forwarder.view().is_empty()
    });
    let conn = shell.session.connection;
    let spec = |t: String| ferossh_config::forward::parse_option(&t).unwrap();

    // Added after login, no reconnect.
    let (l1, r1) = (free_port(), free_port());
    let local = spec(format!("-L 127.0.0.1:{l1}:127.0.0.1:{echo}"));
    let remote = spec(format!("-R {r1}:127.0.0.1:{echo}"));
    forwarder.add(conn, local.clone());
    forwarder.add(conn, remote.clone());
    let fwds = shell.settled(2);
    assert!(
        fwds.iter()
            .all(|f| matches!(f.state, FwdState::Listening(_))),
        "{fwds:?}"
    );
    let s = TcpStream::connect(("127.0.0.1", l1)).unwrap();
    assert_eq!(echo_through(s, b"one"), b"one");
    let got = shell.run(
        &format!(
            "exec 3<>/dev/tcp/127.0.0.1/{r1}; printf hello >&3; echo R1''=$(head -c5 <&3); \
             exec 3>&-"
        ),
        "R1",
    );
    assert_eq!(got, "R1=hello");

    // Moved to other ports: the old ones close, on both sides.
    let (l2, r2) = (free_port(), free_port());
    let local2 = spec(format!("-L 127.0.0.1:{l2}:127.0.0.1:{echo}"));
    let remote2 = spec(format!("-R {r2}:127.0.0.1:{echo}"));
    forwarder.replace(conn, local, local2.clone());
    forwarder.replace(conn, remote, remote2.clone());
    shell.until("the new forwards", |s| {
        s.forwarder.view().first().is_some_and(|c| {
            c.forwards.iter().map(|f| &f.spec).eq([&local2, &remote2])
                && c.forwards
                    .iter()
                    .all(|f| matches!(f.state, FwdState::Listening(_)))
        })
    });
    let s = TcpStream::connect(("127.0.0.1", l2)).unwrap();
    assert_eq!(echo_through(s, b"two"), b"two");
    assert!(
        TcpListener::bind(("127.0.0.1", l1)).is_ok(),
        "the old port stayed bound"
    );
    assert_eq!(server_listens(&mut shell, r1, "OLD"), "OLD=closed");
    let got = shell.run(
        &format!(
            "exec 3<>/dev/tcp/127.0.0.1/{r2}; printf again >&3; echo R2''=$(head -c5 <&3); \
             exec 3>&-"
        ),
        "R2",
    );
    assert_eq!(got, "R2=again");

    // Stopped: nothing listens any more, and the shell is still there.
    forwarder.remove(conn, 1);
    forwarder.remove(conn, 0);
    shell.until("the forwards to stop", |s| {
        s.forwarder
            .view()
            .first()
            .is_some_and(|c| c.forwards.is_empty())
    });
    assert_eq!(server_listens(&mut shell, r2, "GONE"), "GONE=closed");
    assert_eq!(shell.run("echo ALIVE''=yes", "ALIVE"), "ALIVE=yes");
    forwarder.stop(conn);
}
