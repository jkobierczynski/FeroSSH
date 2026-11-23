//! Port forwarding and X11 on a logged-in connection.
//!
//! The UI holds the sockets, the worker the SSH channels; each forwarded
//! TCP connection is one stream channel between them (see the `ipc`
//! crate). Local and dynamic forwards start in the UI ([`open`]): a
//! program connected to a listener there, and the worker opens a
//! `direct-tcpip` channel, after reading the SOCKS request for a dynamic
//! forward. Remote forwards and X11 start at the server ([`incoming`]):
//! the server opens a channel for a port this connection asked it to
//! listen on ([`remote`]), or for an X client, and the UI connects it on.
//! Either end may be a Unix socket: `direct-streamlocal@openssh.com` to
//! one on the server, `streamlocal-forward@openssh.com` for the server to
//! listen on one.
//!
//! Untrusted bytes are parsed here, inside the sandbox: SOCKS requests
//! from local programs, SOCKS requests from programs on the server (a
//! remote forward without a target, `-R 1080`), and X11 connection
//! setups from the server, where the fake cookie the server was given is
//! checked. The real cookie never comes here: the UI checks the setup
//! again and swaps it in.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ferossh_forward::socks::{self, Handshake, Step};
use ferossh_forward::x11::{MAX_SETUP, MIT_COOKIE, Setup, swap_cookie};
use ferossh_ipc::{ChannelId, ForwardTarget, MAX_DATA_CHUNK, Message, STREAM_WINDOW};
use russh::client::Msg;
use russh::{Channel, ChannelMsg};
use tokio::sync::mpsc;

use crate::session::{Connection, Connections, RemoteListener, Route};

/// Data from the server held for a stream whose UI side is slow, before
/// the worker stops reading it. russh then holds the rest, which in the
/// end slows the whole connection: only a socket that stops reading for
/// this long gets there.
const MAX_HELD: usize = 4 << 20;
/// How long a SOCKS client or an X client may take over its handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a channel the server opened comes from.
pub(crate) enum Incoming {
    /// A connection to a port (or socket) a remote forward listens on;
    /// with `socks` it asks where to go first.
    Remote {
        listener: ChannelId,
        socks: bool,
        origin: (String, u32),
    },
    /// An X client.
    X11 {
        connection: ChannelId,
        fake: [u8; 16],
        origin: (String, u32),
    },
}

/// Why a stream ended early; the text is for the user.
type Ended = Result<(), String>;

/// One stream's side of the pipe: flow control and the UI's early data.
struct Stream {
    id: ChannelId,
    out: mpsc::Sender<Message>,
    inbox: mpsc::UnboundedReceiver<Message>,
    /// Bytes the worker may still send the UI.
    credit: u32,
    /// What the UI sent before the channel was open.
    early: Vec<u8>,
    ui_eof: bool,
}

/// What came from the UI while waiting.
enum FromUi {
    Data(Vec<u8>),
    Gone,
}

impl Stream {
    fn new(
        id: ChannelId,
        out: mpsc::Sender<Message>,
        inbox: mpsc::UnboundedReceiver<Message>,
    ) -> Self {
        Self {
            id,
            out,
            inbox,
            credit: STREAM_WINDOW,
            early: Vec::new(),
            ui_eof: false,
        }
    }

    async fn send(&self, msg: Message) -> Ended {
        self.out.send(msg).await.map_err(|_| String::new())
    }

    /// Sends handshake replies, within the credit.
    async fn reply(&mut self, bytes: Vec<u8>) -> Ended {
        if bytes.is_empty() {
            return Ok(());
        }
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        self.credit = self.credit.saturating_sub(len);
        self.send(Message::Data {
            channel: self.id,
            bytes,
        })
        .await
    }

    /// Gives the UI credit for bytes taken off the stream.
    async fn consumed(&self, n: usize) -> Ended {
        let bytes = u32::try_from(n).unwrap_or(u32::MAX);
        if bytes == 0 {
            return Ok(());
        }
        let channel = self.id;
        self.send(Message::Window { channel, bytes }).await
    }

    /// The next message from the UI that matters before the channel is
    /// open: data, or the UI closing the stream.
    async fn next_from_ui(&mut self) -> FromUi {
        loop {
            match self.inbox.recv().await {
                Some(Message::Data { bytes, .. }) => return FromUi::Data(bytes),
                Some(Message::Window { bytes, .. }) => {
                    self.credit = self.credit.saturating_add(bytes).min(STREAM_WINDOW);
                }
                Some(Message::Eof { .. }) => self.ui_eof = true,
                Some(Message::Close { .. }) | None => return FromUi::Gone,
                Some(_) => {}
            }
        }
    }

    /// Waits for the UI's word that it connected a remote SOCKS stream:
    /// `false` when it closed the stream instead.
    async fn connected(&mut self) -> bool {
        loop {
            match self.inbox.recv().await {
                Some(Message::ForwardConnected { .. }) => return true,
                Some(Message::Window { bytes, .. }) => {
                    self.credit = self.credit.saturating_add(bytes).min(STREAM_WINDOW);
                }
                Some(Message::Eof { .. }) => self.ui_eof = true,
                Some(Message::Close { .. }) | None => return false,
                // Nothing comes from the target before it is connected.
                Some(_) => return false,
            }
        }
    }

    /// Waits for `fut` while keeping what the UI sends meanwhile.
    async fn wait<T>(&mut self, fut: impl Future<Output = T>) -> Result<T, String> {
        tokio::pin!(fut);
        loop {
            tokio::select! {
                biased;
                out = &mut fut => return Ok(out),
                msg = self.next_from_ui() => match msg {
                    FromUi::Data(bytes) => {
                        if self.early.len() + bytes.len() > STREAM_WINDOW as usize {
                            return Err("The UI overran the stream window.".into());
                        }
                        self.early.extend(bytes);
                    }
                    FromUi::Gone => return Err(String::new()),
                },
            }
        }
    }

    /// Reads a SOCKS request; returns the target, with any early data
    /// after the request kept in `early`.
    async fn socks(&mut self) -> Result<(String, u16, Handshake), String> {
        let mut hs = Handshake::new();
        let mut input = std::mem::take(&mut self.early);
        loop {
            let fed = input.len();
            let step = hs.feed(&input);
            input.clear();
            match step {
                Step::More { reply } => {
                    self.consumed(fed).await?;
                    self.reply(reply).await?;
                }
                Step::Connect { host, port, rest } => {
                    self.consumed(fed - rest.len()).await?;
                    self.early = rest;
                    return Ok((host, port, hs));
                }
                Step::Refuse { reply } => {
                    self.reply(reply).await?;
                    return Err("The program sent a SOCKS request FeroSSH does not support.".into());
                }
            }
            if self.ui_eof {
                return Err(String::new());
            }
            match self.next_from_ui().await {
                FromUi::Data(bytes) => input = bytes,
                FromUi::Gone => return Err(String::new()),
            }
        }
    }

    /// Pumps data both ways until either side closes; `held` is server
    /// data read before the UI knew of the stream.
    async fn relay(mut self, channel: Channel<Msg>, mut held: Vec<u8>) -> Ended {
        let (mut read, write) = channel.split();
        // Writes wait for the server's window; they run in their own task
        // so data keeps flowing the other way meanwhile. Credit goes back
        // to the UI once the bytes are written.
        let (wtx, mut wrx) = mpsc::unbounded_channel::<Option<Vec<u8>>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let (out, id, q) = (self.out.clone(), self.id, queued.clone());
        let writer = tokio::spawn(async move {
            while let Some(item) = wrx.recv().await {
                match item {
                    Some(bytes) => {
                        let n = bytes.len();
                        if write.data(&bytes[..]).await.is_err() {
                            break;
                        }
                        q.fetch_sub(n, Ordering::Relaxed);
                        let bytes = u32::try_from(n).unwrap_or(u32::MAX);
                        let _ = out.send(Message::Window { channel: id, bytes }).await;
                    }
                    None => {
                        let _ = write.eof().await;
                    }
                }
            }
            // russh does not close a channel that is dropped.
            let _ = write.close().await;
        });
        let send = |bytes: Vec<u8>| {
            queued.fetch_add(bytes.len(), Ordering::Relaxed);
            wtx.send(Some(bytes)).is_ok()
        };
        let early = std::mem::take(&mut self.early);
        if !early.is_empty() {
            send(early);
        }
        if self.ui_eof {
            let _ = wtx.send(None);
        }

        let (mut server_eof, mut eof_sent, mut closed) = (false, false, false);
        let result = loop {
            while self.credit > 0 && !held.is_empty() {
                let n = held.len().min(self.credit as usize).min(MAX_DATA_CHUNK);
                let bytes: Vec<u8> = held.drain(..n).collect();
                self.credit -= u32::try_from(n).unwrap_or(0);
                if self
                    .send(Message::Data { channel: id, bytes })
                    .await
                    .is_err()
                {
                    closed = true;
                    held.clear();
                }
            }
            if held.is_empty() && server_eof && !eof_sent {
                eof_sent = true;
                let _ = self.send(Message::Eof { channel: id }).await;
            }
            if closed && held.is_empty() {
                break Ok(());
            }
            let can_read = !closed && held.len() < MAX_HELD;
            tokio::select! {
                msg = read.wait(), if can_read => match msg {
                    Some(ChannelMsg::Data { data }) => held.extend_from_slice(&data),
                    Some(ChannelMsg::Eof) => server_eof = true,
                    Some(ChannelMsg::Close) | None => closed = true,
                    Some(_) => {}
                },
                msg = self.inbox.recv() => match msg {
                    Some(Message::Data { bytes, .. }) => {
                        if queued.load(Ordering::Relaxed) + bytes.len() > STREAM_WINDOW as usize {
                            break Err("The UI overran the stream window.".into());
                        }
                        if !send(bytes) {
                            break Ok(());
                        }
                    }
                    Some(Message::Window { bytes, .. }) => {
                        self.credit = self.credit.saturating_add(bytes).min(STREAM_WINDOW);
                    }
                    Some(Message::Eof { .. }) => {
                        let _ = wtx.send(None);
                    }
                    Some(Message::Close { .. }) | None => break Ok(()),
                    Some(_) => {}
                },
            }
        };
        // Pending writes still go out, then the writer closes the channel.
        drop(wtx);
        drop(read);
        let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
        result
    }
}

fn route(connections: &Connections, id: ChannelId) -> Option<mpsc::UnboundedReceiver<Message>> {
    let (tx, rx) = mpsc::unbounded_channel();
    connections
        .routes
        .insert(id, Route::Stream(tx))
        .then_some(rx)
}

/// Ends a stream: tells the UI why, and forgets the route.
async fn finish(
    id: ChannelId,
    result: Ended,
    out: &mpsc::Sender<Message>,
    connections: &Connections,
) {
    connections.routes.remove(id);
    let reason = result.err().unwrap_or_default();
    let _ = out
        .send(Message::Disconnected {
            channel: id,
            reason,
        })
        .await;
}

/// A program connected to a local or dynamic forward's listener: opens the
/// `direct-tcpip` channel (after the SOCKS request, without a target) and
/// relays. A stream that `keep`s its connection holds it until the stream
/// ends, and disconnects it then if nothing else holds it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn open(
    channel: ChannelId,
    connection: ChannelId,
    target: Option<ForwardTarget>,
    origin: (String, u16),
    keep: bool,
    inbox: mpsc::UnboundedReceiver<Message>,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let mut stream = Stream::new(channel, out.clone(), inbox);
    let result = open_stream(&mut stream, connection, target, origin, keep, &connections).await;
    let result = match result {
        Ok((ch, held)) => {
            let ended = stream.relay(ch, Vec::new()).await;
            if let Some(conn) = held {
                crate::sftp::release(conn).await;
            }
            ended
        }
        Err(e) => Err(e),
    };
    finish(channel, result, &out, &connections).await;
}

async fn open_stream(
    stream: &mut Stream,
    connection: ChannelId,
    target: Option<ForwardTarget>,
    origin: (String, u16),
    keep: bool,
    connections: &Connections,
) -> Result<(Channel<Msg>, Option<Arc<Connection>>), String> {
    let (target, socks) = match target {
        Some(target) => (target, None),
        None => {
            let (host, port, hs) = tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.socks())
                .await
                .map_err(|_| "The program took too long over its SOCKS request.".to_owned())??;
            (ForwardTarget::Tcp(host, port), Some(hs))
        }
    };
    let Some((conn, ..)) = connections.get(connection) else {
        if let Some(hs) = &socks {
            stream
                .reply(hs.finish(socks::reply::GENERAL_FAILURE))
                .await?;
        }
        return Err("That connection is closed.".into());
    };
    let opening = async {
        match &target {
            ForwardTarget::Tcp(host, port) => {
                let open = conn.channel_open_direct_tcpip(
                    host.clone(),
                    (*port).into(),
                    origin.0,
                    origin.1.into(),
                );
                tokio::time::timeout(OPEN_TIMEOUT, open).await
            }
            ForwardTarget::Unix(path) => {
                let open = conn.channel_open_direct_streamlocal(path.clone());
                tokio::time::timeout(OPEN_TIMEOUT, open).await
            }
        }
    };
    let opened = stream.wait(opening).await;
    // Only a stream asked to keep the connection open holds it.
    let held = keep.then_some(conn);
    let (host, port) = match target {
        ForwardTarget::Tcp(host, port) => (host, port),
        // The UI shows a socket by its path, with no port.
        ForwardTarget::Unix(path) => (path, 0),
    };
    let failed = |why: String| {
        if port == 0 {
            format!("{host}: {why}")
        } else {
            format!("{host}:{port}: {why}")
        }
    };
    let ch = match opened? {
        Ok(Ok(ch)) => ch,
        Ok(Err(e)) => {
            if let Some(hs) = &socks {
                stream
                    .reply(hs.finish(socks::reply::HOST_UNREACHABLE))
                    .await?;
            }
            return Err(failed(match e {
                russh::Error::ChannelOpenFailure(
                    russh::ChannelOpenFailure::AdministrativelyProhibited
                    | russh::ChannelOpenFailure::UnknownChannelType,
                ) => "the server does not forward connections (AllowTcpForwarding no, a \
                      restrict option on the key, or a server without forwarding, such as \
                      tinysshd)."
                    .to_owned(),
                russh::Error::ChannelOpenFailure(why) => {
                    format!("the server could not connect ({why:?}).")
                }
                e => format!("{e}."),
            }));
        }
        Err(_) => {
            if let Some(hs) = &socks {
                stream
                    .reply(hs.finish(socks::reply::GENERAL_FAILURE))
                    .await?;
            }
            return Err(failed("the server did not answer.".into()));
        }
    };
    if let Some(hs) = &socks {
        stream.reply(hs.finish(socks::reply::SUCCEEDED)).await?;
    }
    stream
        .send(Message::ForwardOpened {
            channel: stream.id,
            host,
            port,
        })
        .await?;
    Ok((ch, held))
}

/// The server opened a channel for a remote forward or for X11: checks an
/// X client's cookie, tells the UI, and relays.
pub(crate) async fn incoming(
    mut channel: Channel<Msg>,
    from: Incoming,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let id = connections.new_id();
    let Some(inbox) = route(&connections, id) else {
        return;
    };
    let mut socks = None;
    let (listener, x11, origin, held) = match from {
        Incoming::Remote {
            listener,
            socks: false,
            origin,
        } => (listener, false, origin, Vec::new()),
        Incoming::Remote {
            listener,
            socks: true,
            origin,
        } => {
            let request = tokio::time::timeout(HANDSHAKE_TIMEOUT, server_socks(&mut channel));
            match request.await {
                Ok(Some((host, port, hs, rest))) => {
                    socks = Some((host, port, hs));
                    (listener, false, origin, rest)
                }
                // Not SOCKS, something unsupported, or too slow: nothing
                // on this side hears of it.
                _ => {
                    connections.routes.remove(id);
                    let _ = channel.close().await;
                    return;
                }
            }
        }
        Incoming::X11 {
            connection,
            fake,
            origin,
        } => {
            let setup = tokio::time::timeout(HANDSHAKE_TIMEOUT, x11_setup(&mut channel, &fake));
            match setup.await {
                Ok(Some(bytes)) => (connection, true, origin, bytes),
                // A wrong cookie, junk, or too slow: the X server never
                // hears of it.
                _ => {
                    connections.routes.remove(id);
                    let _ = channel.close().await;
                    return;
                }
            }
        }
    };
    let msg = Message::ForwardIncoming {
        channel: id,
        listener,
        x11,
        origin_host: origin.0,
        origin_port: u16::try_from(origin.1).unwrap_or(0),
        target: socks.as_ref().map(|(h, p, _)| (h.clone(), *p)),
    };
    if out.send(msg).await.is_err() {
        connections.routes.remove(id);
        return;
    }
    let mut stream = Stream::new(id, out.clone(), inbox);
    if let Some((host, port, hs)) = socks {
        // The UI connects to the target first: only then does the
        // client hear that it may talk.
        let connected = tokio::time::timeout(OPEN_TIMEOUT, stream.connected()).await;
        let ok = matches!(connected, Ok(true));
        let code = if ok {
            socks::reply::SUCCEEDED
        } else {
            socks::reply::HOST_UNREACHABLE
        };
        let replied = channel.data(&hs.finish(code)[..]).await.is_ok();
        if !ok || !replied {
            let _ = channel.close().await;
            let reason = if ok {
                String::new()
            } else {
                format!("{}: this PC could not connect.", target_text(&host, port))
            };
            finish(id, Err(reason), &out, &connections).await;
            return;
        }
    }
    let result = stream.relay(channel, held).await;
    finish(id, result, &out, &connections).await;
}

fn target_text(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Reads a SOCKS request from a program on the server, through a remote
/// forward without a target: the host, the port, the handshake (for the
/// final reply), and whatever the program sent after the request.
/// `None` for anything that is not a SOCKS CONNECT FeroSSH supports,
/// after telling the program so.
async fn server_socks(channel: &mut Channel<Msg>) -> Option<(String, u16, Handshake, Vec<u8>)> {
    let mut hs = Handshake::new();
    loop {
        let data = match channel.wait().await? {
            ChannelMsg::Data { data } => data,
            ChannelMsg::Eof | ChannelMsg::Close => return None,
            _ => continue,
        };
        match hs.feed(&data) {
            Step::More { reply } => {
                if !reply.is_empty() {
                    channel.data(&reply[..]).await.ok()?;
                }
            }
            Step::Connect { host, port, rest } => return Some((host, port, hs, rest)),
            Step::Refuse { reply } => {
                if !reply.is_empty() {
                    let _ = channel.data(&reply[..]).await;
                }
                return None;
            }
        }
    }
}

/// Reads an X client's connection setup and returns it as it is, or
/// `None` for a wrong cookie. The UI checks it again and swaps in the real
/// authorisation, which this process never has: this check only keeps
/// junk and wrong guesses from the server off the pipe.
async fn x11_setup(channel: &mut Channel<Msg>, fake: &[u8]) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    loop {
        match channel.wait().await? {
            ChannelMsg::Data { data } => buf.extend_from_slice(&data),
            ChannelMsg::Eof | ChannelMsg::Close => return None,
            _ => continue,
        }
        match swap_cookie(&buf, fake, MIT_COOKIE, fake) {
            Setup::Ready(bytes) => return Some(bytes),
            Setup::Refuse => return None,
            Setup::More if buf.len() > MAX_SETUP => return None,
            Setup::More => {}
        }
    }
}

/// What a remote forward asks the server to listen on.
#[derive(Debug, Clone)]
pub(crate) enum Listen {
    /// An address and a port; with `socks` each connection asks where to
    /// go.
    Port {
        bind: String,
        port: u16,
        socks: bool,
    },
    /// A Unix socket on the server.
    Socket(String),
}

/// Asks the server to listen for a remote forward, and keeps the listener
/// until the UI closes it or the connection ends.
pub(crate) async fn remote(
    channel: ChannelId,
    connection: ChannelId,
    listen: Listen,
    mut inbox: mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let reason = match connections.get(connection) {
        None => "That connection is closed.".to_owned(),
        Some((conn, _, forwards)) => {
            let weak = Arc::downgrade(&conn);
            let mut alive = forwards.alive();
            let asked = tokio::select! {
                r = async {
                    match &listen {
                        Listen::Port { bind, port, .. } => {
                            conn.tcpip_forward(bind.clone(), (*port).into()).await
                        }
                        Listen::Socket(path) => {
                            conn.streamlocal_forward(path.clone()).await.map(|()| 0)
                        }
                    }
                } => Some(r),
                _ = inbox.recv() => None,
            };
            drop(conn);
            match asked {
                None => String::new(),
                Some(Err(_)) => match &listen {
                    Listen::Port { bind, port, .. } => format!(
                        "The server refused to listen on {}:{port} (the port may be taken, or \
                         the server forbids remote forwarding).",
                        if bind.is_empty() { "*" } else { bind }
                    ),
                    Listen::Socket(path) => format!(
                        "The server refused to listen on {path} (the socket may exist already, \
                         its folder may be missing, or the server forbids socket forwarding)."
                    ),
                },
                Some(Ok(chosen)) => {
                    let (registered, port) = match &listen {
                        Listen::Port { bind, port, socks } => {
                            let bound = if *port == 0 { chosen } else { (*port).into() };
                            (
                                RemoteListener::Port {
                                    bind: bind.clone(),
                                    port: bound,
                                    socks: *socks,
                                    listener: channel,
                                },
                                u16::try_from(bound).unwrap_or(0),
                            )
                        }
                        Listen::Socket(path) => (
                            RemoteListener::Socket {
                                path: path.clone(),
                                listener: channel,
                            },
                            0,
                        ),
                    };
                    forwards.add_remote(registered);
                    let listening = Message::ForwardListening { channel, port };
                    let reason = if out.send(listening).await.is_err() {
                        String::new()
                    } else {
                        loop {
                            tokio::select! {
                                msg = inbox.recv() => match msg {
                                    Some(Message::Close { .. }) | None => break String::new(),
                                    Some(_) => {}
                                },
                                _ = alive.changed() => break "Connection closed.".to_owned(),
                            }
                        }
                    };
                    forwards.remove_remote(channel);
                    if let Some(conn) = weak.upgrade() {
                        let cancel = async {
                            match &listen {
                                Listen::Port { bind, .. } => {
                                    conn.cancel_tcpip_forward(bind.clone(), port.into()).await
                                }
                                Listen::Socket(path) => {
                                    conn.cancel_streamlocal_forward(path.clone()).await
                                }
                            }
                        };
                        let _ = tokio::time::timeout(Duration::from_secs(5), cancel).await;
                    }
                    reason
                }
            }
        }
    };
    connections.routes.remove(channel);
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}
