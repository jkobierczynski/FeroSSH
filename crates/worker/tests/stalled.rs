//! One session whose server stops reading must not hold up the others on
//! the worker, nor fill the worker's memory: once its queue is full the
//! worker ends it and says why, while a second session keeps working.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use ferossh_ipc::{Message, PROTOCOL_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[tokio::test]
async fn a_stalled_session_is_closed_and_the_others_go_on() {
    // A server that takes the connection and never reads from it.
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_port = stalled.local_addr().unwrap().port();
    let _hold = tokio::spawn(async move {
        let (s, _) = stalled.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(120)).await;
        drop(s);
    });
    // A server that echoes.
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = echo.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        loop {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if s.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let (to_worker, inbox) = mpsc::channel(64);
    let (out, mut from_worker) = mpsc::channel(64);
    tokio::spawn(ferossh_worker::serve(inbox, out));
    assert!(matches!(
        from_worker.recv().await,
        Some(Message::Hello { .. })
    ));
    to_worker
        .send(Message::Hello {
            version: PROTOCOL_VERSION,
        })
        .await
        .unwrap();
    for (channel, port) in [(1, stalled_port), (2, echo_port)] {
        to_worker
            .send(Message::TelnetConnect {
                channel,
                host: "127.0.0.1".into(),
                port,
                cols: 80,
                rows: 24,
                connect_timeout: 5,
            })
            .await
            .unwrap();
    }
    let mut connected = 0;
    while connected < 2 {
        match from_worker.recv().await {
            Some(Message::Connected { .. }) => connected += 1,
            Some(Message::Data { .. }) => {}
            other => panic!("{other:?}"),
        }
    }

    // Fill the socket buffers with big chunks, then the queues with small
    // ones, reading what comes back meanwhile.
    let flood = {
        let to_worker = to_worker.clone();
        tokio::spawn(async move {
            for i in 0..20_000 {
                let bytes = if i < 512 {
                    vec![b'a'; 64 * 1024]
                } else {
                    vec![b'a']
                };
                if to_worker
                    .send(Message::Data { channel: 1, bytes })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match tokio::time::timeout_at(deadline, from_worker.recv()).await {
            Ok(Some(Message::Disconnected { channel: 1, reason })) => {
                assert!(reason.contains("stopped taking input"), "{reason}");
                break;
            }
            Ok(Some(Message::Data { .. })) => {}
            other => panic!("{other:?}"),
        }
    }
    flood.await.unwrap();

    // The other session still answers.
    to_worker
        .send(Message::Data {
            channel: 2,
            bytes: b"still-here".to_vec(),
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    while !seen.windows(10).any(|w| w == b"still-here") {
        match tokio::time::timeout(Duration::from_secs(10), from_worker.recv()).await {
            Ok(Some(Message::Data { channel: 2, bytes })) => seen.extend(bytes),
            Ok(Some(Message::Disconnected { channel: 1, .. })) => {}
            other => panic!("{other:?}"),
        }
    }
}
