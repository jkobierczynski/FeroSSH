//! The UI process's half of port and X11 forwarding: listeners for local
//! and dynamic forwards, and connections to the targets of remote forwards
//! and to the X server.
//!
//! The sandboxed worker cannot use loopback, so every socket on this PC is
//! here; the worker holds the SSH channels, and each forwarded connection
//! is a stream channel on the pipe between them, with credit-based flow
//! control (see the `ipc` crate). This side never parses what flows
//! through: SOCKS requests and X11 setups go to the worker as they are.
//!
//! [`Forwarder`] runs on its own thread with a small tokio runtime, so a
//! busy forward never slows the window. The window starts and stops a
//! connection's forwards, and reads a [`ConnView`] snapshot for the
//! forwards list and the tab badges. Messages from the worker for forward
//! channels skip the window: the link hands them straight to
//! [`Forwarder::sink`].

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferossh_config::{ForwardKind, ForwardSpec};
use ferossh_forward::x11::Display;
use ferossh_ipc::{ChannelId, FORWARD_IDS, MAX_DATA_CHUNK, Message, STREAM_WINDOW, WORKER_IDS};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::AbortHandle;

/// How long connecting to a remote forward's target or the X server may
/// take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Open connections listed per forward in the view.
const LISTED_STREAMS: usize = 8;

/// What a connection forwards, from its ssh_config.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub forwards: Vec<ForwardSpec>,
    /// `GatewayPorts yes`: local listeners without an address listen on
    /// every interface.
    pub gateway_ports: bool,
    /// X11 forwarding to this display.
    pub x11: Option<Display>,
}

/// Where a forward stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FwdState {
    Starting,
    /// Listening: the local addresses, or the server's port.
    Listening(String),
    Failed(String),
}

/// One forward, for the forwards list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FwdView {
    pub spec: ForwardSpec,
    pub state: FwdState,
    /// Connections open now, and since the forward started.
    pub open: usize,
    pub total: u64,
    /// Some of the open connections: `from → to`.
    pub streams: Vec<String>,
    /// Why the last connection failed, if it did.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11View {
    pub display: String,
    pub open: usize,
    pub total: u64,
    pub error: Option<String>,
}

/// One connection's forwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnView {
    pub connection: ChannelId,
    pub label: String,
    pub forwards: Vec<FwdView>,
    pub x11: Option<X11View>,
}

impl ConnView {
    /// Remote forwards or X11 are on: the server can reach into this side.
    pub fn inbound(&self) -> bool {
        self.x11.is_some()
            || self
                .forwards
                .iter()
                .any(|f| f.spec.kind == ForwardKind::Remote)
    }
}

enum Cmd {
    Start {
        connection: ChannelId,
        label: String,
        setup: Setup,
    },
    Add {
        connection: ChannelId,
        spec: ForwardSpec,
    },
    Remove {
        connection: ChannelId,
        index: usize,
    },
    Stop {
        connection: ChannelId,
    },
    FromWorker(Message),
    WorkerGone,
}

#[derive(Default)]
struct Shared {
    view: Mutex<Vec<ConnView>>,
    changed: AtomicBool,
}

/// The window's handle on the forwarding thread.
#[derive(Clone)]
pub struct Forwarder {
    cmds: UnboundedSender<Cmd>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Forwarder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forwarder").finish_non_exhaustive()
    }
}

/// The forwarding engine before it runs; see [`Forwarder::new`].
pub struct Runner {
    cmds: UnboundedReceiver<Cmd>,
    shared: Arc<Shared>,
}

impl Forwarder {
    /// The handle and the engine. The handle's [`sink`](Self::sink) goes
    /// to the link before the link starts; the engine then runs with the
    /// link's sender.
    pub fn new() -> (Self, Runner) {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared::default());
        (
            Self {
                cmds: tx,
                shared: shared.clone(),
            },
            Runner { cmds: rx, shared },
        )
    }

    fn cmd(&self, cmd: Cmd) {
        let _ = self.cmds.send(cmd);
    }

    /// Takes a message from the worker for a forward channel.
    pub fn sink(&self) -> impl Fn(Message) + Send + 'static {
        let tx = self.cmds.clone();
        move |msg| {
            let _ = tx.send(Cmd::FromWorker(msg));
        }
    }

    /// Starts the forwards of a connection that just logged in.
    pub fn start(&self, connection: ChannelId, label: String, setup: Setup) {
        self.cmd(Cmd::Start {
            connection,
            label,
            setup,
        });
    }

    /// Adds a forward to a running connection.
    pub fn add(&self, connection: ChannelId, spec: ForwardSpec) {
        self.cmd(Cmd::Add { connection, spec });
    }

    /// Stops the connection's `index`th forward and its connections.
    pub fn remove(&self, connection: ChannelId, index: usize) {
        self.cmd(Cmd::Remove { connection, index });
    }

    /// Stops everything the connection forwards.
    pub fn stop(&self, connection: ChannelId) {
        self.cmd(Cmd::Stop { connection });
    }

    /// The worker is gone: everything stops.
    pub fn worker_gone(&self) {
        self.cmd(Cmd::WorkerGone);
    }

    /// The forwards of every connection, by connection.
    pub fn view(&self) -> Vec<ConnView> {
        self.shared
            .view
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Whether the view changed since the last call.
    pub fn take_changed(&self) -> bool {
        self.shared.changed.swap(false, Ordering::AcqRel)
    }
}

impl Runner {
    /// Runs the engine on its own thread. `to_worker` sends to the worker;
    /// `notify` runs when the view changed.
    pub fn spawn(
        self,
        to_worker: impl Fn(Message) + Send + Sync + 'static,
        notify: impl Fn() + Send + 'static,
    ) -> std::io::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        std::thread::Builder::new()
            .name("forwards".into())
            .spawn(move || rt.block_on(self.run(to_worker, notify)))?;
        Ok(())
    }

    /// Runs the engine until the window's handle is gone.
    pub async fn run(
        mut self,
        to_worker: impl Fn(Message) + Send + Sync + 'static,
        notify: impl Fn() + Send + 'static,
    ) {
        let (internal_tx, mut internal) = mpsc::unbounded_channel();
        let mut engine = Engine {
            to_worker: Arc::new(to_worker),
            internal: internal_tx,
            conns: BTreeMap::new(),
            streams: HashMap::new(),
            next_id: FORWARD_IDS,
        };
        let mut last: Vec<ConnView> = Vec::new();
        loop {
            tokio::select! {
                cmd = self.cmds.recv() => match cmd {
                    Some(cmd) => engine.cmd(cmd),
                    None => break,
                },
                Some(ev) = internal.recv() => engine.internal(ev),
            }
            let view = engine.view();
            if view != last {
                if let Ok(mut v) = self.shared.view.lock() {
                    v.clone_from(&view);
                }
                last = view;
                self.shared.changed.store(true, Ordering::Release);
                notify();
            }
        }
        engine.stop_all();
    }
}

/// What a stream belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Owner {
    /// A forward of a connection, by its key.
    Fwd(ChannelId, u64),
    /// The connection's X11.
    X11(ChannelId),
}

impl Owner {
    fn connection(self) -> ChannelId {
        match self {
            Owner::Fwd(c, _) | Owner::X11(c) => c,
        }
    }
}

struct Fwd {
    key: u64,
    spec: ForwardSpec,
    state: FwdState,
    listeners: Vec<AbortHandle>,
    /// The worker's listener channel of a remote forward.
    remote: Option<ChannelId>,
    total: u64,
    error: Option<String>,
}

struct X11 {
    display: Display,
    total: u64,
    error: Option<String>,
}

struct Conn {
    label: String,
    gateway_ports: bool,
    forwards: Vec<Fwd>,
    x11: Option<X11>,
    next_key: u64,
}

/// What the window's side of a stream does next.
enum ToSocket {
    Data(Vec<u8>),
    Eof,
    Window(u32),
    /// The worker ended the stream.
    Close,
}

struct StreamRef {
    owner: Owner,
    tx: UnboundedSender<ToSocket>,
    /// `from → to`, for the view.
    label: String,
}

/// Events from the engine's own tasks.
enum Internal {
    Listening {
        owner: Owner,
        addrs: String,
    },
    ListenFailed {
        owner: Owner,
        why: String,
    },
    Accepted {
        owner: Owner,
        socket: TcpStream,
        peer: SocketAddr,
    },
    /// A stream ended on this side; `why` when it failed.
    Ended {
        channel: ChannelId,
        why: Option<String>,
    },
}

type ToWorker = Arc<dyn Fn(Message) + Send + Sync>;

struct Engine {
    to_worker: ToWorker,
    internal: UnboundedSender<Internal>,
    conns: BTreeMap<ChannelId, Conn>,
    streams: HashMap<ChannelId, StreamRef>,
    next_id: ChannelId,
}

/// The addresses a local listener binds, as `ssh` chooses them.
fn bind_hosts(bind: Option<&str>, gateway_ports: bool) -> Vec<String> {
    let all = || vec!["0.0.0.0".to_owned(), "::".to_owned()];
    let loopback = || vec!["127.0.0.1".to_owned(), "::1".to_owned()];
    match bind {
        None if gateway_ports => all(),
        None => loopback(),
        Some("" | "*") => all(),
        Some(h) if h.eq_ignore_ascii_case("localhost") => loopback(),
        Some(h) => vec![h.to_owned()],
    }
}

/// The address a remote forward asks the server to listen on, as `ssh`
/// sends it.
fn remote_bind(bind: Option<&str>) -> String {
    match bind {
        None => "localhost".to_owned(),
        Some("*") => String::new(),
        Some(h) => h.to_owned(),
    }
}

fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

impl Engine {
    fn send(&self, msg: Message) {
        (self.to_worker)(msg);
    }

    fn new_id(&mut self) -> ChannelId {
        loop {
            let id = self.next_id;
            self.next_id = if id + 1 >= WORKER_IDS {
                FORWARD_IDS
            } else {
                id + 1
            };
            let remote_in_use = self
                .conns
                .values()
                .any(|c| c.forwards.iter().any(|f| f.remote == Some(id)));
            if !self.streams.contains_key(&id) && !remote_in_use {
                return id;
            }
        }
    }

    fn fwd_mut(&mut self, owner: Owner) -> Option<&mut Fwd> {
        let Owner::Fwd(conn, key) = owner else {
            return None;
        };
        self.conns
            .get_mut(&conn)?
            .forwards
            .iter_mut()
            .find(|f| f.key == key)
    }

    fn cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Start {
                connection,
                label,
                setup,
            } => {
                self.stop(connection);
                self.conns.insert(
                    connection,
                    Conn {
                        label,
                        gateway_ports: setup.gateway_ports,
                        forwards: Vec::new(),
                        x11: setup.x11.map(|display| X11 {
                            display,
                            total: 0,
                            error: None,
                        }),
                        next_key: 0,
                    },
                );
                for spec in setup.forwards {
                    self.add(connection, spec);
                }
            }
            Cmd::Add { connection, spec } => self.add(connection, spec),
            Cmd::Remove { connection, index } => {
                let Some(conn) = self.conns.get_mut(&connection) else {
                    return;
                };
                if index < conn.forwards.len() {
                    let fwd = conn.forwards.remove(index);
                    self.end_fwd(connection, fwd);
                }
            }
            Cmd::Stop { connection } => self.stop(connection),
            Cmd::FromWorker(msg) => self.on_worker(msg),
            Cmd::WorkerGone => self.stop_all(),
        }
    }

    fn stop_all(&mut self) {
        let conns: Vec<ChannelId> = self.conns.keys().copied().collect();
        for c in conns {
            self.stop(c);
        }
    }

    fn stop(&mut self, connection: ChannelId) {
        let Some(conn) = self.conns.remove(&connection) else {
            return;
        };
        for fwd in conn.forwards {
            self.end_fwd(connection, fwd);
        }
        self.end_streams(|o| o == Owner::X11(connection));
    }

    /// Stops a forward's listeners and connections.
    fn end_fwd(&mut self, connection: ChannelId, fwd: Fwd) {
        for l in &fwd.listeners {
            l.abort();
        }
        if let Some(remote) = fwd.remote {
            self.send(Message::Close { channel: remote });
        }
        self.end_streams(|o| o == Owner::Fwd(connection, fwd.key));
    }

    fn end_streams(&mut self, mut which: impl FnMut(Owner) -> bool) {
        let ids: Vec<ChannelId> = self
            .streams
            .iter()
            .filter(|(_, s)| which(s.owner))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(s) = self.streams.remove(&id) {
                let _ = s.tx.send(ToSocket::Close);
                self.send(Message::Close { channel: id });
            }
        }
    }

    fn add(&mut self, connection: ChannelId, spec: ForwardSpec) {
        let Some(conn) = self.conns.get_mut(&connection) else {
            return;
        };
        if conn.forwards.iter().any(|f| f.spec == spec) {
            return;
        }
        let key = conn.next_key;
        conn.next_key += 1;
        let owner = Owner::Fwd(connection, key);
        let mut fwd = Fwd {
            key,
            spec: spec.clone(),
            state: FwdState::Starting,
            listeners: Vec::new(),
            remote: None,
            total: 0,
            error: None,
        };
        match spec.kind {
            ForwardKind::Local | ForwardKind::Dynamic => {
                let hosts = bind_hosts(spec.bind.as_deref(), conn.gateway_ports);
                let task = tokio::spawn(listen(hosts, spec.port, owner, self.internal.clone()));
                fwd.listeners.push(task.abort_handle());
            }
            ForwardKind::Remote => {
                let id = self.new_id();
                fwd.remote = Some(id);
                self.send(Message::RemoteForward {
                    channel: id,
                    connection,
                    bind: remote_bind(spec.bind.as_deref()),
                    port: spec.port,
                });
            }
        }
        if let Some(conn) = self.conns.get_mut(&connection) {
            conn.forwards.push(fwd);
        }
    }

    fn internal(&mut self, ev: Internal) {
        match ev {
            Internal::Listening { owner, addrs } => {
                if let Some(f) = self.fwd_mut(owner) {
                    f.state = FwdState::Listening(addrs);
                }
            }
            Internal::ListenFailed { owner, why } => {
                if let Some(f) = self.fwd_mut(owner) {
                    f.state = FwdState::Failed(why);
                }
            }
            Internal::Accepted {
                owner,
                socket,
                peer,
            } => self.accepted(owner, socket, peer),
            Internal::Ended { channel, why } => {
                if let Some(s) = self.streams.remove(&channel) {
                    // The worker may still think it open.
                    self.send(Message::Close { channel });
                    if let Some(why) = why {
                        self.note_error(s.owner, why);
                    }
                }
            }
        }
    }

    fn note_error(&mut self, owner: Owner, why: String) {
        match owner {
            Owner::Fwd(..) => {
                if let Some(f) = self.fwd_mut(owner) {
                    f.error = Some(why);
                }
            }
            Owner::X11(c) => {
                if let Some(x) = self.conns.get_mut(&c).and_then(|c| c.x11.as_mut()) {
                    x.error = Some(why);
                }
            }
        }
    }

    fn accepted(&mut self, owner: Owner, socket: TcpStream, peer: SocketAddr) {
        let Some(fwd) = self.fwd_mut(owner) else {
            return;
        };
        fwd.total += 1;
        let target = fwd.spec.target.clone();
        let label = match &target {
            Some((h, p)) => format!("{peer} → {}", host_port(h, *p)),
            None => format!("{peer} → SOCKS"),
        };
        let id = self.new_id();
        let (tx, rx) = mpsc::unbounded_channel();
        self.streams.insert(id, StreamRef { owner, tx, label });
        self.send(Message::ForwardOpen {
            channel: id,
            connection: owner.connection(),
            target,
            origin_host: peer.ip().to_string(),
            origin_port: peer.port(),
        });
        tokio::spawn(relay(
            id,
            Ok(socket),
            rx,
            self.to_worker.clone(),
            self.internal.clone(),
        ));
    }

    fn on_worker(&mut self, msg: Message) {
        let Some(channel) = msg.channel() else {
            return;
        };
        if let Some(s) = self.streams.get_mut(&channel) {
            let to_socket = match msg {
                Message::Data { bytes, .. } => ToSocket::Data(bytes),
                Message::Eof { .. } => ToSocket::Eof,
                Message::Window { bytes, .. } => ToSocket::Window(bytes),
                Message::ForwardOpened { host, port, .. } => {
                    if let Some((from, _)) = s.label.split_once(" → ") {
                        s.label = format!("{from} → {}", host_port(&host, port));
                    }
                    return;
                }
                Message::Disconnected { reason, .. } => {
                    let owner = s.owner;
                    let _ = s.tx.send(ToSocket::Close);
                    self.streams.remove(&channel);
                    if !reason.is_empty() {
                        self.note_error(owner, reason);
                    }
                    return;
                }
                _ => return,
            };
            let _ = s.tx.send(to_socket);
            return;
        }
        match msg {
            Message::ForwardListening { port, .. } => {
                if let Some(f) = self.remote_fwd(channel) {
                    f.state = FwdState::Listening(format!("server port {port}"));
                }
            }
            Message::Disconnected { reason, .. } => {
                if let Some(f) = self.remote_fwd(channel) {
                    f.remote = None;
                    f.state = FwdState::Failed(if reason.is_empty() {
                        "Stopped.".to_owned()
                    } else {
                        reason
                    });
                }
            }
            Message::ForwardIncoming {
                listener,
                x11,
                origin_host,
                origin_port,
                ..
            } => self.incoming(channel, listener, x11, &origin_host, origin_port),
            // A stream this side already closed.
            _ => {}
        }
    }

    fn remote_fwd(&mut self, channel: ChannelId) -> Option<&mut Fwd> {
        self.conns
            .values_mut()
            .flat_map(|c| c.forwards.iter_mut())
            .find(|f| f.remote == Some(channel))
    }

    /// The server opened a stream: connect it to the forward's target or
    /// the X server.
    fn incoming(
        &mut self,
        channel: ChannelId,
        listener: ChannelId,
        x11: bool,
        origin_host: &str,
        origin_port: u16,
    ) {
        let found = if x11 {
            self.conns.get_mut(&listener).and_then(|c| {
                let x = c.x11.as_mut()?;
                x.total += 1;
                Some((
                    Owner::X11(listener),
                    (x.display.host.clone(), x.display.port()),
                ))
            })
        } else {
            self.conns.iter_mut().find_map(|(conn, c)| {
                let f = c.forwards.iter_mut().find(|f| f.remote == Some(listener))?;
                f.total += 1;
                Some((Owner::Fwd(*conn, f.key), f.spec.target.clone()?))
            })
        };
        let Some((owner, (host, port))) = found else {
            self.send(Message::Close { channel });
            return;
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let label = format!(
            "{} → {}",
            host_port(origin_host, origin_port),
            host_port(&host, port)
        );
        self.streams.insert(channel, StreamRef { owner, tx, label });
        let connect = async move {
            let what = host_port(&host, port);
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
                .await
            {
                Ok(Ok(s)) => Ok(s),
                Ok(Err(e)) => Err(format!("Could not connect to {what}: {e}.")),
                Err(_) => Err(format!("Timed out connecting to {what}.")),
            }
        };
        let (to_worker, internal) = (self.to_worker.clone(), self.internal.clone());
        tokio::spawn(async move {
            let socket = connect.await;
            relay(channel, socket, rx, to_worker, internal).await;
        });
    }

    fn view(&self) -> Vec<ConnView> {
        let mut open: HashMap<Owner, (usize, Vec<String>)> = HashMap::new();
        let mut ids: Vec<&ChannelId> = self.streams.keys().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(s) = self.streams.get(id) else {
                continue;
            };
            let e = open.entry(s.owner).or_default();
            e.0 += 1;
            if e.1.len() < LISTED_STREAMS {
                e.1.push(s.label.clone());
            }
        }
        self.conns
            .iter()
            .map(|(&connection, c)| ConnView {
                connection,
                label: c.label.clone(),
                forwards: c
                    .forwards
                    .iter()
                    .map(|f| {
                        let (n, streams) = open
                            .get(&Owner::Fwd(connection, f.key))
                            .cloned()
                            .unwrap_or_default();
                        FwdView {
                            spec: f.spec.clone(),
                            state: f.state.clone(),
                            open: n,
                            total: f.total,
                            streams,
                            error: f.error.clone(),
                        }
                    })
                    .collect(),
                x11: c.x11.as_ref().map(|x| X11View {
                    display: x.display.to_string(),
                    open: open.get(&Owner::X11(connection)).map_or(0, |(n, _)| *n),
                    total: x.total,
                    error: x.error.clone(),
                }),
            })
            .collect()
    }
}

/// Aborts tasks when dropped.
struct AbortOnDrop(Vec<AbortHandle>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}

/// Binds every address and accepts connections until aborted.
async fn listen(hosts: Vec<String>, port: u16, owner: Owner, events: UnboundedSender<Internal>) {
    let mut listeners = Vec::new();
    let mut errors = Vec::new();
    for host in &hosts {
        let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host.as_str(), port)).await {
            Ok(a) => a.collect(),
            Err(e) => {
                errors.push(format!("{host}: {e}"));
                continue;
            }
        };
        for addr in addrs {
            match TcpListener::bind(addr).await {
                Ok(l) => listeners.push(l),
                Err(e) => errors.push(format!("{}: {e}", host_port(host, port))),
            }
        }
    }
    if listeners.is_empty() {
        let why = format!("Could not listen: {}.", errors.join("; "));
        let _ = events.send(Internal::ListenFailed { owner, why });
        return;
    }
    let addrs: Vec<String> = listeners
        .iter()
        .filter_map(|l| l.local_addr().ok())
        .map(|a| a.to_string())
        .collect();
    let _ = events.send(Internal::Listening {
        owner,
        addrs: addrs.join(", "),
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut accepting = AbortOnDrop(Vec::new());
    for l in listeners {
        let tx = tx.clone();
        let task = tokio::spawn(async move {
            loop {
                match l.accept().await {
                    Ok(conn) => {
                        if tx.send(conn).is_err() {
                            return;
                        }
                    }
                    // Out of sockets or the like: try again shortly.
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        });
        accepting.0.push(task.abort_handle());
    }
    drop(tx);
    // Removing the forward aborts this task, which drops `accepting`: the
    // accept tasks end and the ports are free again.
    while let Some((socket, peer)) = rx.recv().await {
        let _ = socket.set_nodelay(true);
        if events
            .send(Internal::Accepted {
                owner,
                socket,
                peer,
            })
            .is_err()
        {
            return;
        }
    }
}

/// Moves one stream's bytes between its socket and the worker, within the
/// stream windows. Ends when the worker ends the stream or the socket
/// fails.
async fn relay(
    channel: ChannelId,
    socket: Result<TcpStream, String>,
    mut from_worker: UnboundedReceiver<ToSocket>,
    to_worker: ToWorker,
    events: UnboundedSender<Internal>,
) {
    let socket = match socket {
        Ok(s) => s,
        Err(why) => {
            let _ = events.send(Internal::Ended {
                channel,
                why: Some(why),
            });
            return;
        }
    };
    let _ = socket.set_nodelay(true);
    let (mut read, mut write) = socket.into_split();
    // Writes wait for the program at the other end; they run in their own
    // task so reading goes on meanwhile. Credit goes back once written.
    let (wtx, mut wrx) = mpsc::unbounded_channel::<Option<Vec<u8>>>();
    let tw = to_worker.clone();
    let writer = tokio::spawn(async move {
        while let Some(item) = wrx.recv().await {
            match item {
                Some(bytes) => {
                    if write.write_all(&bytes).await.is_err() {
                        return false;
                    }
                    let n = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                    tw(Message::Window { channel, bytes: n });
                }
                None => {
                    let _ = write.shutdown().await;
                }
            }
        }
        let _ = write.shutdown().await;
        true
    });
    let mut credit = STREAM_WINDOW;
    let mut read_eof = false;
    let mut buf = vec![0u8; MAX_DATA_CHUNK];
    let why = loop {
        let want = (credit as usize).min(buf.len());
        tokio::select! {
            r = read.read(buf.get_mut(..want).unwrap_or_default()), if !read_eof && want > 0 => match r {
                Ok(0) => {
                    read_eof = true;
                    to_worker(Message::Eof { channel });
                }
                Ok(n) => {
                    credit -= u32::try_from(n).unwrap_or(credit);
                    to_worker(Message::Data {
                        channel,
                        bytes: buf.get(..n).unwrap_or_default().to_vec(),
                    });
                }
                Err(e) => break Some(format!("The local connection failed: {e}.")),
            },
            msg = from_worker.recv() => match msg {
                Some(ToSocket::Data(bytes)) => {
                    if wtx.send(Some(bytes)).is_err() {
                        break None;
                    }
                }
                Some(ToSocket::Eof) => {
                    let _ = wtx.send(None);
                }
                Some(ToSocket::Window(n)) => credit = credit.saturating_add(n).min(STREAM_WINDOW),
                Some(ToSocket::Close) | None => break None,
            },
        }
    };
    // What the worker sent before closing still reaches the program.
    drop(wtx);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
    let _ = events.send(Internal::Ended { channel, why });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn removing_a_forward_frees_its_port() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (fwd, runner) = Forwarder::new();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        runner
            .spawn(move |m| log.lock().unwrap().push(m), || {})
            .unwrap();
        let spec =
            ferossh_config::forward::parse_option(&format!("-L 127.0.0.1:{port}:db:5432")).unwrap();
        fwd.start(
            1,
            "me@db".into(),
            Setup {
                forwards: vec![spec],
                ..Setup::default()
            },
        );
        let wait = |want: &dyn Fn(&[ConnView]) -> bool| {
            for _ in 0..200 {
                if want(&fwd.view()) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("never got there: {:?}", fwd.view());
        };
        let first = |v: &[ConnView]| v.first().and_then(|c| c.forwards.first()).cloned();
        wait(&|v| first(v).is_some_and(|f| matches!(f.state, FwdState::Listening(_))));
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());
        // A connection becomes a direct-tcpip request to the worker.
        let _c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        wait(&|v| first(v).is_some_and(|f| f.open == 1));
        assert!(sent.lock().unwrap().iter().any(|m| matches!(
            m,
            Message::ForwardOpen { connection: 1, target: Some((h, 5432)), .. } if h == "db"
        )));
        fwd.remove(1, 0);
        wait(&|v| v.first().is_some_and(|c| c.forwards.is_empty()));
        let mut freed = false;
        for _ in 0..100 {
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                freed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(freed, "the port stayed bound");
        // The open connection was closed towards the worker too.
        assert!(
            sent.lock()
                .unwrap()
                .iter()
                .any(|m| matches!(m, Message::Close { .. }))
        );
    }

    #[test]
    fn listening_addresses_follow_ssh() {
        assert_eq!(bind_hosts(None, false), ["127.0.0.1", "::1"]);
        assert_eq!(bind_hosts(None, true), ["0.0.0.0", "::"]);
        assert_eq!(bind_hosts(Some("*"), false), ["0.0.0.0", "::"]);
        assert_eq!(bind_hosts(Some(""), false), ["0.0.0.0", "::"]);
        assert_eq!(bind_hosts(Some("LOCALHOST"), true), ["127.0.0.1", "::1"]);
        assert_eq!(bind_hosts(Some("10.0.0.5"), false), ["10.0.0.5"]);
        assert_eq!(remote_bind(None), "localhost");
        assert_eq!(remote_bind(Some("*")), "");
        assert_eq!(remote_bind(Some("0.0.0.0")), "0.0.0.0");
    }
}
