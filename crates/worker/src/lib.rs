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
mod exec;
mod forward;
mod session;
mod sftp;
pub mod telnet;
mod tunnel;
mod zmodem;

use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

use ferossh_ipc::{ChannelId, Decoder, Message, PROTOCOL_VERSION, Side, encode};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroize;

pub use session::Params;
use session::{Connections, Route, SessionGuard};

/// Messages queued in each direction before the sender waits.
const QUEUE: usize = 64;
/// Messages the dispatcher queues for one session that is not taking
/// them (its server stopped reading). Past this the session is closed
/// rather than let what the user types or pastes fill the worker's memory.
const FRONT_QUEUE: usize = 4096;
/// Bytes the dispatcher queues for one such session, so a big paste into
/// a stalled session is capped too, not only many small messages. Far
/// more than anyone pastes into a terminal; file transfers are paced by
/// their own requests and never come near it.
const FRONT_BYTES: usize = 32 << 20;

/// About what a queued message holds in memory: its size on the wire,
/// which counts every field (data, names, paths), plus a little for the
/// rest.
fn weight(msg: &Message) -> usize {
    let mut frame = Vec::new();
    let wire = match encode(msg, &mut frame) {
        Ok(()) => frame.len(),
        // Cannot be sent on, so as much as any one message may be.
        Err(_) => ferossh_ipc::MAX_BODY_LEN,
    };
    // The copy may hold a password the user typed.
    frame.zeroize();
    wire.saturating_add(64)
}

/// What became of a message handed to a session's front queue.
enum Queued {
    Yes,
    /// The queue holds [`FRONT_QUEUE`] messages or [`FRONT_BYTES`] bytes.
    Full,
    /// The session has ended.
    Closed,
}

/// Hands `msg` to a session's front queue without waiting.
fn queue(tx: &mpsc::Sender<(Message, usize)>, guard: &SessionGuard, msg: Message) -> Queued {
    let n = weight(&msg);
    if !guard.reserve(n, FRONT_BYTES) {
        return Queued::Full;
    }
    match tx.try_send((msg, n)) {
        Ok(()) => Queued::Yes,
        Err(e) => {
            guard.release(n);
            match e {
                mpsc::error::TrySendError::Full(_) => Queued::Full,
                mpsc::error::TrySendError::Closed(_) => Queued::Closed,
            }
        }
    }
}

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
    // A queue for a session task, or `None` when the id is taken. The
    // dispatcher hands messages to a front queue without waiting and a
    // task of its own feeds them on, so one session that stops reading
    // (its server stopped taking data) never stalls the others. When the
    // front queue fills, the dispatcher aborts the session (it may be
    // stuck in a write), drops its route, which stops the feeding task,
    // and tells the UI the session ended and why.
    let session = |channel: ChannelId| {
        let (tx, rx) = mpsc::channel(QUEUE);
        let (front, mut queued) = mpsc::channel::<(Message, usize)>(FRONT_QUEUE);
        let (alive, mut gone) = oneshot::channel::<()>();
        let guard = Arc::new(SessionGuard::new(alive));
        let taken = guard.clone();
        if !routes.insert(channel, Route::Session(front, guard.clone())) {
            return None;
        }
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut gone => break,
                    () = tx.closed() => break,
                    msg = queued.recv() => match msg {
                        Some((msg, n)) => {
                            tokio::select! {
                                biased;
                                _ = &mut gone => break,
                                sent = tx.send(msg) => if sent.is_err() {
                                    break;
                                },
                            }
                            taken.release(n);
                        }
                        None => break,
                    },
                }
            }
        });
        Some((rx, guard))
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
                let Some((rx, guard)) = session(channel) else {
                    continue;
                };
                guard.track(tokio::spawn(session::open(
                    channel,
                    connection,
                    (cols, rows),
                    x11,
                    rx,
                    out.clone(),
                    connections.clone(),
                )));
            }
            Message::SftpOpen {
                channel,
                connection,
            } => {
                let Some((rx, guard)) = session(channel) else {
                    continue;
                };
                guard.track(tokio::spawn(sftp::run(
                    channel,
                    connection,
                    rx,
                    out.clone(),
                    connections.clone(),
                )));
            }
            Message::Exec {
                channel,
                connection,
                command,
                split_stderr,
            } => {
                let Some((rx, guard)) = session(channel) else {
                    continue;
                };
                guard.track(tokio::spawn(exec::run(
                    channel,
                    connection,
                    command,
                    split_stderr,
                    rx,
                    out.clone(),
                    connections.clone(),
                )));
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
                let Some((rx, guard)) = session(channel) else {
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
                guard.track(tokio::spawn(session::run(
                    channel,
                    params,
                    rx,
                    tunnel_rx,
                    out.clone(),
                    connections.clone(),
                )));
            }
            Message::TelnetConnect {
                channel,
                host,
                port,
                cols,
                rows,
                connect_timeout,
            } => {
                let Some((rx, guard)) = session(channel) else {
                    continue;
                };
                let params = telnet::Params {
                    host,
                    port,
                    cols,
                    rows,
                    connect_timeout,
                };
                guard.track(tokio::spawn(telnet::run(channel, params, rx, out.clone())));
            }
            Message::RemoteForward {
                channel,
                connection,
                bind,
                port,
            } => {
                let Some((rx, guard)) = session(channel) else {
                    continue;
                };
                guard.track(tokio::spawn(forward::remote(
                    channel,
                    connection,
                    bind,
                    port,
                    rx,
                    out.clone(),
                    connections.clone(),
                )));
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
                    Some(Route::Session(tx, guard)) => match queue(&tx, &guard, other) {
                        Queued::Yes => true,
                        Queued::Full => {
                            // Its server has stopped taking what the user
                            // types or pastes. End the session even if it
                            // is stuck in a write, and say why: the input
                            // from here on is lost.
                            guard.abort();
                            routes.remove(channel);
                            let reason = "The server stopped taking input, so the \
                                          session was closed and the last input was \
                                          not sent."
                                .to_owned();
                            // Sent from its own task: a UI slow to read
                            // must not hold up input for other sessions.
                            // The session is gone, so nothing for this
                            // channel can follow it.
                            let out = out.clone();
                            tokio::spawn(async move {
                                let _ = out.send(Message::Disconnected { channel, reason }).await;
                            });
                            continue;
                        }
                        Queued::Closed => false,
                    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use ferossh_ipc::SftpOp;

    /// A paste far past the byte budget fills the front queue long before
    /// its message count does, and room comes back as the session reads.
    #[tokio::test]
    async fn the_front_queue_is_capped_by_bytes() {
        let (tx, mut rx) = mpsc::channel(FRONT_QUEUE);
        let (alive, _gone) = oneshot::channel();
        let guard = SessionGuard::new(alive);
        let chunk = || Message::Data {
            channel: 1,
            bytes: vec![b'x'; ferossh_ipc::MAX_DATA_CHUNK],
        };
        let mut queued = 0;
        while matches!(queue(&tx, &guard, chunk()), Queued::Yes) {
            queued += 1;
        }
        assert!(queued < FRONT_QUEUE);
        assert!(queued * ferossh_ipc::MAX_DATA_CHUNK <= FRONT_BYTES);
        let taken = rx.recv().await.map(|(_, n)| n).unwrap_or_default();
        guard.release(taken);
        assert!(matches!(queue(&tx, &guard, chunk()), Queued::Yes));
    }

    /// Requests count what they carry, not only file data: long paths
    /// fill the budget as surely as a paste does.
    #[tokio::test]
    async fn long_paths_count_against_the_budget() {
        let (tx, _rx) = mpsc::channel(FRONT_QUEUE);
        let (alive, _gone) = oneshot::channel();
        let guard = SessionGuard::new(alive);
        let stat = || Message::SftpRequest {
            channel: 1,
            id: 0,
            op: SftpOp::Stat {
                path: vec![b'a'; 512 * 1024],
                follow: false,
            },
        };
        let mut queued = 0;
        while matches!(queue(&tx, &guard, stat()), Queued::Yes) {
            queued += 1;
        }
        assert!(queued < FRONT_QUEUE);
        assert!(queued * 512 * 1024 <= FRONT_BYTES);
    }
}
