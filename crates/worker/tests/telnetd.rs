//! End-to-end test against a real Telnet server: BusyBox's `telnetd`,
//! started on a free local port with a shell instead of a login. The
//! worker connects through [`ferossh_worker::serve`] as the UI would,
//! runs a command, sends a resize and reads it back with `stty size`.
//! Without `busybox telnetd` the test passes without running, unless
//! `FEROSSH_REQUIRE_TELNETD` is set (the Linux CI job sets it).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ferossh_ipc::{Message, PROTOCOL_VERSION};
use tokio::sync::mpsc;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn has_telnetd() -> bool {
    Command::new("busybox")
        .arg("--list")
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == "telnetd")
        })
}

fn start() -> Option<(Server, u16)> {
    if !has_telnetd() {
        assert!(
            std::env::var_os("FEROSSH_REQUIRE_TELNETD").is_none(),
            "busybox telnetd is missing"
        );
        eprintln!("busybox telnetd not found; skipping");
        return None;
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let child = Command::new("busybox")
        .args(["telnetd", "-F", "-b"])
        .arg(format!("127.0.0.1:{port}"))
        .args(["-l", "/bin/sh"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let server = Server(child);
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some((server, port));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("telnetd did not start");
}

/// Reads terminal data until `want` shows up.
async fn wait_for(rx: &mut mpsc::Receiver<Message>, seen: &mut String, want: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains(want) {
        let msg = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no {want:?} in {seen:?}"));
        match msg {
            Some(Message::Data { bytes, .. }) => seen.push_str(&String::from_utf8_lossy(&bytes)),
            other => panic!("{other:?} while waiting for {want:?} in {seen:?}"),
        }
    }
}

#[tokio::test]
async fn runs_a_shell_over_telnet() {
    let Some((_server, port)) = start() else {
        return;
    };
    let (to_worker, inbox) = mpsc::channel(64);
    let (out, mut from_worker) = mpsc::channel(64);
    let worker = tokio::spawn(ferossh_worker::serve(inbox, out));
    assert!(matches!(
        from_worker.recv().await,
        Some(Message::Hello { .. })
    ));
    let send = |m| {
        let tx = to_worker.clone();
        async move { tx.send(m).await.unwrap() }
    };
    send(Message::Hello {
        version: PROTOCOL_VERSION,
    })
    .await;
    send(Message::TelnetConnect {
        channel: 9,
        host: "127.0.0.1".into(),
        port,
        cols: 100,
        rows: 30,
        connect_timeout: 5,
    })
    .await;
    assert!(matches!(
        from_worker.recv().await,
        Some(Message::Connected { channel: 9, .. })
    ));
    let mut seen = String::new();
    // telnetd negotiated echo off its side and NAWS: the shell's terminal
    // has the pane's size, and typed lines come back once.
    send(Message::Data {
        channel: 9,
        bytes: b"stty size; echo done-$((6*7))\r".to_vec(),
    })
    .await;
    wait_for(&mut from_worker, &mut seen, "done-42").await;
    assert!(seen.contains("30 100"), "{seen:?}");
    send(Message::Resize {
        channel: 9,
        cols: 132,
        rows: 43,
    })
    .await;
    // Give the size a moment to reach the pty before asking.
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(Message::Data {
        channel: 9,
        bytes: b"stty size; echo again-$((2*3))\r".to_vec(),
    })
    .await;
    wait_for(&mut from_worker, &mut seen, "again-6").await;
    assert!(seen.contains("43 132"), "{seen:?}");
    send(Message::Data {
        channel: 9,
        bytes: b"exit\r".to_vec(),
    })
    .await;
    loop {
        match tokio::time::timeout(Duration::from_secs(10), from_worker.recv())
            .await
            .unwrap()
        {
            Some(Message::Data { .. }) => {}
            Some(Message::Disconnected { channel: 9, .. }) => break,
            other => panic!("{other:?}"),
        }
    }
    drop(to_worker);
    worker.await.unwrap();
}
