//! Session > Remote apps > Run remote app: a Linux program on the host, its windows as
//! native windows here, through FeroStream.
//!
//! No second login: the agent (`ferostream-agent --stdio`) runs on an
//! exec channel of the pane's own SSH connection, in that connection's
//! sandboxed worker. When the host lacks the agent (or has another
//! version) and copying is on, the same channel carries ours over first;
//! with removal on, the agent deletes itself once its last session ends.
//!
//! What the host sends never reaches this process raw: it goes from the
//! worker straight to a decoder process of its own (`stream`, an
//! AppContainer with no capabilities), and only the decoder's checked,
//! unpacked messages reach the windows, which `ferostream_client::win32`
//! draws in FeroSSH's message loop. Input and copied text go back the
//! other way, through the worker.
//!
//! When the stream ends while the program still runs on the host (the
//! connection dropped), FeroStream keeps the windows and asks for a new
//! stream to the same session: [`AppHost::connect`] opens another exec
//! channel on the connection, once it is back.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

use ferossh_ipc::{ChannelId, FORWARD_IDS, MAX_DATA_CHUNK, Message};
use ferostream_client::link::Event;
use ferostream_client::transport::{AgentFile, AttachTo, Place, remote_command};
use ferostream_client::win32::{End, Events, Host, Transport};

use crate::link::Relays;
use crate::sandbox;
use crate::stream::{FromDecoder, MAX_IN, MAX_OUT, read_frame, write_frame};

/// Remote apps' exec channels: the top of the window's channel range,
/// which the window's own ids stay below.
pub const FIRST_ID: ChannelId = FORWARD_IDS - 0x0100_0000;
static NEXT_ID: AtomicU32 = AtomicU32::new(FIRST_ID);

/// Host bytes waiting for the decoder; a slow decoder slows the worker.
const BACKLOG: usize = 256;
/// The end of the agent's error output kept for the user.
const STDERR_KEEP: usize = 4096;

/// Sends to a connection's worker from any thread.
pub type SendFn = Arc<dyn Fn(Message) + Send + Sync>;

/// How to reach a connection's worker.
#[derive(Clone)]
pub struct Route {
    pub connection: ChannelId,
    pub send: SendFn,
    pub relays: Relays,
}

/// A stream running now: its exec channel, its connection, and the way to
/// end it when the connection's worker goes.
struct Running {
    channel: ChannelId,
    connection: ChannelId,
    end: SyncSender<Option<Vec<u8>>>,
}

static ACTIVE: Mutex<Vec<Running>> = Mutex::new(Vec::new());

fn active() -> std::sync::MutexGuard<'static, Vec<Running>> {
    ACTIVE.lock().unwrap_or_else(|p| p.into_inner())
}

/// The connections remote apps stream over now (their workers must stay).
pub fn connections_in_use() -> Vec<ChannelId> {
    active().iter().map(|r| r.connection).collect()
}

/// The worker of `connection` is gone: its streams end, and their sessions
/// wait to reconnect.
pub fn connection_gone(connection: ChannelId) {
    for r in active().iter() {
        if r.connection == connection {
            let _ = r.end.try_send(None);
        }
    }
}

fn next_channel() -> ChannelId {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    if id >= FORWARD_IDS - 1 {
        NEXT_ID.store(FIRST_ID, Ordering::SeqCst);
    }
    id.clamp(FIRST_ID, FORWARD_IDS - 1)
}

/// What a remote app session needs besides its route.
pub struct AppHost {
    route: Route,
    /// Whether `route` was just handed over (the first connect uses it as
    /// it is; later ones look the connection up again).
    fresh: bool,
    /// Finds the route to a connection again, `None` when it is closed.
    lookup: Box<dyn Fn(ChannelId) -> Option<Route>>,
    /// Agents to copy to the host; empty when copying is off.
    files: Vec<AgentFile>,
    place: Place,
    on_end: Box<dyn FnMut(End)>,
}

impl std::fmt::Debug for AppHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppHost")
            .field("connection", &self.route.connection)
            .field("place", &self.place)
            .finish_non_exhaustive()
    }
}

impl AppHost {
    pub fn new(
        route: Route,
        lookup: Box<dyn Fn(ChannelId) -> Option<Route>>,
        files: Vec<AgentFile>,
        place: Place,
        on_end: Box<dyn FnMut(End)>,
    ) -> AppHost {
        AppHost {
            route,
            fresh: true,
            lookup,
            files,
            place,
            on_end,
        }
    }
}

impl Host for AppHost {
    fn connect(&mut self, attach: &AttachTo, events: Events) -> Result<Box<dyn Transport>, String> {
        if !std::mem::take(&mut self.fresh) {
            self.route = (self.lookup)(self.route.connection)
                .ok_or("The SSH connection is closed; it comes back when FeroSSH reconnects.")?;
        }
        open(&self.route, attach, events, &self.files, &self.place)
    }

    fn ended(&mut self, end: End) {
        (self.on_end)(end);
    }
}

/// Messages for the agent, framed for FeroStream, on the exec channel.
struct AppLink {
    channel: ChannelId,
    send: SendFn,
}

impl std::fmt::Debug for AppLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppLink")
            .field("channel", &self.channel)
            .finish_non_exhaustive()
    }
}

impl Transport for AppLink {
    fn send(&self, m: ferostream_proto::Message) {
        send_bytes(&self.send, self.channel, &m.encode());
    }
}

impl Drop for AppLink {
    fn drop(&mut self) {
        // Closing the channel ends the agent's input: the session lives on
        // detached, or ends when its program has.
        (self.send)(Message::Close {
            channel: self.channel,
        });
    }
}

fn send_bytes(send: &SendFn, channel: ChannelId, bytes: &[u8]) {
    for chunk in bytes.chunks(MAX_DATA_CHUNK) {
        send(Message::Data {
            channel,
            bytes: chunk.to_vec(),
        });
    }
}

/// Starts a decoder and the agent on a new exec channel of `route`'s
/// connection.
fn open(
    route: &Route,
    attach: &AttachTo,
    events: Events,
    files: &[AgentFile],
    place: &Place,
) -> Result<Box<dyn Transport>, String> {
    let channel = next_channel();
    let (mut to_decoder, mut from_decoder, guard) = sandbox::spawn_stream()
        .map_err(|e| format!("Could not start the decoder: {}", e.message()))?
        .split();

    let (tx, rx) = sync_channel::<Option<Vec<u8>>>(BACKLOG);
    let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
    {
        let tx = tx.clone();
        let stderr = Arc::clone(&stderr);
        // On the worker link's reader thread: host bytes to the decoder.
        route.relays.insert(channel, move |m| match m {
            Message::Data { bytes, .. } => {
                let _ = tx.send(Some(bytes));
            }
            Message::ExecStderr { bytes, .. } => {
                let mut e = stderr.lock().unwrap_or_else(|p| p.into_inner());
                e.extend_from_slice(&bytes);
                if e.len() > STDERR_KEEP * 2 {
                    let cut = e.len() - STDERR_KEEP;
                    e.drain(..cut);
                }
            }
            Message::Close { .. } => {
                let _ = tx.send(None);
            }
            _ => {}
        });
    }
    active().push(Running {
        channel,
        connection: route.connection,
        end: tx,
    });

    // Host bytes to the decoder, until the channel or the worker ends.
    std::thread::spawn(move || {
        while let Ok(Some(bytes)) = rx.recv() {
            for chunk in bytes.chunks(MAX_IN) {
                if write_frame(&mut to_decoder, chunk).is_err() {
                    return;
                }
            }
        }
        // Dropping the pipe ends the decoder's input.
    });

    // The decoder's checked output to the windows.
    {
        let send = Arc::clone(&route.send);
        let relays = route.relays.clone();
        let files = files.to_vec();
        std::thread::spawn(move || {
            let error = loop {
                let frame = match read_frame(&mut from_decoder, MAX_OUT) {
                    Ok(Some(f)) => f,
                    Ok(None) => break None,
                    Err(e) => break Some(format!("The decoder's pipe failed: {e}.")),
                };
                match FromDecoder::decode(&frame) {
                    Ok(FromDecoder::Message(m)) => events.send(Event::Message(m)),
                    Ok(FromDecoder::Upload(arch)) => {
                        let Some(file) = files.iter().find(|f| f.arch == arch) else {
                            break Some(format!(
                                "The host ({arch}) needs an agent FeroSSH does not have: \
                                 put ferostream-agent-{arch} next to ferossh.exe."
                            ));
                        };
                        match std::fs::read(&file.path) {
                            Ok(bytes) if bytes.len() as u64 == file.size => {
                                send_bytes(&send, channel, &bytes);
                            }
                            Ok(_) => break Some("The agent file changed; try again.".into()),
                            Err(e) => {
                                break Some(format!("Could not read {}: {e}", file.path.display()));
                            }
                        }
                    }
                    Ok(FromDecoder::Failed(why)) => break Some(why),
                    Err(e) => {
                        break Some(format!("The decoder sent {e}; the remote app was stopped."));
                    }
                }
            };
            relays.remove(channel);
            send(Message::Close { channel });
            active().retain(|r| r.channel != channel);
            drop(guard);
            let tail = {
                let e = stderr.lock().unwrap_or_else(|p| p.into_inner());
                let start = e.len().saturating_sub(STDERR_KEEP);
                crate::ssh::sanitize(&String::from_utf8_lossy(e.get(start..).unwrap_or_default()))
                    .trim()
                    .to_string()
            };
            events.send(Event::Closed {
                error,
                stderr: tail,
            });
        });
    }

    (route.send)(Message::Exec {
        channel,
        connection: route.connection,
        command: remote_command(files, attach, place).into_bytes(),
        split_stderr: true,
    });
    Ok(Box::new(AppLink {
        channel,
        send: Arc::clone(&route.send),
    }))
}

/// The agents FeroSSH can copy: `ferostream-agent` (x86_64) and
/// `ferostream-agent-<uname -m>` next to ferossh.exe.
pub fn agent_files() -> Vec<AgentFile> {
    std::env::current_exe()
        .ok()
        .and_then(|me| {
            me.parent()
                .map(ferostream_client::transport::find_agent_files)
        })
        .unwrap_or_default()
}
