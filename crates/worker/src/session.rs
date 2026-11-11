//! One SSH connection: TCP, key exchange, host key check through the UI,
//! user authentication with keys the UI holds, a shell on a pty, and the
//! data pump. Further shells for new tabs and panes open as more channels
//! on the same connection ([`open`]); the connection ends with its last
//! shell.
//!
//! Every question the worker cannot answer itself (is this host key
//! trusted, which public keys exist, sign this, what is the password) goes
//! to the UI as a request message; the answer comes back on the session's
//! inbox and is routed to whoever is waiting for it by [`drive`].
//!
//! Connections the server opens back (remote forwards and X11) arrive at
//! the [`Client`] handler, which checks them against what this connection
//! asked for and hands them to [`crate::forward`].

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use ferossh_forward::proxy;
use ferossh_forward::x11::{MIT_COOKIE, hex};
use ferossh_ipc::{
    ChannelId, Family, GssReply, MAX_DATA_CHUNK, Message, MoshOptions, PromptField, PromptKind,
    ProxyKind, ProxyServer, Secrets, ShellOptions, Timing, WORKER_IDS, X11Auth,
};
use russh::client::{self, AuthResult, DisconnectReason, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::ssh_encoding::Encode;
use russh::keys::ssh_key::certificate::CertType;
use russh::keys::ssh_key::public::KeyData;
use russh::keys::{Algorithm, Certificate, HashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{
    AlgorithmKind, Channel, ChannelMsg, ChannelOpenFailure, Disconnect, MethodKind, MethodSet,
    Names, Preferred, SshId,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::crypto_policy::{
    MIN_RSA_BITS, aes_gcm_first, legacy_would_help, preferred_for, terrapin_prone, with_legacy,
    without_terrapin,
};
use crate::forward::{self, Incoming};
use crate::zmodem::{self, Zmodem, sleep_until};

/// Password and keyboard-interactive rounds, like `NumberOfPasswordPrompts`.
const MAX_ATTEMPTS: usize = 3;
const TERM: &str = "xterm-256color";
const CLIENT_ID: &str = "SSH-2.0-FeroSSH";
const TERRAPIN: &str = "The server has no strict key exchange, so its ChaCha20-Poly1305 and EtM \
     MACs are open to the Terrapin attack, and it offers nothing else FeroSSH allows. Update \
     the server's SSH software.";
/// Added to a negotiation failure when the legacy set would have helped.
const LEGACY_HINT: &str = "If this is older equipment you trust, add `LegacyAlgorithms yes` to \
     its profile in ferossh.conf.";

/// What the UI asked for in [`Message::Connect`].
#[derive(Debug, Clone)]
pub struct Params {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub cols: u16,
    pub rows: u16,
    pub host_key_algs: Vec<String>,
    pub x11: Option<X11Auth>,
    /// Also offer the legacy algorithm set.
    pub legacy: bool,
    pub timing: Timing,
    /// Run the connection over this stream channel instead of TCP: the
    /// hop after a jump host, or a `ProxyCommand` (see [`crate::tunnel`]).
    pub tunnel: Option<ChannelId>,
    /// Open a shell once logged in; a jump host's login opens none.
    pub shell: bool,
    /// What the shell's channel asks for.
    pub options: ShellOptions,
    /// Which IP versions to connect over.
    pub family: Family,
    /// Try Kerberos (`gssapi-with-mic`) first when the server offers it.
    pub gssapi: bool,
    /// Reach the host through this SOCKS 5 or HTTP proxy.
    pub proxy: Option<ProxyServer>,
}

/// Why a session ended early.
#[derive(Debug)]
pub enum Failure {
    Ssh(russh::Error),
    /// A policy refusal or a user decision, already worded for the user.
    Refused(String),
    /// The server chose ChaCha20-Poly1305 (or CTR with an EtM MAC) but has
    /// no strict key exchange, which leaves it open to Terrapin.
    /// [`session`] retries without those.
    NoStrictKex,
    /// The UI closed the channel or went away.
    Cancelled,
}

impl From<russh::Error> for Failure {
    fn from(e: russh::Error) -> Self {
        Failure::Ssh(e)
    }
}

impl From<russh::SendError> for Failure {
    fn from(_: russh::SendError) -> Self {
        Failure::Cancelled
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Ssh(russh::Error::NoCommonAlgo { kind, theirs, .. }) => {
                let kind = match kind {
                    AlgorithmKind::Kex => "key exchange",
                    AlgorithmKind::Key => "host key",
                    AlgorithmKind::Cipher => "cipher",
                    AlgorithmKind::Mac => "MAC",
                    AlgorithmKind::Compression => "compression",
                };
                // The pseudo-algorithms say nothing useful here.
                let offered: Vec<&str> = theirs
                    .iter()
                    .map(String::as_str)
                    .filter(|n| !n.starts_with("ext-info-") && !n.starts_with("kex-strict-"))
                    .collect();
                write!(
                    f,
                    "The server offers no {kind} algorithm that FeroSSH allows. It offers: {}.",
                    offered.join(", ")
                )
            }
            Failure::Ssh(russh::Error::KeepaliveTimeout) => f.write_str(
                "The server stopped responding (no reply to keepalives); the connection is \
                 closed.",
            ),
            Failure::Ssh(russh::Error::IO(e)) => match e.kind() {
                std::io::ErrorKind::UnexpectedEof => {
                    f.write_str("The server closed the connection without saying why.")
                }
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted => {
                    f.write_str("The connection was reset.")
                }
                _ => write!(f, "The connection broke: {e}."),
            },
            Failure::Ssh(e) => write!(f, "{e}."),
            Failure::Refused(why) => f.write_str(why),
            Failure::NoStrictKex => f.write_str(TERRAPIN),
            Failure::Cancelled => f.write_str("Cancelled."),
        }
    }
}

/// A logged-in connection, shared by the channels (shells) on it.
pub(crate) type Connection = Handle<Client>;

/// Where the worker's dispatcher sends a channel's messages from the UI.
#[derive(Clone)]
pub(crate) enum Route {
    /// A shell, SFTP or a remote forward's listener. Only the UI sends on
    /// it (what the user types or pastes, and SFTP requests the UI keeps
    /// to a window), without waiting; a task per session feeds it into
    /// the session's bounded queue, so a stalled session never holds up
    /// the pipe. A session whose front queue is full is aborted. Each
    /// message travels with its weight, the bytes it counts against the
    /// session's budget.
    Session(mpsc::Sender<(Message, usize)>, Arc<SessionGuard>),
    /// A forwarded stream: flow control bounds what the UI sends, and one
    /// slow stream must never hold up the pipe.
    Stream(mpsc::UnboundedSender<Message>),
}

/// A session's task and the task that feeds its queue: dropping the
/// last copy (with the route) stops the feeding task.
pub(crate) struct SessionGuard {
    _feed: oneshot::Sender<()>,
    task: Mutex<Option<tokio::task::AbortHandle>>,
    /// Bytes the dispatcher has queued that the session has not taken.
    queued: AtomicUsize,
}

impl SessionGuard {
    pub(crate) fn new(feed: oneshot::Sender<()>) -> Self {
        Self {
            _feed: feed,
            task: Mutex::new(None),
            queued: AtomicUsize::new(0),
        }
    }

    /// Counts `n` more queued bytes, or refuses when that would pass
    /// `limit`.
    pub(crate) fn reserve(&self, n: usize, limit: usize) -> bool {
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |q| {
                q.checked_add(n).filter(|total| *total <= limit)
            })
            .is_ok()
    }

    /// Counts `n` queued bytes as taken by the session.
    pub(crate) fn release(&self, n: usize) {
        let _ = self
            .queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |q| {
                Some(q.saturating_sub(n))
            });
    }

    /// The session's own task, for [`SessionGuard::abort`].
    pub(crate) fn track(&self, task: tokio::task::JoinHandle<()>) {
        if let Ok(mut t) = self.task.lock() {
            *t = Some(task.abort_handle());
        }
    }

    /// Ends the session's task where it is, even mid-write.
    pub(crate) fn abort(&self) {
        if let Some(t) = self.task.lock().ok().and_then(|mut t| t.take()) {
            t.abort();
        }
    }
}

impl Route {
    fn is_closed(&self) -> bool {
        match self {
            Route::Session(tx, _) => tx.is_closed(),
            Route::Stream(tx) => tx.is_closed(),
        }
    }
}

/// Every channel the UI can address, by id.
#[derive(Clone, Default)]
pub(crate) struct Routes(Arc<Mutex<HashMap<ChannelId, Route>>>);

impl Routes {
    /// Adds a route; `false` when the id is taken by a live channel.
    pub(crate) fn insert(&self, channel: ChannelId, route: Route) -> bool {
        let Ok(mut map) = self.0.lock() else {
            return false;
        };
        map.retain(|_, r| !r.is_closed());
        if map.contains_key(&channel) {
            return false;
        }
        map.insert(channel, route);
        true
    }

    pub(crate) fn get(&self, channel: ChannelId) -> Option<Route> {
        self.0.lock().ok()?.get(&channel).cloned()
    }

    pub(crate) fn remove(&self, channel: ChannelId) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&channel);
        }
    }
}

/// What the connection set up for traffic the server opens back to this
/// side, which [`Client`] checks every such channel against.
pub(crate) struct ConnForwards {
    /// The channel that made the connection; X11 streams name it.
    pub(crate) connection: ChannelId,
    /// Remote forwards the server listens for: address and port as asked
    /// (the port the server chose, for port 0), and the UI's listener.
    remote: Mutex<Vec<(String, u32, ChannelId)>>,
    /// The fake cookie the server got, and the real authorisation.
    x11: Mutex<Option<([u8; 16], X11Auth)>>,
    /// Changes (fails) when the connection's handler is gone.
    alive: watch::Receiver<()>,
    /// A shell asked for agent forwarding, so the server may open agent
    /// channels.
    agent: AtomicBool,
    /// The host name as given, and the address the TCP connection went
    /// to (none through a jump host or a `ProxyCommand`): where Mosh's
    /// UDP goes.
    host: String,
    peer: Option<std::net::IpAddr>,
    family: Family,
}

impl ConnForwards {
    pub(crate) fn alive(&self) -> watch::Receiver<()> {
        self.alive.clone()
    }

    pub(crate) fn add_remote(&self, bind: &str, port: u32, listener: ChannelId) {
        if let Ok(mut r) = self.remote.lock() {
            r.push((bind.to_owned(), port, listener));
        }
    }

    pub(crate) fn remove_remote(&self, listener: ChannelId) {
        if let Ok(mut r) = self.remote.lock() {
            r.retain(|(_, _, l)| *l != listener);
        }
    }

    /// The listener a `forwarded-tcpip` channel belongs to: the address and
    /// port as asked, or failing that the only forward on that port (some
    /// servers report the address they actually bound).
    fn remote_listener(&self, address: &str, port: u32) -> Option<ChannelId> {
        let r = self.remote.lock().ok()?;
        if let Some((_, _, l)) = r.iter().find(|(a, p, _)| a == address && *p == port) {
            return Some(*l);
        }
        let mut on_port = r.iter().filter(|(_, p, _)| *p == port);
        match (on_port.next(), on_port.next()) {
            (Some((_, _, l)), None) => Some(*l),
            _ => None,
        }
    }

    /// The fake cookie for an `x11-req`: one per connection, made with the
    /// first request. The latest real authorisation is kept.
    fn x11_cookie(&self, real: X11Auth) -> Option<[u8; 16]> {
        let mut x = self.x11.lock().ok()?;
        let fake = match x.as_ref() {
            Some((fake, _)) => *fake,
            None => {
                let mut fake = [0u8; 16];
                getrandom::fill(&mut fake).ok()?;
                fake
            }
        };
        *x = Some((fake, real));
        Some(fake)
    }

    /// The fake cookie and the real authorisation, once X11 was asked for.
    pub(crate) fn x11(&self) -> Option<([u8; 16], X11Auth)> {
        self.x11.lock().ok()?.clone()
    }
}

/// Live connections by the channel that made them, for
/// [`Message::OpenChannel`], SFTP and forwards. Weak: each shell holds its
/// connection (as do SFTP and the streams that keep it, a remote
/// desktop's), and the last one to close disconnects it.
/// The negotiated names travel along, for the new shell's
/// [`Message::Connected`]. Also the dispatcher's routes, and the ids for
/// streams the server opens.
#[derive(Clone, Default)]
pub(crate) struct Connections {
    map: Arc<Mutex<HashMap<ChannelId, Entry>>>,
    pub(crate) routes: Routes,
    next_id: Arc<AtomicU32>,
}

struct Entry {
    conn: Weak<Connection>,
    names: Names3,
    forwards: Arc<ConnForwards>,
}

/// Negotiated kex, cipher and host key algorithm.
type Names3 = Option<(String, String, String)>;

impl fmt::Debug for Connections {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connections").finish_non_exhaustive()
    }
}

impl Connections {
    fn insert(
        &self,
        channel: ChannelId,
        conn: &Arc<Connection>,
        names: Names3,
        forwards: Arc<ConnForwards>,
    ) {
        if let Ok(mut map) = self.map.lock() {
            map.retain(|_, e| e.conn.strong_count() > 0);
            map.insert(
                channel,
                Entry {
                    conn: Arc::downgrade(conn),
                    names,
                    forwards,
                },
            );
        }
    }

    pub(crate) fn get(
        &self,
        channel: ChannelId,
    ) -> Option<(Arc<Connection>, Names3, Arc<ConnForwards>)> {
        let map = self.map.lock().ok()?;
        let e = map.get(&channel)?;
        let conn = e.conn.upgrade().filter(|c| !c.is_closed())?;
        Some((conn, e.names.clone(), e.forwards.clone()))
    }

    /// An id for a stream the server opened, from the worker's own range.
    pub(crate) fn new_id(&self) -> ChannelId {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        WORKER_IDS | (n & !WORKER_IDS)
    }
}

/// Answers the UI has not delivered yet, and what the handler learnt.
#[derive(Default)]
struct Shared {
    verdict: Option<oneshot::Sender<bool>>,
    signature: Option<oneshot::Sender<Vec<u8>>>,
    identities: Option<oneshot::Sender<Vec<Vec<u8>>>>,
    answers: Option<oneshot::Sender<Secrets>>,
    gssapi: Option<oneshot::Sender<GssReply>>,
    /// Negotiated kex, cipher and host key algorithm.
    names: Option<(String, String, String)>,
    /// The server's disconnect message.
    server_said: Option<String>,
    /// Why the connection broke, when it did not close cleanly.
    lost: Option<String>,
    /// The host key the UI accepted, so a retried connection to the same
    /// server does not ask again.
    accepted_key: Option<Vec<u8>>,
}

type SharedRef = Arc<Mutex<Shared>>;

fn with<R>(shared: &SharedRef, f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
    shared.lock().ok().map(|mut s| f(&mut s))
}

pub(crate) struct Client {
    channel: ChannelId,
    out: mpsc::Sender<Message>,
    shared: SharedRef,
    forwards: Arc<ConnForwards>,
    connections: Connections,
    /// Dropped with the handler, when the connection ends.
    _alive: watch::Sender<()>,
}

impl Client {
    fn incoming(&self, channel: Channel<client::Msg>, from: Incoming) {
        tokio::spawn(forward::incoming(
            channel,
            from,
            self.out.clone(),
            self.connections.clone(),
        ));
    }
}

impl client::Handler for Client {
    type Error = Failure;

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut client::Session,
    ) -> Result<(), Failure> {
        let msg = Message::Banner {
            channel: self.channel,
            text: banner.to_owned(),
        };
        self.out.send(msg).await.map_err(|_| Failure::Cancelled)
    }

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Failure> {
        let key = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(cert) => return self.check_host_cert(cert).await,
        };
        check_key_size(key.key_data(), "host key")?;
        let blob = key
            .to_bytes()
            .map_err(|_| Failure::Refused("The server's host key is malformed.".into()))?;
        if with(&self.shared, |s| s.accepted_key.as_ref() == Some(&blob)) == Some(true) {
            return Ok(true);
        }
        let (tx, rx) = oneshot::channel();
        with(&self.shared, |s| s.verdict = Some(tx));
        let msg = Message::HostKey {
            channel: self.channel,
            key: blob.clone(),
        };
        self.out.send(msg).await.map_err(|_| Failure::Cancelled)?;
        match rx.await {
            Ok(true) => {
                with(&self.shared, |s| s.accepted_key = Some(blob));
                Ok(true)
            }
            Ok(false) => Err(Failure::Refused("Host key not accepted.".into())),
            Err(_) => Err(Failure::Cancelled),
        }
    }

    async fn kex_done(
        &mut self,
        _shared_secret: Option<&[u8]>,
        names: &Names,
        _session: &mut client::Session,
    ) -> Result<(), Failure> {
        // Terrapin (CVE-2023-48795) needs ChaCha20-Poly1305 or an EtM MAC
        // without strict key exchange; AES-GCM is not affected.
        if !names.strict_kex() && terrapin_prone(&names.cipher, &names.client_mac) {
            return Err(Failure::NoStrictKex);
        }
        let found = (
            names.kex.as_ref().to_owned(),
            names.cipher.as_ref().to_owned(),
            names.key.as_str().to_owned(),
        );
        with(&self.shared, |s| s.names = Some(found));
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Failure> {
        // Only ports this connection asked the server to listen on.
        match self
            .forwards
            .remote_listener(connected_address, connected_port)
        {
            Some(listener) => {
                reply.accept().await;
                self.incoming(
                    channel,
                    Incoming::Remote {
                        listener,
                        origin: (originator_address.to_owned(), originator_port),
                    },
                );
            }
            None => {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await
            }
        }
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        channel: Channel<client::Msg>,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Failure> {
        // Only after a shell asked for X11 forwarding.
        match self.forwards.x11() {
            Some((fake, real)) => {
                reply.accept().await;
                self.incoming(
                    channel,
                    Incoming::X11 {
                        connection: self.forwards.connection,
                        fake,
                        real,
                        origin: (originator_address.to_owned(), originator_port),
                    },
                );
            }
            None => {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await
            }
        }
        Ok(())
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Failure> {
        // Only after a shell asked for agent forwarding.
        if self.forwards.agent.load(Ordering::Relaxed) {
            reply.accept().await;
            tokio::spawn(crate::agent::serve(
                channel,
                self.forwards.connection,
                self.out.clone(),
                self.connections.clone(),
            ));
        } else {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
        }
        Ok(())
    }

    async fn disconnected(&mut self, reason: DisconnectReason<Failure>) -> Result<(), Failure> {
        match reason {
            DisconnectReason::ReceivedDisconnect(info) => {
                with(&self.shared, |s| s.server_said = Some(info.message));
                Ok(())
            }
            DisconnectReason::Error(e) => {
                // The shells only see their channels close; keep why.
                with(&self.shared, |s| s.lost = Some(e.to_string()));
                Err(e)
            }
        }
    }
}

/// Refuses RSA keys below [`MIN_RSA_BITS`]; `what` names the key.
fn check_key_size(key: &KeyData, what: &str) -> Result<(), Failure> {
    match key.rsa() {
        Some(rsa) if (rsa.key_size() as usize) < MIN_RSA_BITS => Err(Failure::Refused(format!(
            "The server's RSA {what} has {} bits; FeroSSH requires at least {MIN_RSA_BITS}.",
            rsa.key_size()
        ))),
        _ => Ok(()),
    }
}

/// What makes a host certificate unusable before the UI sees it: the wrong
/// type, unsupported critical options, a bad CA signature, or a CA key
/// or signature outside the policy.
fn cert_problem(cert: &Certificate) -> Option<String> {
    if cert.cert_type() != CertType::Host {
        return Some("is a user certificate, not a host certificate".into());
    }
    // Critical options constrain validity and must never be ignored. No
    // host-certificate critical options are currently supported.
    if !cert.critical_options().is_empty() {
        return Some("contains unsupported critical options".into());
    }
    let ca = cert.signature_key();
    match ca.algorithm() {
        Algorithm::Ed25519 | Algorithm::Ecdsa { .. } => {}
        Algorithm::Rsa { .. } => {
            if ca
                .rsa()
                .is_some_and(|r| (r.key_size() as usize) < MIN_RSA_BITS)
            {
                return Some(format!(
                    "is signed by an RSA CA key under {MIN_RSA_BITS} bits"
                ));
            }
            // ssh-rsa signatures are SHA-1.
            if cert.signature().algorithm().as_str() == "ssh-rsa" {
                return Some("is signed with SHA-1 (ssh-rsa)".into());
            }
        }
        other => return Some(format!("is signed by a {} CA key", other.as_str())),
    }
    if let Err(e) = cert.verify_signature() {
        return Some(format!("has a CA signature that does not verify ({e})"));
    }
    None
}

impl Client {
    /// Checks what the worker can of a host certificate, then asks the UI
    /// whether its CA is trusted for this host.
    async fn check_host_cert(&mut self, cert: &Certificate) -> Result<bool, Failure> {
        if let Some(problem) = cert_problem(cert) {
            return Err(Failure::Refused(format!(
                "The server's host certificate {problem}. Not connecting."
            )));
        }
        check_key_size(cert.public_key(), "certified host key")?;
        let malformed = || Failure::Refused("The server's host certificate is malformed.".into());
        let ca = cert.signature_key().encode_vec().map_err(|_| malformed())?;
        let key = cert.public_key().encode_vec().map_err(|_| malformed())?;
        let (tx, rx) = oneshot::channel();
        with(&self.shared, |s| s.verdict = Some(tx));
        let msg = Message::HostCert {
            channel: self.channel,
            ca,
            key,
            principals: cert.valid_principals().to_vec(),
            valid_after: cert.valid_after(),
            valid_before: cert.valid_before(),
            key_id: cert.key_id().to_owned(),
            serial: cert.serial(),
        };
        self.out.send(msg).await.map_err(|_| Failure::Cancelled)?;
        match rx.await {
            Ok(true) => Ok(true),
            Ok(false) => Err(Failure::Refused("Host certificate not accepted.".into())),
            Err(_) => Err(Failure::Cancelled),
        }
    }
}

/// Signs through the UI. A refusal or failure must not be returned as an
/// error: russh would then wait forever for the signature. Instead an
/// empty signature goes to the server, which rejects it, and the next key
/// is tried.
struct IpcSigner {
    channel: ChannelId,
    out: mpsc::Sender<Message>,
    shared: SharedRef,
}

/// Appends `sig` to `data` as an SSH string, which is what russh expects
/// back from a signer.
fn append_signature(mut data: Vec<u8>, sig: &[u8]) -> Vec<u8> {
    data.extend_from_slice(&u32::try_from(sig.len()).unwrap_or(0).to_be_bytes());
    data.extend_from_slice(sig);
    data
}

fn sig_algorithm(sig: &[u8]) -> Option<&[u8]> {
    let (len, rest) = sig.split_first_chunk::<4>()?;
    rest.get(..u32::from_be_bytes(*len) as usize)
}

/// A signature the server will reject: the right algorithm, no bytes.
fn empty_signature(alg: &str) -> Vec<u8> {
    append_signature(append_signature(Vec::new(), alg.as_bytes()), &[])
}

impl russh::Signer for IpcSigner {
    type Error = Failure;

    fn auth_sign(
        &mut self,
        key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Failure>> + Send {
        let (blob, alg) = match key {
            AgentIdentity::PublicKey { key, .. } => (
                key.to_bytes().unwrap_or_default(),
                match key.algorithm() {
                    Algorithm::Rsa { .. } => Algorithm::Rsa { hash: hash_alg },
                    other => other,
                }
                .as_str()
                .to_owned(),
            ),
            // The certificate goes to the UI to find its signer; the
            // signature is made by the key inside it.
            AgentIdentity::Certificate { certificate, .. } => (
                certificate.to_bytes().unwrap_or_default(),
                match certificate.public_key().algorithm() {
                    Algorithm::Rsa { .. } => Algorithm::Rsa { hash: hash_alg },
                    other => other,
                }
                .as_str()
                .to_owned(),
            ),
        };
        async move {
            let (tx, rx) = oneshot::channel();
            with(&self.shared, |s| s.signature = Some(tx));
            let msg = Message::SignRequest {
                channel: self.channel,
                key: blob,
                data: to_sign.clone(),
            };
            self.out.send(msg).await.map_err(|_| Failure::Cancelled)?;
            let sig = rx.await.map_err(|_| Failure::Cancelled)?;
            // The UI's signer must use the algorithm the request names.
            if sig.is_empty() || sig_algorithm(&sig) != Some(alg.as_bytes()) {
                return Ok(append_signature(to_sign, &empty_signature(&alg)));
            }
            Ok(append_signature(to_sign, &sig))
        }
    }
}

/// Session state the driver keeps while it waits.
struct Ctx {
    channel: ChannelId,
    out: mpsc::Sender<Message>,
    shared: SharedRef,
    /// The latest pane size, applied when the pty is requested.
    size: (u16, u16),
    /// X11 forwarding to ask for with the shell.
    x11: Option<X11Auth>,
    /// What else the shell's channel asks for.
    options: ShellOptions,
    /// The connection's forwarding state, once logged in.
    forwards: Option<Arc<ConnForwards>>,
}

impl Ctx {
    async fn send(&self, msg: Message) -> Result<(), Failure> {
        self.out.send(msg).await.map_err(|_| Failure::Cancelled)
    }

    async fn notice(&self, text: String) -> Result<(), Failure> {
        let channel = self.channel;
        self.send(Message::Notice { channel, text }).await
    }
}

/// Runs `fut` while delivering the UI's answers from `inbox` to whoever
/// waits for them. A closed channel cancels.
async fn drive<T>(
    fut: impl Future<Output = T>,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<T, Failure> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            biased;
            out = &mut fut => return Ok(out),
            msg = inbox.recv() => {
                let s = &ctx.shared;
                match msg {
                    None | Some(Message::Close { .. }) => return Err(Failure::Cancelled),
                    Some(Message::HostKeyVerdict { accept, .. }) => {
                        if let Some(tx) = with(s, |s| s.verdict.take()).flatten() {
                            let _ = tx.send(accept);
                        }
                    }
                    Some(Message::Signature { signature, .. }) => {
                        if let Some(tx) = with(s, |s| s.signature.take()).flatten() {
                            let _ = tx.send(signature);
                        }
                    }
                    Some(Message::Identities { keys, .. }) => {
                        if let Some(tx) = with(s, |s| s.identities.take()).flatten() {
                            let _ = tx.send(keys);
                        }
                    }
                    Some(Message::GssapiReply { reply, .. }) => {
                        if let Some(tx) = with(s, |s| s.gssapi.take()).flatten() {
                            let _ = tx.send(reply);
                        }
                    }
                    Some(Message::Answers { answers, .. }) => {
                        if let Some(tx) = with(s, |s| s.answers.take()).flatten() {
                            let _ = tx.send(answers);
                        }
                    }
                    Some(Message::Resize { cols, rows, .. }) => ctx.size = (cols, rows),
                    // Typing before the shell is open goes nowhere, as in ssh.
                    Some(_) => {}
                }
            }
        }
    }
}

/// Ends a session: reports why to the UI. `tunnel` is the inbox of the
/// stream [`Params::tunnel`] names.
pub async fn run(
    channel: ChannelId,
    params: Params,
    mut inbox: mpsc::Receiver<Message>,
    tunnel: Option<mpsc::UnboundedReceiver<Message>>,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let shared: SharedRef = Arc::default();
    let mut ctx = Ctx {
        channel,
        out: out.clone(),
        shared: shared.clone(),
        size: (params.cols, params.rows),
        x11: params.x11.clone(),
        options: params.options.clone(),
        forwards: None,
    };
    let result = session(&params, &mut inbox, tunnel, &mut ctx, &connections).await;
    let server_said = with(&shared, |s| s.server_said.take()).flatten();
    let lost = with(&shared, |s| s.lost.take()).flatten();
    let mut dropped = false;
    let reason = match (result, server_said) {
        (_, Some(said)) if !said.trim().is_empty() => {
            format!("The server closed the connection: {}", said.trim())
        }
        (Ok(Ended::Lost), _) => {
            dropped = true;
            lost.unwrap_or_else(|| LOST.to_owned())
        }
        (Ok(Ended::Connection), _) => lost.unwrap_or_else(|| "Connection closed.".to_owned()),
        (Ok(Ended::Shell), _) => SHELL_CLOSED.to_owned(),
        (Err(e), _) => e.to_string(),
    };
    if dropped {
        let _ = out.send(Message::Lost { channel }).await;
    }
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}

/// What a shell opened on an existing connection asks for.
#[derive(Debug, Clone)]
pub struct ShellRequest {
    /// The pane's size.
    pub size: (u16, u16),
    pub x11: Option<X11Auth>,
    pub options: ShellOptions,
}

/// Opens another shell on the connection `connection` made, for a new tab
/// or pane: no new login. Reports why it ended, like [`run`].
pub async fn open(
    channel: ChannelId,
    connection: ChannelId,
    shell_request: ShellRequest,
    mut inbox: mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let ShellRequest { size, x11, options } = shell_request;
    let mut ctx = Ctx {
        channel,
        out: out.clone(),
        shared: Arc::default(),
        size,
        x11,
        options,
        forwards: None,
    };
    let result = match connections.get(connection) {
        Some((conn, names, forwards)) => {
            with(&ctx.shared, |s| s.names = names);
            ctx.forwards = Some(forwards);
            shell(conn, &mut inbox, &mut ctx).await
        }
        None => Err(Failure::Refused(
            "That connection is closed; connect again.".into(),
        )),
    };
    let reason = match result {
        Ok(Ended::Lost) => {
            let _ = out.send(Message::Lost { channel }).await;
            LOST.to_owned()
        }
        Ok(Ended::Connection) => "Connection closed.".to_owned(),
        Ok(Ended::Shell) => SHELL_CLOSED.to_owned(),
        Err(e) => e.to_string(),
    };
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}

/// Why a shell's channel ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// This was the connection's last channel, so the connection closed too.
    Connection,
    /// Other tabs still use the connection.
    Shell,
    /// The connection broke under the open shell (a network drop, a
    /// keepalive timeout, the server going away) rather than the shell
    /// closing its channel.
    Lost,
}

/// The reason given when a shell ends while its connection stays open.
const SHELL_CLOSED: &str = "The shell closed; the connection stays open for the other tabs.";

/// The reason given when the connection broke and nothing says why.
const LOST: &str = "The connection was lost.";

/// How a shell's channel ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closed {
    /// The server closed the channel: the shell exited.
    Channel,
    /// The channel went away without being closed: the connection broke.
    Lost,
}

async fn session(
    p: &Params,
    inbox: &mut mpsc::Receiver<Message>,
    tunnel: Option<mpsc::UnboundedReceiver<Message>>,
    ctx: &mut Ctx,
    connections: &Connections,
) -> Result<Ended, Failure> {
    let mut preferred = preferred_for(&p.host_key_algs);
    if p.legacy {
        preferred = with_legacy(preferred);
    }
    let (mut handle, forwards) = match (p.tunnel, tunnel) {
        (Some(id), Some(rx)) => {
            // A tunnel carries one connection, so there is no second try
            // without ChaCha20: AES-GCM goes first instead, which any
            // server without strict key exchange has.
            let stream = crate::tunnel::open(id, rx, ctx.out.clone(), connections.routes.clone());
            match handshake(
                p,
                aes_gcm_first(preferred),
                stream,
                None,
                inbox,
                ctx,
                connections,
            )
            .await
            {
                Err(Failure::Ssh(e @ russh::Error::NoCommonAlgo { .. })) if !p.legacy => {
                    return Err(suggest_legacy(e));
                }
                other => other?,
            }
        }
        (Some(_), None) => return Err(Failure::Refused("The tunnel is already in use.".into())),
        (None, _) => direct(p, preferred, inbox, ctx, connections).await?,
    };

    authenticate(&mut handle, p, inbox, ctx).await?;
    let conn = Arc::new(handle);
    let names = with(&ctx.shared, |s| s.names.clone()).flatten();
    connections.insert(ctx.channel, &conn, names, forwards.clone());
    ctx.forwards = Some(forwards.clone());
    if p.shell {
        shell(conn, inbox, ctx).await
    } else {
        carry(conn, &forwards, inbox, ctx).await
    }
}

/// Logs in over TCP. A server without strict key exchange may not use
/// ChaCha20, but that only shows once it has chosen it: connect again
/// without it.
async fn direct(
    p: &Params,
    preferred: Preferred,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
    connections: &Connections,
) -> Result<(Connection, Arc<ConnForwards>), Failure> {
    let first = connect(p, preferred.clone(), inbox, ctx, connections).await;
    Ok(match first {
        Err(Failure::NoStrictKex) => {
            match connect(p, without_terrapin(preferred), inbox, ctx, connections).await {
                Err(Failure::Ssh(russh::Error::NoCommonAlgo { .. })) => {
                    return Err(Failure::NoStrictKex);
                }
                other => other?,
            }
        }
        Err(Failure::Ssh(e @ russh::Error::NoCommonAlgo { .. })) if !p.legacy => {
            return Err(suggest_legacy(e));
        }
        other => other?,
    })
}

/// A jump host's login: no shell, only the streams the UI opens on the
/// connection for the next hop. Holds the connection until the UI closes
/// the channel or the server goes away; the streams hold it after that.
async fn carry(
    conn: Arc<Connection>,
    forwards: &ConnForwards,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<Ended, Failure> {
    ctx.send(connected(ctx)).await?;
    let mut alive = forwards.alive();
    loop {
        tokio::select! {
            msg = inbox.recv() => match msg {
                None | Some(Message::Close { .. }) => break,
                Some(_) => {}
            },
            gone = alive.changed() => if gone.is_err() {
                break;
            },
        }
    }
    Ok(match Arc::into_inner(conn) {
        Some(handle) => {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
            Ended::Connection
        }
        None => Ended::Shell,
    })
}

/// [`Message::Connected`] with the negotiated algorithms.
fn connected(ctx: &Ctx) -> Message {
    let (kex, cipher, host_key) = with(&ctx.shared, |s| s.names.clone())
        .flatten()
        .unwrap_or_default();
    Message::Connected {
        channel: ctx.channel,
        kex,
        cipher,
        host_key,
    }
}

/// A negotiation failure, with the hint to turn on the legacy set when the
/// server offers something in it.
fn suggest_legacy(e: russh::Error) -> Failure {
    let helps =
        matches!(&e, russh::Error::NoCommonAlgo { theirs, .. } if legacy_would_help(theirs));
    let failure = Failure::Ssh(e);
    if helps {
        Failure::Refused(format!("{failure} {LEGACY_HINT}"))
    } else {
        failure
    }
}

/// Opens the TCP connection and runs key exchange with `preferred`.
async fn connect(
    p: &Params,
    preferred: Preferred,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
    connections: &Connections,
) -> Result<(Connection, Arc<ConnForwards>), Failure> {
    let timeout = Duration::from_secs(p.timing.connect_timeout.max(1).into());
    if let Some(proxy) = &p.proxy {
        let tcp = drive(
            tokio::time::timeout(timeout, through_proxy(proxy, &p.host, p.port, p.family)),
            inbox,
            ctx,
        )
        .await?
        .map_err(|_| {
            Failure::Refused(format!(
                "Timed out connecting to {} through the proxy {}.",
                p.host, proxy.host
            ))
        })?;
        let tcp = match tcp {
            Ok(tcp) => tcp,
            Err(ProxyError::Auth(why)) => {
                // Only a login the UI gave can be refused; a proxy that
                // wants one when none was given just says so.
                if !proxy.user.is_empty() {
                    let channel = ctx.channel;
                    ctx.send(Message::ProxyAuth {
                        channel,
                        accepted: false,
                    })
                    .await?;
                }
                return Err(Failure::Refused(why));
            }
            Err(ProxyError::Other(why)) => return Err(Failure::Refused(why)),
        };
        if !proxy.user.is_empty() {
            let channel = ctx.channel;
            ctx.send(Message::ProxyAuth {
                channel,
                accepted: true,
            })
            .await?;
        }
        let _ = tcp.set_nodelay(true);
        // The address is the proxy's, not the host's: Mosh looks the host
        // up itself.
        return handshake(p, preferred, tcp, None, inbox, ctx, connections).await;
    }
    let tcp = drive(
        tokio::time::timeout(timeout, connect_tcp(&p.host, p.port, p.family)),
        inbox,
        ctx,
    )
    .await?
    .map_err(|_| Failure::Refused(format!("Timed out connecting to {}.", p.host)))?
    .map_err(|e| Failure::Refused(format!("Could not connect to {}: {e}.", p.host)))?;
    let _ = tcp.set_nodelay(true);
    let peer = tcp.peer_addr().ok().map(|a| a.ip());
    handshake(p, preferred, tcp, peer, inbox, ctx, connections).await
}

/// Why the way through a proxy failed.
enum ProxyError {
    /// The proxy wants a login, or refused the one given.
    Auth(String),
    Other(String),
}

/// Connects to `proxy` and asks it for `host`:`port`; returns the
/// connection, the server's bytes next. The proxy's replies are parsed
/// here, in the sandbox ([`ferossh_forward::proxy`]).
async fn through_proxy(
    proxy: &ProxyServer,
    host: &str,
    port: u16,
    family: Family,
) -> Result<TcpStream, ProxyError> {
    let kind = match proxy.kind {
        ProxyKind::Socks5 => proxy::Kind::Socks5,
        ProxyKind::Http => proxy::Kind::Http,
    };
    // No login without a user; a missing password is an empty one.
    let login = (!proxy.user.is_empty()).then(|| {
        let word = proxy
            .password
            .0
            .first()
            .map_or(Default::default(), String::as_str);
        (proxy.user.as_str(), word)
    });
    let mut client = proxy::Client::new(kind, host, port, login).ok_or_else(|| {
        ProxyError::Other(format!(
            "FeroSSH cannot ask the proxy for {host}: the host name, user name or \
             password is not one the proxy protocol can carry."
        ))
    })?;
    let mut tcp = connect_tcp(&proxy.host, proxy.port, family)
        .await
        .map_err(|e| {
            ProxyError::Other(format!(
                "Could not connect to the proxy {}: {e}.",
                proxy.host
            ))
        })?;
    let lost = |e: std::io::Error| {
        ProxyError::Other(format!(
            "The proxy {} closed the connection before it was through: {e}.",
            proxy.host
        ))
    };
    let mut send = client.start();
    let mut buf = Vec::new();
    loop {
        tcp.write_all(&send).await.map_err(lost)?;
        buf.resize(client.wants(), 0);
        tcp.read_exact(&mut buf).await.map_err(lost)?;
        match client.feed(&buf) {
            proxy::Step::Send(bytes) => send = bytes,
            // Only what the handshake wanted was read, so `rest` is empty.
            proxy::Step::Open { .. } => return Ok(tcp),
            proxy::Step::AuthFailed(why) => return Err(ProxyError::Auth(why)),
            proxy::Step::Failed(why) => return Err(ProxyError::Other(why)),
        }
    }
}

/// Connects to the first address of `host` that answers, of the IP
/// versions `family` allows (`AddressFamily`).
async fn connect_tcp(host: &str, port: u16, family: Family) -> std::io::Result<TcpStream> {
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await?
        .filter(|a| match family {
            Family::Any => true,
            Family::V4 => a.is_ipv4(),
            Family::V6 => a.is_ipv6(),
        })
        .collect();
    let mut last = std::io::Error::new(
        std::io::ErrorKind::NotFound,
        match family {
            Family::Any => "no address found",
            Family::V4 => "no IPv4 address found (AddressFamily inet)",
            Family::V6 => "no IPv6 address found (AddressFamily inet6)",
        },
    );
    for addr in addrs {
        match TcpStream::connect(addr).await {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Runs key exchange with `preferred` over `stream`.
async fn handshake(
    p: &Params,
    preferred: Preferred,
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    peer: Option<std::net::IpAddr>,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
    connections: &Connections,
) -> Result<(Connection, Arc<ConnForwards>), Failure> {
    let config = Arc::new(client::Config {
        client_id: SshId::Standard(CLIENT_ID.into()),
        preferred,
        keepalive_interval: (p.timing.alive_interval > 0)
            .then(|| Duration::from_secs(p.timing.alive_interval.into())),
        keepalive_max: p.timing.alive_max.max(1) as usize,
        nodelay: true,
        ..Default::default()
    });
    let (alive_tx, alive) = watch::channel(());
    let forwards = Arc::new(ConnForwards {
        connection: ctx.channel,
        remote: Mutex::default(),
        x11: Mutex::default(),
        alive,
        agent: AtomicBool::new(false),
        host: p.host.clone(),
        peer,
        family: p.family,
    });
    let handler = Client {
        channel: ctx.channel,
        out: ctx.out.clone(),
        shared: ctx.shared.clone(),
        forwards: forwards.clone(),
        connections: connections.clone(),
        _alive: alive_tx,
    };
    let handle = drive(client::connect_stream(config, stream, handler), inbox, ctx).await??;
    Ok((handle, forwards))
}

/// Opens a shell on `conn` and pumps it until it closes. Disconnects the
/// connection when this was its last shell.
async fn shell(
    conn: Arc<Connection>,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<Ended, Failure> {
    let result = pump(&conn, inbox, ctx).await;
    // The last shell on the connection closes it.
    let ended = match Arc::into_inner(conn) {
        Some(handle) => {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
            Ended::Connection
        }
        None => Ended::Shell,
    };
    match result {
        Ok(Closed::Lost) => Ok(Ended::Lost),
        // The user closed the tab: nothing to report as an error.
        Ok(Closed::Channel) | Err(Failure::Cancelled) => Ok(ended),
        Err(e) => Err(e),
    }
}

async fn pump(
    conn: &Connection,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<Closed, Failure> {
    if let Some(mosh) = ctx.options.mosh.clone() {
        return mosh_session(conn, *mosh, inbox, ctx).await;
    }
    let channel = drive(conn.channel_open_session(), inbox, ctx).await??;
    // X11 first: its reply is then the first one back.
    let mut x11_reply = false;
    if let Some(real) = ctx.x11.take() {
        let screen = real.screen;
        if let Some(fake) = ctx.forwards.as_ref().and_then(|f| f.x11_cookie(real)) {
            channel
                .request_x11(true, false, MIT_COOKIE, hex(&fake), screen)
                .await?;
            x11_reply = true;
        }
    }
    // Variables first, as OpenSSH sends them; a server that does not take
    // one (sshd's AcceptEnv) just ignores it.
    for (name, value) in &ctx.options.env {
        channel
            .set_env(false, name.as_str(), value.as_str())
            .await?;
    }
    if ctx.options.agent
        && let Some(f) = &ctx.forwards
    {
        f.agent.store(true, Ordering::Relaxed);
        channel.agent_forward(false).await?;
    }
    let (cols, rows) = ctx.size;
    if ctx.options.pty {
        channel
            .request_pty(false, TERM, cols.into(), rows.into(), 0, 0, &[])
            .await?;
    }
    match &ctx.options.command {
        Some(command) => channel.exec(true, command.as_bytes()).await?,
        None => channel.request_shell(true).await?,
    }
    ctx.send(connected(ctx)).await?;

    let (mut read, write) = channel.split();
    // Writes can wait for the server's window; they run in their own task
    // so output keeps flowing meanwhile.
    let (input, mut input_rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(msg) = input_rx.recv().await {
            let sent = match msg {
                Message::Data { bytes, .. } => write.data(&bytes[..]).await,
                Message::Resize { cols, rows, .. } => {
                    write.window_change(cols.into(), rows.into(), 0, 0).await
                }
                _ => Ok(()),
            };
            if sent.is_err() {
                break;
            }
        }
    });

    let mut zm = Zmodem::new(ctx.channel);
    let mut heard = Instant::now();
    let result = loop {
        let wake = zm.patience().map(|p| heard + p);
        let step = tokio::select! {
            msg = read.wait() => match msg {
                Some(ChannelMsg::Data { data }) => {
                    heard = Instant::now();
                    zm.on_server(&data)
                }
                // stderr is never part of a transfer.
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    let mut step = zmodem::Step::default();
                    step.show(ctx.channel, &data);
                    step
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    ctx.send(Message::ExitStatus { channel: ctx.channel, code: exit_status }).await?;
                    continue;
                }
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    ctx.notice(format!("The remote shell was killed by signal {signal_name:?}.")).await?;
                    continue;
                }
                Some(ChannelMsg::Success) if x11_reply => {
                    x11_reply = false;
                    continue;
                }
                Some(ChannelMsg::Failure) if x11_reply => {
                    x11_reply = false;
                    ctx.notice(
                        "The server refused X11 forwarding (its sshd_config may say \
                         X11Forwarding no, or xauth is not installed there)."
                            .into(),
                    )
                    .await?;
                    continue;
                }
                Some(ChannelMsg::Close) => break Ok(Closed::Channel),
                // The connection's task ended with the channel still open.
                None => break Ok(Closed::Lost),
                Some(_) => continue,
            },
            msg = inbox.recv() => match msg {
                None | Some(Message::Close { .. }) => break Err(Failure::Cancelled),
                Some(m) => match zm.on_ui(&m) {
                    Some(step) => {
                        heard = Instant::now();
                        step
                    }
                    None if matches!(m, Message::Data { .. } | Message::Resize { .. }) => {
                        // The writer stops when a write fails; how the
                        // channel ended is then read above.
                        let _ = input.send(m).await;
                        continue;
                    }
                    None => continue,
                },
            },
            () = sleep_until(wake) => {
                heard = Instant::now();
                zm.timeout()
            }
        };
        run_step(step, ctx, &input).await?;
    };
    writer.abort();
    result
}

/// Starts `mosh-server` on an exec channel, reads where it listens and
/// its key, and runs the session over UDP from there. The SSH connection
/// stays open beside it, for File transfer and forwards while it lasts;
/// the Mosh session does not need it.
async fn mosh_session(
    conn: &Connection,
    mosh: MoshOptions,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<Closed, Failure> {
    use ferossh_mosh::bootstrap::{BootError, MAX_OUTPUT, ServerCommand, parse_connect};

    let command = ServerCommand {
        program: mosh.server.clone(),
        ports: mosh.ports,
        command: ctx.options.command.clone(),
    };
    let Some(line) = command.line() else {
        return Err(Failure::Refused(format!(
            "Not a program name: {:?}. Set the Mosh server to mosh-server or a path.",
            mosh.server
        )));
    };
    let channel = drive(conn.channel_open_session(), inbox, ctx).await??;
    channel.exec(true, line.as_bytes()).await?;
    let (mut read, _write) = channel.split();
    let output = async {
        let mut got = Vec::new();
        while got.len() < MAX_OUTPUT {
            match read.wait().await {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    got.extend_from_slice(&data);
                    // mosh-server prints the line, then detaches; stop as
                    // soon as it is complete.
                    if got.windows(13).any(|w| w == b"MOSH CONNECT ")
                        && got.ends_with(b"\n")
                        && parse_connect(&got).is_ok()
                    {
                        break;
                    }
                }
                Some(ChannelMsg::Close | ChannelMsg::Eof) | None => break,
                Some(_) => {}
            }
        }
        got
    };
    let got = drive(tokio::time::timeout(MOSH_START, output), inbox, ctx)
        .await?
        .map_err(|_| Failure::Refused("mosh-server did not answer in time.".into()))?;
    let connect = parse_connect(&got).map_err(|e| {
        Failure::Refused(match e {
            BootError::NoConnect(said) if said.is_empty() => {
                "mosh-server did not start: is Mosh installed on the server?".to_owned()
            }
            BootError::NoConnect(said) => {
                format!("mosh-server did not start: {said} (is Mosh installed on the server?)")
            }
            BootError::Malformed => "mosh-server's MOSH CONNECT line is malformed.".to_owned(),
        })
    })?;
    let Some(f) = ctx.forwards.clone() else {
        return Err(Failure::Refused("The connection is gone.".into()));
    };
    let ip = match f.peer {
        Some(ip) => ip,
        None => {
            // Through a jump host or a ProxyCommand: Mosh's UDP goes to the
            // host itself, as the mosh script does.
            let found = tokio::net::lookup_host((f.host.as_str(), connect.port))
                .await
                .map_err(|e| Failure::Refused(format!("Mosh: could not look up {}: {e}.", f.host)))?
                .find(|a| match f.family {
                    Family::Any => true,
                    Family::V4 => a.is_ipv4(),
                    Family::V6 => a.is_ipv6(),
                });
            found
                .map(|a| a.ip())
                .ok_or_else(|| Failure::Refused(format!("Mosh: no address for {}.", f.host)))?
        }
    };
    let addr = std::net::SocketAddr::new(ip, connect.port);
    let (kex, _, host_key) = with(&ctx.shared, |s| s.names.clone())
        .flatten()
        .unwrap_or_default();
    ctx.send(Message::Connected {
        channel: ctx.channel,
        kex,
        cipher: format!("Mosh aes128-ocb, UDP {addr}"),
        host_key,
    })
    .await?;
    let target = crate::mosh::Target {
        addr,
        key: connect.key,
    };
    let ended = crate::mosh::run(ctx.channel, target, ctx.size, inbox, &ctx.out).await;
    // Whatever became of the SSH connection meanwhile is not why the
    // Mosh session ended.
    with(&ctx.shared, |s| {
        s.lost = None;
        s.server_said = None;
    });
    match ended {
        Ok(crate::mosh::Ended::Server) => Ok(Closed::Channel),
        Ok(crate::mosh::Ended::Closed) => Err(Failure::Cancelled),
        Err(why) => Err(Failure::Refused(why)),
    }
}

/// How long `mosh-server new` may take to print where it listens.
const MOSH_START: Duration = Duration::from_secs(20);

/// Carries out a ZMODEM step: messages (terminal output among them) to the
/// UI, and bytes for the server to the writer. A writer that has gone is
/// left to the channel's end, read in the pump.
async fn run_step(
    step: zmodem::Step,
    ctx: &Ctx,
    input: &mpsc::Sender<Message>,
) -> Result<(), Failure> {
    for msg in step.ui {
        ctx.send(msg).await?;
    }
    for chunk in step.server.chunks(MAX_DATA_CHUNK) {
        let msg = Message::Data {
            channel: ctx.channel,
            bytes: chunk.to_vec(),
        };
        if input.send(msg).await.is_err() {
            break;
        }
    }
    Ok(())
}

fn method_names(methods: &MethodSet) -> String {
    methods
        .iter()
        .map(<&str>::from)
        .collect::<Vec<_>>()
        .join(",")
}

/// The outcome of one method: done, or the methods the server still
/// accepts.
enum Step {
    Done,
    Continue(MethodSet),
}

fn step(result: AuthResult) -> Step {
    match result {
        AuthResult::Success => Step::Done,
        AuthResult::Failure {
            remaining_methods, ..
        } => Step::Continue(remaining_methods),
    }
}

async fn authenticate(
    handle: &mut Handle<Client>,
    p: &Params,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<(), Failure> {
    let mut methods = match step(drive(handle.authenticate_none(&p.user), inbox, ctx).await??) {
        Step::Done => return Ok(()),
        Step::Continue(m) => m,
    };
    let mut keys_tried = false;
    // OpenSSH's order: Kerberos before keys, once.
    let mut gss_tried = !p.gssapi;
    let (mut kbd_tries, mut pw_tries) = (0, 0);
    loop {
        let next = if !gss_tried && methods.contains(&MethodKind::GssapiWithMic) {
            gss_tried = true;
            kerberos(handle, p, inbox, ctx, &methods).await?
        } else if !keys_tried && methods.contains(&MethodKind::PublicKey) {
            keys_tried = true;
            public_keys(handle, p, inbox, ctx).await?
        } else if kbd_tries < MAX_ATTEMPTS && methods.contains(&MethodKind::KeyboardInteractive) {
            kbd_tries += 1;
            keyboard_interactive(handle, p, inbox, ctx).await?
        } else if pw_tries < MAX_ATTEMPTS && methods.contains(&MethodKind::Password) {
            pw_tries += 1;
            password(handle, p, inbox, ctx).await?
        } else {
            return Err(Failure::Refused(format!(
                "Permission denied ({}).",
                method_names(&methods)
            )));
        };
        match next {
            Step::Done => return Ok(()),
            // An empty list after a failure means the server is gone.
            Step::Continue(m) if m.is_empty() => {
                return Err(Failure::Refused("The server ended authentication.".into()));
            }
            Step::Continue(m) => methods = m,
        }
    }
}

async fn public_keys(
    handle: &mut Handle<Client>,
    p: &Params,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<Step, Failure> {
    let (tx, rx) = oneshot::channel();
    with(&ctx.shared, |s| s.identities = Some(tx));
    ctx.send(Message::IdentitiesRequest {
        channel: ctx.channel,
    })
    .await?;
    let keys = drive(rx, inbox, ctx)
        .await?
        .map_err(|_| Failure::Cancelled)?;
    let mut remaining = MethodSet::empty();
    remaining.push(MethodKind::PublicKey);
    let mut rsa_hash: Option<Option<HashAlg>> = None;
    for blob in keys {
        // A user certificate, or a plain key.
        let (key, cert) = match Certificate::from_bytes(&blob) {
            Ok(cert) => (PublicKey::new(cert.public_key().clone(), ""), Some(cert)),
            Err(_) => match PublicKey::from_bytes(&blob) {
                Ok(key) => (key, None),
                Err(_) => continue,
            },
        };
        let hash = if key.algorithm().is_rsa() {
            if rsa_hash.is_none() {
                let best = drive(handle.best_supported_rsa_hash(), inbox, ctx)
                    .await?
                    .ok()
                    .flatten()
                    .flatten();
                // A server without server-sig-algs gets SHA-512 anyway;
                // SHA-1 (ssh-rsa) is never offered.
                rsa_hash = Some(Some(best.unwrap_or(HashAlg::Sha512)));
            }
            rsa_hash.flatten()
        } else {
            None
        };
        let mut signer = IpcSigner {
            channel: ctx.channel,
            out: ctx.out.clone(),
            shared: ctx.shared.clone(),
        };
        let result = match cert {
            Some(cert) => {
                drive(
                    handle.authenticate_certificate_with(&p.user, cert, hash, &mut signer),
                    inbox,
                    ctx,
                )
                .await??
            }
            None => {
                drive(
                    handle.authenticate_publickey_with(&p.user, key, hash, &mut signer),
                    inbox,
                    ctx,
                )
                .await??
            }
        };
        match result {
            AuthResult::Success => return Ok(Step::Done),
            AuthResult::Failure {
                remaining_methods,
                partial_success: true,
            } => return Ok(Step::Continue(remaining_methods)),
            AuthResult::Failure {
                remaining_methods, ..
            } => {
                remaining = remaining_methods;
                if !remaining.contains(&MethodKind::PublicKey) {
                    break;
                }
            }
        }
    }
    Ok(Step::Continue(remaining))
}

/// The DER encoding of Kerberos 5's GSS-API mechanism OID,
/// 1.2.840.113554.1.2.2: the only mechanism FeroSSH offers.
pub const KRB5_OID: &[u8] = &[
    0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02,
];

/// Most security context rounds before the worker gives up: Kerberos
/// takes one, or two with mutual authentication.
const MAX_GSS_STEPS: u32 = 8;

/// Runs a Kerberos login's GSS-API steps in the UI, which holds the
/// user's tickets: the worker never sees a ticket or a session key, only
/// the opaque tokens the server sees too.
struct IpcGssapi {
    channel: ChannelId,
    out: mpsc::Sender<Message>,
    shared: SharedRef,
    steps: u32,
}

/// Why a Kerberos login stopped without an answer from the server.
#[derive(Debug)]
enum GssStop {
    /// No Kerberos login on this PC for this host; try other methods.
    Declined,
    Failed(Failure),
}

impl From<russh::SendError> for GssStop {
    fn from(e: russh::SendError) -> Self {
        GssStop::Failed(e.into())
    }
}

impl russh::GssapiAuthenticator for IpcGssapi {
    type Error = GssStop;

    fn gssapi_step(
        &mut self,
        selected_mechanism: Option<Vec<u8>>,
        input_token: Option<Vec<u8>>,
        mic_data: Vec<u8>,
    ) -> impl Future<Output = Result<russh::GssapiStep, GssStop>> + Send {
        self.steps += 1;
        let first = selected_mechanism.is_some();
        // RFC 4462 3.3: the server must pick a mechanism the client offered.
        let wrong = selected_mechanism.is_some_and(|m| m != KRB5_OID);
        let too_many = self.steps > MAX_GSS_STEPS;
        async move {
            if wrong || too_many {
                return Err(GssStop::Declined);
            }
            let (tx, rx) = oneshot::channel();
            with(&self.shared, |s| s.gssapi = Some(tx));
            let msg = Message::GssapiStep {
                channel: self.channel,
                first,
                token: input_token.unwrap_or_default(),
                mic_data,
            };
            self.out
                .send(msg)
                .await
                .map_err(|_| GssStop::Failed(Failure::Cancelled))?;
            match rx.await.map_err(|_| GssStop::Failed(Failure::Cancelled))? {
                GssReply::Continue(token) => Ok(russh::GssapiStep::Continue { token }),
                GssReply::Complete { token, mic } => Ok(russh::GssapiStep::Complete {
                    token: (!token.is_empty()).then_some(token),
                    mic: Some(mic),
                }),
                GssReply::Failed => Err(GssStop::Declined),
            }
        }
    }
}

/// A Kerberos login through the UI's tickets. When the UI has none for
/// this host, the other methods follow; a token the server rejects ends
/// the method as any refused login does.
async fn kerberos(
    handle: &mut Handle<Client>,
    p: &Params,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
    methods: &MethodSet,
) -> Result<Step, Failure> {
    let mut gss = IpcGssapi {
        channel: ctx.channel,
        out: ctx.out.clone(),
        shared: ctx.shared.clone(),
        steps: 0,
    };
    let result = drive(
        handle.authenticate_gssapi_with_mic(&p.user, vec![KRB5_OID.to_vec()], &mut gss),
        inbox,
        ctx,
    )
    .await?;
    match result {
        Ok(result) => Ok(step(result)),
        Err(GssStop::Failed(e)) => Err(e),
        // The server still waits in the GSS-API exchange; the next
        // method's request starts over, as OpenSSH's client does.
        Err(GssStop::Declined) => Ok(Step::Continue(methods.clone())),
    }
}

/// Asks the user through the UI and waits for the answers.
async fn ask(
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
    kind: PromptKind,
    name: String,
    instructions: String,
    fields: Vec<PromptField>,
) -> Result<Secrets, Failure> {
    let expected = fields.len();
    let (tx, rx) = oneshot::channel();
    with(&ctx.shared, |s| s.answers = Some(tx));
    ctx.send(Message::Prompt {
        channel: ctx.channel,
        kind,
        name,
        instructions,
        fields,
    })
    .await?;
    let answers = drive(rx, inbox, ctx)
        .await?
        .map_err(|_| Failure::Cancelled)?;
    if answers.0.len() != expected {
        return Err(Failure::Cancelled);
    }
    Ok(answers)
}

async fn keyboard_interactive(
    handle: &mut Handle<Client>,
    p: &Params,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<Step, Failure> {
    let mut reply = drive(
        handle.authenticate_keyboard_interactive_start(&p.user, None),
        inbox,
        ctx,
    )
    .await??;
    loop {
        reply = match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(Step::Done),
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods, ..
            } => return Ok(Step::Continue(remaining_methods)),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let mut answers = if prompts.is_empty() {
                    Secrets::default()
                } else {
                    let fields = prompts
                        .into_iter()
                        .map(|p| PromptField {
                            text: p.prompt,
                            echo: p.echo,
                        })
                        .collect();
                    let kind = PromptKind::KeyboardInteractive;
                    ask(inbox, ctx, kind, name, instructions, fields).await?
                };
                let responses = std::mem::take(&mut answers.0);
                drive(
                    handle.authenticate_keyboard_interactive_respond(responses),
                    inbox,
                    ctx,
                )
                .await??
            }
        };
    }
}

async fn password(
    handle: &mut Handle<Client>,
    p: &Params,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<Step, Failure> {
    let field = PromptField {
        text: format!("{}@{}'s password: ", p.user, p.host),
        echo: false,
    };
    let kind = PromptKind::Password;
    let mut answers = ask(inbox, ctx, kind, String::new(), String::new(), vec![field]).await?;
    let password = answers.0.pop().unwrap_or_default();
    // russh wipes the password when its auth method is dropped.
    let result = drive(handle.authenticate_password(&p.user, password), inbox, ctx).await??;
    Ok(step(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_certificates_reject_critical_options_but_allow_extensions()
    -> Result<(), Box<dyn std::error::Error>> {
        use russh::keys::ssh_key::{PrivateKey, certificate::Builder, private::Ed25519Keypair};

        let ca = PrivateKey::from(Ed25519Keypair::from_seed(&[1; 32]));
        let host = PrivateKey::from(Ed25519Keypair::from_seed(&[2; 32]));
        for (critical, extension) in [(false, false), (false, true), (true, false)] {
            let mut builder = Builder::new(
                vec![3; 32],
                host.public_key().key_data().clone(),
                0,
                u64::MAX,
            )?;
            builder.cert_type(CertType::Host)?;
            builder.valid_principal("host.example")?;
            if critical {
                builder.critical_option("unknown@example", "required")?;
            }
            if extension {
                builder.extension("unknown@example", "optional")?;
            }
            let cert = builder.sign(&ca)?;
            let cert = Certificate::from_bytes(&cert.to_bytes()?)?;
            // The rejection must be due to the critical option, not a bad
            // signature or malformed certificate.
            cert.verify_signature()?;
            assert_eq!(
                cert_problem(&cert).as_deref(),
                critical.then_some("contains unsupported critical options")
            );
        }
        Ok(())
    }

    #[test]
    fn signatures_are_ssh_strings() {
        let sig = empty_signature("ssh-ed25519");
        assert_eq!(sig_algorithm(&sig), Some(&b"ssh-ed25519"[..]));
        let data = append_signature(b"ab".to_vec(), &sig);
        assert_eq!(data.get(..2), Some(&b"ab"[..]));
        assert_eq!(data.len(), 2 + 4 + sig.len());
        assert_eq!(sig_algorithm(b"\0\0\0\x09short"), None);
    }
}
