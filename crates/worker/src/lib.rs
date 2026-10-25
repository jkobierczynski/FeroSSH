//! The sandboxed SSH worker: russh sessions, channels and forwards.
//!
//! `ferossh.exe` starts a copy of itself in an AppContainer that may only
//! use the network, and talks to it over two inherited pipes. The worker
//! cannot open the user's files or keys: everything it needs from them
//! (known_hosts verdicts, public keys, signatures, passwords) it asks the
//! UI for over IPC; file transfers stream through IPC for the same reason
//! ([`sftp`]). This crate is that worker, minus the process setup,
//! and runs anywhere tokio does, so it is tested against a real OpenSSH
//! server on Linux.

pub mod crypto_policy;
mod session;
mod sftp;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::thread;

use ferossh_ipc::{ChannelId, Decoder, Message, PROTOCOL_VERSION, encode};
use tokio::sync::mpsc;

use session::Connections;
pub use session::Params;

/// Messages queued in each direction before the sender waits.
const QUEUE: usize = 64;

/// Serves the UI: one task per shell or SFTP channel, routed by channel id;
/// shells opened with [`Message::OpenChannel`] and SFTP channels share their
/// host's connection. Returns when the UI closes its end.
pub async fn serve(mut inbox: mpsc::Receiver<Message>, out: mpsc::Sender<Message>) {
    let hello = Message::Hello {
        version: PROTOCOL_VERSION,
    };
    if out.send(hello).await.is_err() {
        return;
    }
    match inbox.recv().await {
        Some(Message::Hello { version }) if version == PROTOCOL_VERSION => {}
        _ => return,
    }
    let mut sessions: HashMap<ChannelId, mpsc::Sender<Message>> = HashMap::new();
    let connections = Connections::default();
    while let Some(msg) = inbox.recv().await {
        match msg {
            Message::OpenChannel {
                channel,
                connection,
                cols,
                rows,
            } => {
                sessions.retain(|_, tx| !tx.is_closed());
                if sessions.contains_key(&channel) {
                    continue;
                }
                let (tx, rx) = mpsc::channel(QUEUE);
                sessions.insert(channel, tx);
                tokio::spawn(session::open(
                    channel,
                    connection,
                    (cols, rows),
                    rx,
                    out.clone(),
                    connections.clone(),
                ));
            }
            Message::SftpOpen {
                channel,
                connection,
            } => {
                sessions.retain(|_, tx| !tx.is_closed());
                if sessions.contains_key(&channel) {
                    continue;
                }
                let (tx, rx) = mpsc::channel(QUEUE);
                sessions.insert(channel, tx);
                tokio::spawn(sftp::run(
                    channel,
                    connection,
                    rx,
                    out.clone(),
                    connections.clone(),
                ));
            }
            Message::Connect {
                channel,
                host,
                port,
                user,
                cols,
                rows,
                host_key_algs,
            } => {
                sessions.retain(|_, tx| !tx.is_closed());
                if sessions.contains_key(&channel) {
                    continue;
                }
                let (tx, rx) = mpsc::channel(QUEUE);
                sessions.insert(channel, tx);
                let params = Params {
                    host,
                    port,
                    user,
                    cols,
                    rows,
                    host_key_algs,
                };
                tokio::spawn(session::run(
                    channel,
                    params,
                    rx,
                    out.clone(),
                    connections.clone(),
                ));
            }
            other => {
                let Some(channel) = other.channel() else {
                    continue;
                };
                if let Some(tx) = sessions.get(&channel)
                    && tx.send(other).await.is_err()
                {
                    sessions.remove(&channel);
                }
            }
        }
    }
}

/// Runs the worker over a byte stream pair (the inherited pipes): a reader
/// thread decodes frames, a writer thread encodes them, and the sessions
/// run on a single-threaded runtime. Returns when the UI goes away or
/// sends a malformed frame.
pub fn run_blocking(
    mut input: impl Read + Send + 'static,
    mut output: impl Write + Send + 'static,
) -> std::io::Result<()> {
    let (in_tx, in_rx) = mpsc::channel::<Message>(QUEUE);
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(QUEUE);
    thread::Builder::new()
        .name("ipc reader".into())
        .spawn(move || {
            let mut decoder = Decoder::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = match input.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                decoder.feed(buf.get(..n).unwrap_or_default());
                loop {
                    match decoder.next_message() {
                        Ok(Some(msg)) => {
                            if in_tx.blocking_send(msg).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        // Out of sync: drop the connection.
                        Err(_) => return,
                    }
                }
            }
        })?;
    thread::Builder::new()
        .name("ipc writer".into())
        .spawn(move || {
            let mut frame = Vec::new();
            while let Some(msg) = out_rx.blocking_recv() {
                frame.clear();
                if encode(&msg, &mut frame).is_err() {
                    continue;
                }
                if output
                    .write_all(&frame)
                    .and_then(|()| output.flush())
                    .is_err()
                {
                    return;
                }
            }
        })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve(in_rx, out_tx));
    Ok(())
}
