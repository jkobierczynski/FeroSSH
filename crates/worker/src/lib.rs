//! The sandboxed SSH worker: russh sessions, channels and forwards,
//! Telnet sessions ([`telnet`]), and ZMODEM in either's shell.
//!
//! `ferossh.exe` starts a copy of itself in an AppContainer that may only
//! use the network, and talks to it over two inherited pipes. The worker
//! cannot open the user's files or keys: everything it needs from them
//! (known_hosts verdicts, public keys, signatures, passwords) it asks the
//! UI for over IPC; file transfers stream through IPC for the same reason
//! ([`sftp`]), and so do forwarded connections, whose sockets the UI holds
//! ([`forward`]). This crate is that worker, minus the process setup,
//! and runs anywhere tokio does, so it is tested against a real OpenSSH
//! server on Linux.

pub mod crypto_policy;
mod forward;
mod session;
mod sftp;
pub mod telnet;
mod tunnel;
mod zmodem;

use std::io::{Read, Write};
use std::thread;

use ferossh_ipc::{ChannelId, Decoder, Message, PROTOCOL_VERSION, Side, encode};
use tokio::sync::mpsc;

pub use session::Params;
use session::{Connections, Route};

/// Messages queued in each direction before the sender waits.
const QUEUE: usize = 64;

/// Serves the UI: one task per shell, SFTP channel, remote forward
/// listener and forwarded stream, routed by channel id; shells opened with
/// [`Message::OpenChannel`], SFTP and forwards share their host's
/// connection. A connection through a jump host or a `ProxyCommand` runs
/// over a stream channel of its own ([`tunnel`]). Returns when the UI
/// closes its end.
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
    let connections = Connections::default();
    let routes = connections.routes.clone();
    // A queue for a session task, or `None` when the id is taken.
    let session = |channel: ChannelId| {
        let (tx, rx) = mpsc::channel(QUEUE);
        routes.insert(channel, Route::Session(tx)).then_some(rx)
    };
    while let Some(msg) = inbox.recv().await {
        match msg {
            Message::OpenChannel {
                channel,
                connection,
                cols,
                rows,
                x11,
            } => {
                let Some(rx) = session(channel) else { continue };
                tokio::spawn(session::open(
                    channel,
                    connection,
                    (cols, rows),
                    x11,
                    rx,
                    out.clone(),
                    connections.clone(),
                ));
            }
            Message::SftpOpen {
                channel,
                connection,
            } => {
                let Some(rx) = session(channel) else { continue };
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
                x11,
                legacy,
                timing,
                tunnel,
                shell,
            } => {
                // The tunnel's stream first: the UI may send its bytes
                // right behind this message.
                let tunnel_rx = match tunnel {
                    Some(id) => {
                        let (tx, rx) = mpsc::unbounded_channel();
                        if !routes.insert(id, Route::Stream(tx)) {
                            continue;
                        }
                        Some(rx)
                    }
                    None => None,
                };
                let Some(rx) = session(channel) else {
                    if let Some(id) = tunnel {
                        routes.remove(id);
                    }
                    continue;
                };
                let params = Params {
                    host,
                    port,
                    user,
                    cols,
                    rows,
                    host_key_algs,
                    x11,
                    legacy,
                    timing,
                    tunnel,
                    shell,
                };
                tokio::spawn(session::run(
                    channel,
                    params,
                    rx,
                    tunnel_rx,
                    out.clone(),
                    connections.clone(),
                ));
            }
            Message::TelnetConnect {
                channel,
                host,
                port,
                cols,
                rows,
                connect_timeout,
            } => {
                let Some(rx) = session(channel) else { continue };
                let params = telnet::Params {
                    host,
                    port,
                    cols,
                    rows,
                    connect_timeout,
                };
                tokio::spawn(telnet::run(channel, params, rx, out.clone()));
            }
            Message::RemoteForward {
                channel,
                connection,
                bind,
                port,
            } => {
                let Some(rx) = session(channel) else { continue };
                tokio::spawn(forward::remote(
                    channel,
                    connection,
                    bind,
                    port,
                    rx,
                    out.clone(),
                    connections.clone(),
                ));
            }
            Message::ForwardOpen {
                channel,
                connection,
                target,
                origin_host,
                origin_port,
                keep,
            } => {
                let (tx, rx) = mpsc::unbounded_channel();
                if !routes.insert(channel, Route::Stream(tx)) {
                    continue;
                }
                tokio::spawn(forward::open(
                    channel,
                    connection,
                    target,
                    (origin_host, origin_port),
                    keep,
                    rx,
                    out.clone(),
                    connections.clone(),
                ));
            }
            other => {
                let Some(channel) = other.channel() else {
                    continue;
                };
                let delivered = match routes.get(channel) {
                    Some(Route::Session(tx)) => tx.send(other).await.is_ok(),
                    Some(Route::Stream(tx)) => tx.send(other).is_ok(),
                    None => true,
                };
                if !delivered {
                    routes.remove(channel);
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
                        // Only the worker sends these: the UI is confused.
                        Ok(Some(msg)) if msg.sent_by() == Side::Worker => return,
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
