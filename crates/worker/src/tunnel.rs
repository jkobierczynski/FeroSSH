//! An SSH connection carried by a stream channel instead of TCP: the hop
//! after a jump host (`ProxyJump`), or a host reached through a local
//! `ProxyCommand`.
//!
//! The UI connects the other end of the stream to a `direct-tcpip`
//! channel that the jump host's own worker opened, or to the command's
//! standard input and output. Either way this worker sees only the bytes
//! of its own SSH connection, which are encrypted end to end: the jump
//! host's worker, which holds the jump host's session, never sees this
//! connection's keys or what flows through it, and this worker never sees
//! the jump host's.
//!
//! Flow control is the stream's (see the `ipc` crate): at most
//! [`STREAM_WINDOW`] bytes in flight each way, credit handed back with
//! [`Message::Window`] as bytes are delivered.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message, STREAM_WINDOW};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{Semaphore, mpsc};

use crate::session::Routes;

/// The bytes russh and the pump may each hold in the pipe between them.
const PIPE: usize = 64 * 1024;

/// The transport for the connection: russh reads and writes the returned
/// end, a task pumps the other end to and from stream `id`. The task ends,
/// and tells the UI with [`Message::Disconnected`], when either side
/// closes.
pub(crate) fn open(
    id: ChannelId,
    inbox: mpsc::UnboundedReceiver<Message>,
    out: mpsc::Sender<Message>,
    routes: Routes,
) -> DuplexStream {
    let (ours, theirs) = tokio::io::duplex(PIPE);
    tokio::spawn(pump(id, theirs, inbox, out, routes));
    ours
}

async fn pump(
    id: ChannelId,
    end: DuplexStream,
    mut inbox: mpsc::UnboundedReceiver<Message>,
    out: mpsc::Sender<Message>,
    routes: Routes,
) {
    let (mut read, mut write) = tokio::io::split(end);
    // What russh writes goes to the UI, within the credit the UI gives.
    let credit = Arc::new(Semaphore::new(STREAM_WINDOW as usize));
    let reader = {
        let (credit, out) = (credit.clone(), out.clone());
        tokio::spawn(async move {
            let mut buf = vec![0u8; MAX_DATA_CHUNK];
            loop {
                let n = match read.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let Ok(permits) = credit
                    .acquire_many(u32::try_from(n).unwrap_or(u32::MAX))
                    .await
                else {
                    break;
                };
                permits.forget();
                let bytes = buf.get(..n).unwrap_or_default().to_vec();
                if out
                    .send(Message::Data { channel: id, bytes })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = out.send(Message::Eof { channel: id }).await;
        })
    };
    // What the UI sends goes to russh; credit goes back once written. The
    // writes run in a task of their own, so a russh that is busy writing
    // never stops this side from reading what it wrote.
    let queued = Arc::new(AtomicUsize::new(0));
    let (wtx, mut wrx) = mpsc::unbounded_channel::<Option<Vec<u8>>>();
    let writer = {
        let (queued, out) = (queued.clone(), out.clone());
        tokio::spawn(async move {
            while let Some(item) = wrx.recv().await {
                let Some(bytes) = item else {
                    let _ = write.shutdown().await;
                    continue;
                };
                if write.write_all(&bytes).await.is_err() {
                    break;
                }
                queued.fetch_sub(bytes.len(), Ordering::Relaxed);
                let bytes = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                let _ = out.send(Message::Window { channel: id, bytes }).await;
            }
        })
    };
    let reason = loop {
        match inbox.recv().await {
            Some(Message::Data { bytes, .. }) => {
                if queued.load(Ordering::Relaxed) + bytes.len() > STREAM_WINDOW as usize {
                    break "The tunnel's other end overran the stream window.".to_owned();
                }
                queued.fetch_add(bytes.len(), Ordering::Relaxed);
                if wtx.send(Some(bytes)).is_err() {
                    break String::new();
                }
            }
            Some(Message::Window { bytes, .. }) => {
                // Never more than a window's worth, whatever the UI says.
                let room = (STREAM_WINDOW as usize).saturating_sub(credit.available_permits());
                credit.add_permits((bytes as usize).min(room));
            }
            Some(Message::Eof { .. }) => {
                let _ = wtx.send(None);
            }
            Some(Message::Close { .. }) | None => break String::new(),
            Some(_) => {}
        }
    };
    // Closing both ends makes russh see the connection drop.
    credit.close();
    reader.abort();
    writer.abort();
    routes.remove(id);
    let _ = out
        .send(Message::Disconnected {
            channel: id,
            reason,
        })
        .await;
}
