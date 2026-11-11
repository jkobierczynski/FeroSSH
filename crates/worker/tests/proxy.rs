//! Connecting through a SOCKS 5 or HTTP proxy: the worker speaks to the
//! proxy, asks it for the host by name and runs SSH over what it opens;
//! a refused login says so with [`Message::ProxyAuth`].
//!
//! The proxies here are small fakes that check what the worker sends.
//! The host behind them is a real sshd when there is one (the worker then
//! gets as far as its host key), else a fake that sends a banner and
//! hangs up (the worker then gets as far as the proxy's verdict).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::time::Duration;

use ferossh_ipc::{Message, PROTOCOL_VERSION, ProxyKind, ProxyServer, Secrets};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

/// What the fake proxy should do with a login.
#[derive(Clone, Copy)]
enum Login {
    None,
    Want(&'static str, &'static str),
}

/// The host behind the proxy: sshd's port if there is one (and the guard
/// that stops it), else a fake's. `name` keeps each test's sshd files
/// apart.
async fn host(name: &str) -> (Option<common::Kill>, u16, bool) {
    if std::path::Path::new(common::SSHD).exists() {
        let dir = std::env::temp_dir().join(format!("ferossh-proxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (kill, port) = common::sshd(&dir, name, "", None);
        return (Some(kill), port, true);
    }
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let _ = s.write_all(b"SSH-2.0-OpenSSH_9.6\r\n").await;
        }
    });
    (None, port, false)
}

/// A SOCKS 5 proxy for one connection; says what host and port were
/// asked for, and relays them to `upstream`.
async fn socks5(login: Login, upstream: u16) -> (u16, oneshot::Receiver<(String, u16)>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut head = [0u8; 2];
        s.read_exact(&mut head).await.unwrap();
        let mut methods = vec![0u8; usize::from(head[1])];
        s.read_exact(&mut methods).await.unwrap();
        assert_eq!(head[0], 5);
        match login {
            Login::None => {
                assert!(methods.contains(&0));
                s.write_all(&[5, 0]).await.unwrap();
            }
            Login::Want(user, pass) => {
                assert!(methods.contains(&2));
                s.write_all(&[5, 2]).await.unwrap();
                let mut v = [0u8; 2];
                s.read_exact(&mut v).await.unwrap();
                let mut u = vec![0u8; usize::from(v[1])];
                s.read_exact(&mut u).await.unwrap();
                let mut plen = [0u8; 1];
                s.read_exact(&mut plen).await.unwrap();
                let mut p = vec![0u8; usize::from(plen[0])];
                s.read_exact(&mut p).await.unwrap();
                if u != user.as_bytes() || p != pass.as_bytes() {
                    s.write_all(&[1, 1]).await.unwrap();
                    return;
                }
                s.write_all(&[1, 0]).await.unwrap();
            }
        }
        let mut req = [0u8; 5];
        s.read_exact(&mut req).await.unwrap();
        assert_eq!(&req[..4], &[5, 1, 0, 3], "CONNECT by name");
        let mut name = vec![0u8; usize::from(req[4])];
        s.read_exact(&mut name).await.unwrap();
        let mut port = [0u8; 2];
        s.read_exact(&mut port).await.unwrap();
        let _ = tx.send((String::from_utf8(name).unwrap(), u16::from_be_bytes(port)));
        let mut up = TcpStream::connect(("127.0.0.1", upstream)).await.unwrap();
        s.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 22])
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut s, &mut up).await;
    });
    (port, rx)
}

/// An HTTP proxy for one connection that answers `status`; says what
/// request it got, and relays a 200's connection to `upstream`.
async fn http(status: &'static str, upstream: u16) -> (u16, oneshot::Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut req = Vec::new();
        let mut b = [0u8; 1];
        while !req.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b).await.unwrap();
            req.push(b[0]);
        }
        let _ = tx.send(String::from_utf8(req).unwrap());
        s.write_all(format!("HTTP/1.1 {status}\r\nX-Junk: 1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        if status.starts_with("200") {
            let mut up = TcpStream::connect(("127.0.0.1", upstream)).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut s, &mut up).await;
        }
    });
    (port, rx)
}

/// Connects to "localhost" on `host_port` through `proxy`; returns what
/// the worker said up to its host key or the end.
async fn connect(proxy: ProxyServer, host_port: u16) -> Vec<Message> {
    let (to_worker, worker_in) = mpsc::channel(64);
    let (worker_out, mut from_worker) = mpsc::channel(64);
    tokio::spawn(ferossh_worker::serve(worker_in, worker_out));
    for msg in [
        Message::Hello {
            version: PROTOCOL_VERSION,
        },
        Message::Connect {
            channel: 1,
            host: "localhost".into(),
            port: host_port,
            user: "nobody".into(),
            cols: 80,
            rows: 24,
            host_key_algs: vec![],
            x11: None,
            legacy: false,
            timing: Default::default(),
            tunnel: None,
            shell: true,
            options: Default::default(),
            family: Default::default(),
            gssapi: false,
            proxy: Some(proxy),
        },
    ] {
        to_worker.send(msg).await.unwrap();
    }
    let mut got = Vec::new();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), from_worker.recv())
            .await
            .expect("timed out")
            .expect("worker hung up");
        let end = matches!(
            msg,
            Message::HostKey { .. } | Message::HostCert { .. } | Message::Disconnected { .. }
        );
        if !matches!(msg, Message::Hello { .. }) {
            got.push(msg);
        }
        if end {
            let _ = to_worker.send(Message::Close { channel: 1 }).await;
            return got;
        }
    }
}

fn proxy(kind: ProxyKind, port: u16, user: &str, pass: &str) -> ProxyServer {
    ProxyServer {
        kind,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        password: if user.is_empty() {
            Secrets::default()
        } else {
            Secrets(vec![pass.into()])
        },
    }
}

/// The worker got through the proxy: to the host key with sshd, else to
/// key exchange with the fake.
fn got_through(got: &[Message], sshd: bool) {
    match got.last() {
        Some(Message::HostKey { .. } | Message::HostCert { .. }) => {}
        Some(Message::Disconnected { reason, .. }) if !sshd => {
            assert!(!reason.contains("proxy"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn socks5_with_a_login_reaches_the_host_by_name() {
    let (_sshd, host_port, sshd) = host("socks-login").await;
    let (port, asked) = socks5(Login::Want("me", "s3cret"), host_port).await;
    let got = connect(proxy(ProxyKind::Socks5, port, "me", "s3cret"), host_port).await;
    assert_eq!(asked.await.unwrap(), ("localhost".to_owned(), host_port));
    assert!(
        matches!(got.first(), Some(Message::ProxyAuth { accepted: true, .. })),
        "{got:?}"
    );
    got_through(&got, sshd);
}

#[tokio::test]
async fn socks5_without_a_login_says_nothing_of_one() {
    let (_sshd, host_port, sshd) = host("socks-none").await;
    let (port, asked) = socks5(Login::None, host_port).await;
    let got = connect(proxy(ProxyKind::Socks5, port, "", ""), host_port).await;
    assert_eq!(asked.await.unwrap().0, "localhost");
    assert!(!got.iter().any(|m| matches!(m, Message::ProxyAuth { .. })));
    got_through(&got, sshd);
}

#[tokio::test]
async fn a_refused_socks5_login_is_reported() {
    let (port, _) = socks5(Login::Want("me", "right"), 9).await;
    let got = connect(proxy(ProxyKind::Socks5, port, "me", "wrong"), 22).await;
    assert!(
        matches!(
            got.first(),
            Some(Message::ProxyAuth {
                accepted: false,
                ..
            })
        ),
        "{got:?}"
    );
    let Some(Message::Disconnected { reason, .. }) = got.last() else {
        panic!("{got:?}");
    };
    assert!(
        reason.contains("refused the user name or password"),
        "{reason}"
    );
}

#[tokio::test]
async fn http_connect_with_basic_login() {
    let (_sshd, host_port, sshd) = host("http-login").await;
    let (port, asked) = http("200 Connection established", host_port).await;
    let got = connect(proxy(ProxyKind::Http, port, "me", "pw"), host_port).await;
    let req = asked.await.unwrap();
    assert!(
        req.starts_with(&format!("CONNECT localhost:{host_port} HTTP/1.1\r\n")),
        "{req}"
    );
    // base64("me:pw")
    assert!(
        req.contains("Proxy-Authorization: Basic bWU6cHc=\r\n"),
        "{req}"
    );
    assert!(matches!(
        got.first(),
        Some(Message::ProxyAuth { accepted: true, .. })
    ));
    got_through(&got, sshd);
}

#[tokio::test]
async fn http_refusals_are_reported() {
    let (port, _) = http("407 Proxy Authentication Required", 9).await;
    let got = connect(proxy(ProxyKind::Http, port, "me", "pw"), 22).await;
    assert!(matches!(
        got.first(),
        Some(Message::ProxyAuth {
            accepted: false,
            ..
        })
    ));

    // A proxy that wants a login when none was given: no verdict, which
    // the window would take as out of turn, only the reason.
    let (port, _) = http("407 Proxy Authentication Required", 9).await;
    let got = connect(proxy(ProxyKind::Http, port, "", ""), 22).await;
    assert!(!got.iter().any(|m| matches!(m, Message::ProxyAuth { .. })));
    let Some(Message::Disconnected { reason, .. }) = got.last() else {
        panic!("{got:?}");
    };
    assert!(
        reason.contains("wants a user name and password"),
        "{reason}"
    );

    let (port, _) = http("403 Forbidden", 9).await;
    let got = connect(proxy(ProxyKind::Http, port, "", ""), 22).await;
    let Some(Message::Disconnected { reason, .. }) = got.last() else {
        panic!("{got:?}");
    };
    assert!(reason.contains("403 Forbidden"), "{reason}");
    assert!(!got.iter().any(|m| matches!(m, Message::ProxyAuth { .. })));
}

#[tokio::test]
async fn an_unreachable_proxy_is_named() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let got = connect(proxy(ProxyKind::Socks5, port, "", ""), 22).await;
    let Some(Message::Disconnected { reason, .. }) = got.last() else {
        panic!("{got:?}");
    };
    assert!(
        reason.contains("Could not connect to the proxy 127.0.0.1"),
        "{reason}"
    );
}
