//! Port and X11 forwarding end to end: the forwarding engine and an
//! [`SshSession`] talk to a real worker over OS pipes, logged in to a real
//! `sshd`, as on Windows. Local, SOCKS 4a and SOCKS 5 forwards are used
//! from this process; a remote forward and X11 from the shell on the
//! server, with a stand-in X server here that checks it gets the real
//! cookie. Skipped where `sshd` or `ssh-keygen` is missing; the X11 part
//! also needs `xauth`.

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

use crate::forwards::{Forwarder, FwdState, Setup};
use crate::link::{Link, LinkEvent};
use crate::ssh::{Effect, Env, SshSession, State, Target};
use crate::ssh_e2e::{local_user, start_sshd};

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
    let mut client: Option<std::thread::JoinHandle<()>> = None;
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
            }
        }
        if session.state() == State::Ended {
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
                client = Some(std::thread::spawn(move || {
                    local_side(local, dynamic, echo, closed_target);
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
    client.expect("forwards never listened").join().unwrap();

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
