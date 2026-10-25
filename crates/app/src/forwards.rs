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
//!
//! A remote desktop is a local forward too: [`Forwarder::open_desktop`]
//! first checks that something answers on the server's RDP port, then
//! listens on a loopback port (never 3389, which Windows' own Remote
//! Desktop may hold) and tells the window where, through
//! [`Forwarder::take_desktop_events`]. It stops with the connection's other
//! forwards.

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
/// Remote desktop tunnels listen on a port in this range, picked from the
/// host's name so the same host gets the same port each time (Remote
/// Desktop Connection remembers certificates and sign-ins by address). A
/// port in use falls back to one the system picks.
const DESKTOP_PORTS: std::ops::RangeInclusive<u16> = 33900..=33999;

/// The loopback port a remote desktop tunnel for `label` tries first.
pub fn desktop_port_for(label: &str) -> u16 {
    // FNV-1a: stable across runs and builds, unlike std's hasher.
    let hash = label
        .to_ascii_lowercase()
        .bytes()
        .fold(0x811c_9dc5_u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        });
    let span = u32::from(DESKTOP_PORTS.end() - DESKTOP_PORTS.start()) + 1;
    DESKTOP_PORTS.start() + u16::try_from(hash % span).unwrap_or(0)
}

/// What became of a request to open a remote desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopEvent {
    /// The tunnel to `host:port` (as the server sees it; `localhost` is
    /// the server itself) listens at `addr` (`127.0.0.1:port`); a viewer
    /// may connect.
    Ready {
        connection: ChannelId,
        host: String,
        port: u16,
        addr: String,
    },
    /// Nothing answers on `host:port`, or the tunnel could not listen.
    Failed {
        connection: ChannelId,
        host: String,
        port: u16,
        why: String,
    },
}

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
    /// The tunnel of a remote desktop.
    pub desktop: bool,
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
    Desktop {
        connection: ChannelId,
        host: String,
        port: u16,
        local_port: u16,
    },
    FromWorker(Message),
    WorkerGone,
}

#[derive(Default)]
struct Shared {
    view: Mutex<Vec<ConnView>>,
    changed: AtomicBool,
    desktop: Mutex<Vec<DesktopEvent>>,
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

    /// Opens a tunnel to the desktop server on `host:port` as the
    /// connection's host sees it (`localhost`: the host itself),
    /// listening on loopback `local_port` (or, when that is taken, on a
    /// port the system picks). The answer comes as a [`DesktopEvent`].
    pub fn open_desktop(&self, connection: ChannelId, host: &str, port: u16, local_port: u16) {
        self.cmd(Cmd::Desktop {
            connection,
            host: host.to_owned(),
            port,
            local_port,
        });
    }

    /// What became of remote desktop requests since the last call.
    pub fn take_desktop_events(&self) -> Vec<DesktopEvent> {
        self.shared
            .desktop
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default()
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
            probes: HashMap::new(),
            desktop_events: Vec::new(),
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
            let desktop = std::mem::take(&mut engine.desktop_events);
            if !desktop.is_empty() {
                if let Ok(mut d) = self.shared.desktop.lock() {
                    d.extend(desktop);
                }
                self.shared.changed.store(true, Ordering::Release);
                notify();
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
    /// A remote desktop tunnel: the desktop server's host and port.
    desktop: Option<(String, u16)>,
    /// The window waits to hear where this desktop tunnel listens.
    desktop_waiting: bool,
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

/// A check that the server's RDP port answers, before a desktop tunnel.
struct Probe {
    connection: ChannelId,
    host: String,
    port: u16,
    local_port: u16,
}

struct Engine {
    to_worker: ToWorker,
    internal: UnboundedSender<Internal>,
    conns: BTreeMap<ChannelId, Conn>,
    streams: HashMap<ChannelId, StreamRef>,
    /// Open checks of RDP ports, by stream channel.
    probes: HashMap<ChannelId, Probe>,
    /// For the window; see [`Forwarder::take_desktop_events`].
    desktop_events: Vec<DesktopEvent>,
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
            if !self.streams.contains_key(&id) && !self.probes.contains_key(&id) && !remote_in_use {
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
            Cmd::Desktop {
                connection,
                host,
                port,
                local_port,
            } => self.probe_desktop(connection, host, port, local_port),
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
        let probes: Vec<ChannelId> = self
            .probes
            .iter()
            .filter(|(_, p)| p.connection == connection)
            .map(|(id, _)| *id)
            .collect();
        for id in probes {
            self.probes.remove(&id);
            self.send(Message::Close { channel: id });
        }
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
        self.add_fwd(connection, spec, None);
    }

    /// Adds a forward; `desktop` marks a remote desktop tunnel to that
    /// host and port. Returns the forward's key, or `None` when the
    /// connection is gone or already has this forward.
    fn add_fwd(
        &mut self,
        connection: ChannelId,
        spec: ForwardSpec,
        desktop: Option<(String, u16)>,
    ) -> Option<u64> {
        let conn = self.conns.get_mut(&connection)?;
        if conn.forwards.iter().any(|f| f.spec == spec) {
            return None;
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
            desktop_waiting: desktop.is_some(),
            desktop,
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
        Some(key)
    }

    /// Checks that something on the server answers on `port` before
    /// opening a desktop tunnel to it: a viewer pointed at a dead port
    /// only says it cannot connect, without saying why.
    fn probe_desktop(&mut self, connection: ChannelId, host: String, port: u16, local_port: u16) {
        if !self.conns.contains_key(&connection) {
            self.desktop_events.push(DesktopEvent::Failed {
                connection,
                host,
                port,
                why: "That connection is not logged in.".into(),
            });
            return;
        }
        let id = self.new_id();
        self.probes.insert(
            id,
            Probe {
                connection,
                host: host.clone(),
                port,
                local_port,
            },
        );
        self.send(Message::ForwardOpen {
            channel: id,
            connection,
            target: Some((host, port)),
            origin_host: "127.0.0.1".into(),
            origin_port: 0,
        });
    }

    /// The worker answered a probe.
    fn probed(&mut self, probe: Probe, answer: Result<(), String>) {
        let Probe {
            connection,
            host,
            port,
            local_port,
        } = probe;
        if let Err(why) = answer {
            let why = if host == "localhost" {
                format!(
                    "Nothing answers on the server's port {port}, so it has no remote desktop \
                     service there ({why})."
                )
            } else {
                format!(
                    "Nothing answers on {host} port {port} from the SSH server, so the \
                     desktop cannot be reached that way ({why})."
                )
            };
            self.desktop_events.push(DesktopEvent::Failed {
                connection,
                host,
                port,
                why,
            });
            return;
        }
        let target = (host, port);
        // A tunnel already open for this desktop serves again.
        let existing = self.conns.get_mut(&connection).and_then(|c| {
            c.forwards.iter_mut().find(|f| {
                f.desktop.as_ref() == Some(&target) && !matches!(f.state, FwdState::Failed(_))
            })
        });
        if let Some(f) = existing {
            match &f.state {
                FwdState::Listening(addrs) => {
                    let addr = first_addr(addrs);
                    let (host, port) = target;
                    self.desktop_events.push(DesktopEvent::Ready {
                        connection,
                        host,
                        port,
                        addr,
                    });
                }
                _ => f.desktop_waiting = true,
            }
            return;
        }
        self.add_fwd(connection, desktop_spec(local_port, &target), Some(target));
    }

    /// A desktop tunnel's listener started or failed.
    fn desktop_listening(&mut self, owner: Owner, result: Result<String, String>) {
        let Owner::Fwd(connection, key) = owner else {
            return;
        };
        let Some(f) = self.fwd_mut(owner) else {
            return;
        };
        let (Some(target), true) = (f.desktop.clone(), f.desktop_waiting) else {
            return;
        };
        f.desktop_waiting = false;
        let fixed = f.spec.port != 0;
        match result {
            Ok(addrs) => self.desktop_events.push(DesktopEvent::Ready {
                connection,
                host: target.0,
                port: target.1,
                addr: first_addr(&addrs),
            }),
            // The host's own port is taken: any free one will do.
            Err(_) if fixed => {
                if let Some(c) = self.conns.get_mut(&connection)
                    && let Some(i) = c.forwards.iter().position(|f| f.key == key)
                {
                    let fwd = c.forwards.remove(i);
                    self.end_fwd(connection, fwd);
                }
                self.add_fwd(connection, desktop_spec(0, &target), Some(target));
            }
            Err(why) => self.desktop_events.push(DesktopEvent::Failed {
                connection,
                host: target.0,
                port: target.1,
                why,
            }),
        }
    }

    fn internal(&mut self, ev: Internal) {
        match ev {
            Internal::Listening { owner, addrs } => {
                if let Some(f) = self.fwd_mut(owner) {
                    f.state = FwdState::Listening(addrs.clone());
                }
                self.desktop_listening(owner, Ok(addrs));
            }
            Internal::ListenFailed { owner, why } => {
                if let Some(f) = self.fwd_mut(owner) {
                    f.state = FwdState::Failed(why.clone());
                }
                self.desktop_listening(owner, Err(why));
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
        if self.probes.contains_key(&channel) {
            let answer = match msg {
                Message::ForwardOpened { .. } => Ok(()),
                Message::Disconnected { reason, .. } => Err(reason),
                // Anything the server says first is not ours to read.
                _ => return,
            };
            if answer.is_ok() {
                self.send(Message::Close { channel });
            }
            if let Some(probe) = self.probes.remove(&channel) {
                self.probed(probe, answer);
            }
            return;
        }
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
                            desktop: f.desktop.is_some(),
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

/// The forward behind a remote desktop: loopback `local` to the desktop
/// server's host and port.
fn desktop_spec(local: u16, (host, port): &(String, u16)) -> ForwardSpec {
    ForwardSpec {
        kind: ForwardKind::Local,
        bind: Some("127.0.0.1".into()),
        port: local,
        target: Some((host.clone(), *port)),
    }
}

/// The first of a listener's addresses.
fn first_addr(addrs: &str) -> String {
    addrs.split(", ").next().unwrap_or_default().to_owned()
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

    /// The engine with a log of what it sent the worker.
    fn engine() -> (Forwarder, Arc<Mutex<Vec<Message>>>) {
        let (fwd, runner) = Forwarder::new();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        runner
            .spawn(move |m| log.lock().unwrap().push(m), || {})
            .unwrap();
        (fwd, sent)
    }

    fn wait_for<T>(mut get: impl FnMut() -> Option<T>) -> T {
        for _ in 0..300 {
            if let Some(t) = get() {
                return t;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never got there");
    }

    /// The probe the engine sent for a desktop on `port`.
    fn probe(sent: &Mutex<Vec<Message>>, port: u16) -> ChannelId {
        wait_for(|| {
            sent.lock().unwrap().iter().find_map(|m| match m {
                Message::ForwardOpen {
                    channel,
                    target: Some((h, p)),
                    ..
                } if h == "localhost" && *p == port => Some(*channel),
                _ => None,
            })
        })
    }

    fn desktop_event(fwd: &Forwarder) -> DesktopEvent {
        wait_for(|| fwd.take_desktop_events().into_iter().next())
    }

    #[test]
    fn a_desktop_without_a_server_says_so() {
        let (fwd, sent) = engine();
        fwd.start(1, "me@box".into(), Setup::default());
        fwd.open_desktop(1, "localhost", 3389, 0);
        let channel = probe(&sent, 3389);
        fwd.sink()(Message::Disconnected {
            channel,
            reason: "localhost:3389: the server could not connect (ConnectFailed).".into(),
        });
        let DesktopEvent::Failed {
            connection: 1, why, ..
        } = desktop_event(&fwd)
        else {
            panic!("no failure");
        };
        assert!(why.contains("no remote desktop service"), "{why}");
        // No tunnel was left listening.
        assert!(fwd.view()[0].forwards.is_empty());
        // Not logged in: refused at once.
        fwd.open_desktop(9, "localhost", 3389, 0);
        assert!(matches!(
            desktop_event(&fwd),
            DesktopEvent::Failed { connection: 9, .. }
        ));
    }

    #[test]
    fn a_desktop_tunnel_listens_on_loopback_and_is_reused() {
        let (fwd, sent) = engine();
        fwd.start(1, "me@box".into(), Setup::default());
        // Someone holds the host's own port: the tunnel takes another.
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = taken.local_addr().unwrap().port();
        fwd.open_desktop(1, "localhost", 3390, busy);
        let channel = probe(&sent, 3390);
        fwd.sink()(Message::ForwardOpened {
            channel,
            host: "localhost".into(),
            port: 3390,
        });
        let DesktopEvent::Ready {
            connection: 1,
            addr,
            ..
        } = desktop_event(&fwd)
        else {
            panic!("not ready");
        };
        let at: SocketAddr = addr.parse().unwrap();
        assert!(at.ip().is_loopback());
        assert_ne!(at.port(), busy);
        assert_ne!(at.port(), 3389);
        // The probe's stream was closed again.
        assert!(
            sent.lock()
                .unwrap()
                .iter()
                .any(|m| matches!(m, Message::Close { channel: c } if *c == channel))
        );
        let view = fwd.view();
        let [f] = &view[0].forwards[..] else {
            panic!("{view:?}");
        };
        assert!(f.desktop);
        assert_eq!(f.spec.target, Some(("localhost".into(), 3390)));
        // A connection to the tunnel goes to the server's RDP port.
        let _c = std::net::TcpStream::connect(at).unwrap();
        wait_for(|| {
            sent.lock().unwrap().iter().any(|m| {
                matches!(m, Message::ForwardOpen { channel: c, target: Some((_, 3390)), .. } if *c != channel)
            }).then_some(())
        });
        // Asking again probes again and reuses the tunnel.
        sent.lock().unwrap().clear();
        fwd.open_desktop(1, "localhost", 3390, busy);
        let again = probe(&sent, 3390);
        fwd.sink()(Message::ForwardOpened {
            channel: again,
            host: "localhost".into(),
            port: 3390,
        });
        assert_eq!(
            desktop_event(&fwd),
            DesktopEvent::Ready {
                connection: 1,
                host: "localhost".into(),
                port: 3390,
                addr: at.to_string()
            }
        );
        assert_eq!(fwd.view()[0].forwards.len(), 1);
        // The tunnel stops with the connection.
        fwd.stop(1);
        wait_for(|| fwd.view().is_empty().then_some(()));
        wait_for(|| std::net::TcpListener::bind(at).ok());
    }

    #[test]
    fn desktop_ports_are_stable_and_never_rdp_s_own() {
        let a = desktop_port_for("jurgen@box");
        assert_eq!(a, desktop_port_for("JURGEN@box"));
        assert!(DESKTOP_PORTS.contains(&a));
        assert!(!DESKTOP_PORTS.contains(&3389));
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
