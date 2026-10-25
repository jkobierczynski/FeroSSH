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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use ferossh_forward::x11::{MIT_COOKIE, hex};
use ferossh_ipc::{
    ChannelId, MAX_DATA_CHUNK, Message, PromptField, PromptKind, Secrets, Timing, WORKER_IDS,
    X11Auth,
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
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};

use crate::crypto_policy::{
    MIN_RSA_BITS, legacy_would_help, preferred_for, terrapin_prone, with_legacy, without_terrapin,
};
use crate::forward::{self, Incoming};

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
    /// A shell, SFTP or a remote forward's listener: few messages, and a
    /// full queue may make the UI wait.
    Session(mpsc::Sender<Message>),
    /// A forwarded stream: flow control bounds what the UI sends, and one
    /// slow stream must never hold up the pipe.
    Stream(mpsc::UnboundedSender<Message>),
}

impl Route {
    fn is_closed(&self) -> bool {
        match self {
            Route::Session(tx) => tx.is_closed(),
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
/// connection, and the last one to close disconnects it.
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
/// type, a bad CA signature, or a CA key or signature outside the policy.
fn cert_problem(cert: &Certificate) -> Option<String> {
    if cert.cert_type() != CertType::Host {
        return Some("is a user certificate, not a host certificate".into());
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
            AgentIdentity::Certificate { certificate, .. } => {
                (Vec::new(), certificate.algorithm().as_str().to_owned())
            }
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

/// Ends a session: reports why to the UI.
pub async fn run(
    channel: ChannelId,
    params: Params,
    mut inbox: mpsc::Receiver<Message>,
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
        forwards: None,
    };
    let result = session(&params, &mut inbox, &mut ctx, &connections).await;
    let server_said = with(&shared, |s| s.server_said.take()).flatten();
    let lost = with(&shared, |s| s.lost.take()).flatten();
    let reason = match (result, server_said) {
        (_, Some(said)) if !said.trim().is_empty() => {
            format!("The server closed the connection: {}", said.trim())
        }
        (Ok(Ended::Connection), _) => lost.unwrap_or_else(|| "Connection closed.".to_owned()),
        (Ok(Ended::Shell), _) => SHELL_CLOSED.to_owned(),
        (Err(e), _) => e.to_string(),
    };
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}

/// Opens another shell on the connection `connection` made, for a new tab
/// or pane: no new login. Reports why it ended, like [`run`].
pub async fn open(
    channel: ChannelId,
    connection: ChannelId,
    size: (u16, u16),
    x11: Option<X11Auth>,
    mut inbox: mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let mut ctx = Ctx {
        channel,
        out: out.clone(),
        shared: Arc::default(),
        size,
        x11,
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
}

/// The reason given when a shell ends while its connection stays open.
const SHELL_CLOSED: &str = "The shell closed; the connection stays open for the other tabs.";

async fn session(
    p: &Params,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
    connections: &Connections,
) -> Result<Ended, Failure> {
    // A server without strict key exchange may not use ChaCha20, but that
    // only shows once it has chosen it: connect again without it.
    let mut preferred = preferred_for(&p.host_key_algs);
    if p.legacy {
        preferred = with_legacy(preferred);
    }
    let first = connect(p, preferred.clone(), inbox, ctx, connections).await;
    let (mut handle, forwards) = match first {
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
    };

    authenticate(&mut handle, p, inbox, ctx).await?;
    let conn = Arc::new(handle);
    let names = with(&ctx.shared, |s| s.names.clone()).flatten();
    connections.insert(ctx.channel, &conn, names, forwards.clone());
    ctx.forwards = Some(forwards);
    shell(conn, inbox, ctx).await
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
    let config = Arc::new(client::Config {
        client_id: SshId::Standard(CLIENT_ID.into()),
        preferred,
        keepalive_interval: (p.timing.alive_interval > 0)
            .then(|| Duration::from_secs(p.timing.alive_interval.into())),
        keepalive_max: p.timing.alive_max.max(1) as usize,
        nodelay: true,
        ..Default::default()
    });
    let addr = (p.host.as_str(), p.port);
    let tcp = drive(
        tokio::time::timeout(
            Duration::from_secs(p.timing.connect_timeout.max(1).into()),
            TcpStream::connect(addr),
        ),
        inbox,
        ctx,
    )
    .await?
    .map_err(|_| Failure::Refused(format!("Timed out connecting to {}.", p.host)))?
    .map_err(|e| Failure::Refused(format!("Could not connect to {}: {e}.", p.host)))?;
    let _ = tcp.set_nodelay(true);
    let (alive_tx, alive) = watch::channel(());
    let forwards = Arc::new(ConnForwards {
        connection: ctx.channel,
        remote: Mutex::default(),
        x11: Mutex::default(),
        alive,
    });
    let handler = Client {
        channel: ctx.channel,
        out: ctx.out.clone(),
        shared: ctx.shared.clone(),
        forwards: forwards.clone(),
        connections: connections.clone(),
        _alive: alive_tx,
    };
    let handle = drive(client::connect_stream(config, tcp, handler), inbox, ctx).await??;
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
        // The user closed the tab: nothing to report as an error.
        Ok(()) | Err(Failure::Cancelled) => Ok(ended),
        Err(e) => Err(e),
    }
}

async fn pump(
    conn: &Connection,
    inbox: &mut mpsc::Receiver<Message>,
    ctx: &mut Ctx,
) -> Result<(), Failure> {
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
    let (cols, rows) = ctx.size;
    channel
        .request_pty(false, TERM, cols.into(), rows.into(), 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;
    let (kex, cipher, host_key) = with(&ctx.shared, |s| s.names.clone())
        .flatten()
        .unwrap_or_default();
    ctx.send(Message::Connected {
        channel: ctx.channel,
        kex,
        cipher,
        host_key,
    })
    .await?;

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

    let result = loop {
        tokio::select! {
            msg = read.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    for chunk in data.chunks(MAX_DATA_CHUNK) {
                        ctx.send(Message::Data { channel: ctx.channel, bytes: chunk.to_vec() }).await?;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    ctx.send(Message::ExitStatus { channel: ctx.channel, code: exit_status }).await?;
                }
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    ctx.notice(format!("The remote shell was killed by signal {signal_name:?}.")).await?;
                }
                Some(ChannelMsg::Success) if x11_reply => x11_reply = false,
                Some(ChannelMsg::Failure) if x11_reply => {
                    x11_reply = false;
                    ctx.notice(
                        "The server refused X11 forwarding (its sshd_config may say \
                         X11Forwarding no, or xauth is not installed there)."
                            .into(),
                    )
                    .await?;
                }
                Some(ChannelMsg::Close) | None => break Ok(()),
                Some(_) => {}
            },
            msg = inbox.recv() => match msg {
                None | Some(Message::Close { .. }) => break Err(Failure::Cancelled),
                Some(m @ (Message::Data { .. } | Message::Resize { .. })) => {
                    if input.send(m).await.is_err() {
                        break Ok(());
                    }
                }
                Some(_) => {}
            },
        }
    };
    writer.abort();
    result
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
    let (mut kbd_tries, mut pw_tries) = (0, 0);
    loop {
        let next = if !keys_tried && methods.contains(&MethodKind::PublicKey) {
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
        let Ok(key) = PublicKey::from_bytes(&blob) else {
            continue;
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
        let result = drive(
            handle.authenticate_publickey_with(&p.user, key, hash, &mut signer),
            inbox,
            ctx,
        )
        .await??;
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
    fn signatures_are_ssh_strings() {
        let sig = empty_signature("ssh-ed25519");
        assert_eq!(sig_algorithm(&sig), Some(&b"ssh-ed25519"[..]));
        let data = append_signature(b"ab".to_vec(), &sig);
        assert_eq!(data.get(..2), Some(&b"ab"[..]));
        assert_eq!(data.len(), 2 + 4 + sig.len());
        assert_eq!(sig_algorithm(b"\0\0\0\x09short"), None);
    }
}
