//! Message types and framing for the pipe between the UI process and the
//! sandboxed SSH worker.
//!
//! Every frame is a little-endian `u32` body length followed by the body.
//! The body starts with the protocol version and a message tag; the rest is
//! the message's fields in a fixed order. Integers are little-endian;
//! strings and byte strings are a `u32` length and the bytes; lists are a
//! `u32` count and the items. Both sides treat the other as untrusted:
//! lengths are capped, unknown tags and versions are errors, strings must be
//! UTF-8, booleans must be 0 or 1, and decoding never panics. Every accepted
//! frame re-encodes to the same bytes, which the fuzz target checks.
//!
//! The worker holds the network connection; the UI holds everything else.
//! So the worker asks the UI to verify the host key, to list public keys, to
//! sign the user-auth request and to ask the user for passwords, and the UI
//! answers. One `channel` is one shell; the first channel on a host makes
//! the SSH connection, and [`Message::OpenChannel`] opens further shells on
//! that connection for new tabs and panes.
//!
//! File transfer runs as SFTP on another channel of the same connection
//! ([`Message::SftpOpen`]). The worker speaks the SFTP protocol to the
//! server and the UI asks it for one operation at a time
//! ([`Message::SftpRequest`]); file contents travel in those requests and
//! their replies, because the worker cannot open files. Paths and names are
//! bytes, as the server sends them: a name need not be UTF-8.
//!
//! ZMODEM (`rz` and `sz` on the server) runs inside a shell's channel: the
//! worker spots a transfer's start in the output, holds the output back
//! and asks the UI ([`Message::ZmodemStart`]), which turns it down, or
//! names the files to send, or takes the files the server sends. As with
//! SFTP, file contents cross the pipe; the worker opens no file.
//!
//! Port forwards are streams: one channel per forwarded TCP connection,
//! carrying [`Message::Data`], [`Message::Eof`] and [`Message::Close`]. The
//! UI holds the sockets (the AppContainer cannot use loopback), the worker
//! holds the SSH channels. Each direction of a stream may have at most
//! [`STREAM_WINDOW`] bytes in flight; the receiver hands credit back with
//! [`Message::Window`] as it delivers them, so one slow socket never stalls
//! the pipe for the others. Channel ids come from three ranges: the
//! window's below [`FORWARD_IDS`], the UI's forwarding engine's from there
//! to [`WORKER_IDS`], and the worker's (connections the server opens to
//! this side) above.

use std::fmt;

use zeroize::Zeroize;

/// Protocol version carried in every frame. Bump on any wire change.
pub const PROTOCOL_VERSION: u8 = 18;

/// Largest frame body accepted, in bytes. Terminal and SFTP data is chunked
/// below this, so anything larger is a bug or an attack.
pub const MAX_BODY_LEN: usize = 1 << 20;

/// Largest terminal data chunk either side puts in one [`Message::Data`].
pub const MAX_DATA_CHUNK: usize = 64 * 1024;

const LEN_PREFIX: usize = 4;

/// Bytes either side of a forwarded stream may send before the other
/// side's [`Message::Window`] gives credit back.
pub const STREAM_WINDOW: u32 = 512 * 1024;

/// The first channel id of the UI's forwarding engine; the window's ids
/// stay below it.
pub const FORWARD_IDS: ChannelId = 0x4000_0000;

/// The first channel id the worker hands out, for connections the server
/// opens (remote forwards and X11).
pub const WORKER_IDS: ChannelId = 0x8000_0000;

/// Whether a channel belongs to the forwarding engine rather than to a
/// window's pane.
pub fn is_forward_channel(channel: ChannelId) -> bool {
    channel >= FORWARD_IDS
}

/// Identifies one SSH channel (terminal, SFTP, forward) inside a worker.
pub type ChannelId = u32;

/// What a [`Message::Prompt`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// The server accepts the `password` method.
    Password,
    /// A keyboard-interactive round: one-time codes, MFA, PAM passwords.
    KeyboardInteractive,
}

/// One question in a [`Message::Prompt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptField {
    /// The server's (or worker's) text, not yet sanitised.
    pub text: String,
    /// Whether the answer may be shown while typed.
    pub echo: bool,
}

/// Answers to a prompt: passwords and codes. Wiped when dropped and never
/// printed by `Debug`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secrets(pub Vec<String>);

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secrets(<{} redacted>)", self.0.len())
    }
}

impl Drop for Secrets {
    fn drop(&mut self) {
        for s in &mut self.0 {
            s.zeroize();
        }
    }
}

/// What X11 forwarding on a shell needs: the screen, and the authorisation
/// the local X server expects (empty when it needs none). The worker gives
/// the server a fake cookie and puts this one in its place when a client
/// connects; this one never leaves the PC. `Debug` leaves the cookie out.
/// What a shell's channel asks the server for besides a login shell, from
/// ssh_config: `SetEnv` and `SendEnv`, `RemoteCommand`, `RequestTTY` and
/// `ForwardAgent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOptions {
    /// Variables to set before the shell starts (`env` requests; the
    /// server's `AcceptEnv` decides which it takes).
    pub env: Vec<(String, String)>,
    /// Run this instead of the login shell.
    pub command: Option<String>,
    /// Ask for a terminal.
    pub pty: bool,
    /// Ask for agent forwarding, so the server may open agent channels.
    pub agent: bool,
    /// Start `mosh-server` instead of a shell and carry the terminal over
    /// Mosh (UDP) from then on; `command` is what Mosh runs.
    pub mosh: Option<Box<MoshOptions>>,
}

/// How a Mosh session starts its server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoshOptions {
    /// The server program: `mosh-server`, or a path to it.
    pub server: String,
    /// The UDP port, or range of ports, the server may listen on.
    pub ports: Option<(u16, u16)>,
}

impl Default for ShellOptions {
    /// A login shell on a terminal, nothing else.
    fn default() -> Self {
        Self {
            env: Vec::new(),
            command: None,
            pty: true,
            agent: false,
            mosh: None,
        }
    }
}

/// Which IP versions a connection may use (`AddressFamily`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Family {
    #[default]
    Any,
    V4,
    V6,
}

/// The UI's answer to a [`Message::GssapiStep`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GssReply {
    /// The security context needs another round: send `token` to the
    /// server.
    Continue(Vec<u8>),
    /// The context is established (with mutual authentication): `token`
    /// is the last one for the server, if not empty, and `mic` the MIC
    /// over the step's `mic_data`.
    Complete { token: Vec<u8>, mic: Vec<u8> },
    /// No Kerberos login: no ticket, unknown host, or the user is not
    /// in a domain. The worker goes on with the other methods.
    Failed,
}

/// One SSPI operation a WinRM session's worker asks the UI for. The UI
/// holds the user's credentials and the security contexts (Negotiate:
/// Kerberos or NTLM); the worker only moves opaque tokens and sealed
/// messages between them and the server. `conn` in
/// [`Message::SspiRequest`] says which of the session's HTTP connections
/// (each has a context of its own) it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SspiOp {
    /// Start a new security context for the host's `HTTP` service.
    Start,
    /// Continue with the server's token.
    Step(Vec<u8>),
    /// Encrypt a request body.
    Seal(Vec<u8>),
    /// Decrypt a response body: its signature and encrypted data.
    Unseal { header: Vec<u8>, data: Vec<u8> },
}

/// The UI's answer to a [`Message::SspiRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SspiResult {
    /// Send `token` to the server and pass on its answer.
    Continue(Vec<u8>),
    /// The context is established; `token`, if not empty, is the last one
    /// for the server.
    Done(Vec<u8>),
    /// A sealed request body: its signature and encrypted data.
    Sealed { header: Vec<u8>, data: Vec<u8> },
    /// An unsealed response body.
    Unsealed(Vec<u8>),
    /// No login, or a message that failed its check: the session ends.
    Failed(String),
}

/// How long the worker waits for a server, from ssh_config's
/// `ConnectTimeout`, `ServerAliveInterval` and `ServerAliveCountMax`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Seconds for the TCP connection and the server's first reply.
    pub connect_timeout: u32,
    /// Seconds of silence before a keepalive; 0 sends none.
    pub alive_interval: u32,
    /// Unanswered keepalives before the connection counts as dead.
    pub alive_max: u32,
}

impl Default for Timing {
    /// FeroSSH's defaults: 20 s to connect, a keepalive after 30 s of
    /// silence, dead after 3 unanswered.
    fn default() -> Self {
        Self {
            connect_timeout: 20,
            alive_interval: 30,
            alive_max: 3,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct X11Auth {
    pub screen: u32,
    /// Usually `MIT-MAGIC-COOKIE-1`.
    pub protocol: String,
    pub cookie: Vec<u8>,
}

impl fmt::Debug for X11Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("X11Auth")
            .field("screen", &self.screen)
            .field("protocol", &self.protocol)
            .field("cookie", &format_args!("<{} bytes>", self.cookie.len()))
            .finish()
    }
}

impl Drop for X11Auth {
    fn drop(&mut self) {
        self.cookie.zeroize();
    }
}

/// File attributes, as far as the server sent them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileAttrs {
    pub size: Option<u64>,
    /// Unix mode bits, file type included.
    pub permissions: Option<u32>,
    /// Modification time, seconds since 1970.
    pub mtime: Option<u32>,
}

impl FileAttrs {
    const TYPE_MASK: u32 = 0o170_000;

    fn file_type(&self) -> Option<u32> {
        self.permissions.map(|p| p & Self::TYPE_MASK)
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == Some(0o040_000)
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type() == Some(0o120_000)
    }

    /// A regular file, or a server that did not say.
    pub fn is_file(&self) -> bool {
        matches!(self.file_type(), Some(0o100_000) | None)
    }
}

/// One name in a directory listing. `.` and `..` are left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: Vec<u8>,
    /// Not following symlinks: a link is listed as a link.
    pub attrs: FileAttrs,
}

/// How [`SftpOp::Open`] opens a remote file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    Read,
    /// Write, creating the file if it is missing; `truncate` empties it.
    Write {
        truncate: bool,
    },
}

/// One SFTP operation the UI asks the worker for. Handles are the worker's
/// small numbers, not the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SftpOp {
    /// The absolute, canonical form of `path` (symlinks resolved).
    RealPath {
        path: Vec<u8>,
    },
    /// Attributes of `path`; `follow` resolves a final symlink.
    Stat {
        path: Vec<u8>,
        follow: bool,
    },
    /// The names in directory `path`, in one or more
    /// [`SftpReply::Entries`].
    List {
        path: Vec<u8>,
    },
    Open {
        path: Vec<u8>,
        mode: OpenMode,
    },
    /// Up to `len` bytes at `offset`; [`SftpReply::Eof`] past the end.
    Read {
        handle: u32,
        offset: u64,
        len: u32,
    },
    Write {
        handle: u32,
        offset: u64,
        data: Vec<u8>,
    },
    Close {
        handle: u32,
    },
    Mkdir {
        path: Vec<u8>,
    },
    Remove {
        path: Vec<u8>,
    },
    Rmdir {
        path: Vec<u8>,
    },
    /// Renames `from` to `to`; with `overwrite`, an existing `to` is
    /// replaced.
    Rename {
        from: Vec<u8>,
        to: Vec<u8>,
        overwrite: bool,
    },
    /// The SHA-256 of a remote file, computed on the server by
    /// `xargs -0 sha256sum`, the path on stdin (an exec channel, not SFTP).
    /// Fails where the server has no such commands.
    Checksum {
        path: Vec<u8>,
    },
    /// Sets the permission bits (the low 12 bits of `mode`) of `path`.
    Chmod {
        path: Vec<u8>,
        mode: u32,
    },
}

/// The outcome of one [`SftpOp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SftpReply {
    Done,
    /// The server's status code ([`sftp_code`]) and message.
    Failed {
        code: u32,
        message: String,
    },
    Path(Vec<u8>),
    Attrs(FileAttrs),
    /// Part of a listing; `last` ends it.
    Entries {
        entries: Vec<DirEntry>,
        last: bool,
    },
    Opened {
        handle: u32,
    },
    Data(Vec<u8>),
    Eof,
    /// Lower-case hex.
    Checksum(String),
}

/// Status codes in [`SftpReply::Failed`]: SFTP's own (RFC draft 02), and
/// FeroSSH's above 1000.
pub mod sftp_code {
    pub const EOF: u32 = 1;
    pub const NO_SUCH_FILE: u32 = 2;
    pub const PERMISSION_DENIED: u32 = 3;
    pub const FAILURE: u32 = 4;
    pub const UNSUPPORTED: u32 = 8;
    /// The worker refused the request (unknown handle, too many open).
    pub const REFUSED: u32 = 1001;
    /// The SFTP channel or the connection is gone.
    pub const GONE: u32 = 1002;
}

/// The UI's answer to [`Message::ZmodemStart`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZmodemAnswer {
    /// ZMODEM is turned off: show the output as it came.
    Off,
    /// Stop the server's `rz` or `sz`.
    Decline,
    /// Take the files the server's `sz` sends.
    Receive,
    /// Send these files to the server's `rz`, in this order.
    Send(Vec<ZmodemUpload>),
}

/// A file the UI sends with ZMODEM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZmodemUpload {
    /// The name the server sees, without a folder.
    pub name: Vec<u8>,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub mtime: u64,
}

/// A message on the UI/worker pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// First message from each side; carries the sender's protocol version.
    Hello { version: u8 },
    /// Terminal bytes, in either direction.
    Data { channel: ChannelId, bytes: Vec<u8> },
    /// The UI resized a terminal pane.
    Resize {
        channel: ChannelId,
        cols: u16,
        rows: u16,
    },
    /// Either side closes a channel.
    Close { channel: ChannelId },

    // UI to worker.
    /// Connect to `host`, authenticate as `user` and open a shell on a pty
    /// of `cols` x `rows`. `host_key_algs` are the key types known_hosts
    /// already holds for this host; the worker asks for those first. With
    /// `x11`, the shell asks the server for X11 forwarding.
    ///
    /// With a `tunnel`, the worker does not open a TCP connection: the SSH
    /// connection runs over that stream channel instead, whose other end
    /// the UI connects to a jump host's `direct-tcpip` channel in another
    /// worker, or to a local `ProxyCommand`. A login without `shell` is a
    /// jump host's: the worker logs in, answers [`Message::Connected`] and
    /// keeps the connection for the streams opened on it until
    /// [`Message::Close`].
    Connect {
        channel: ChannelId,
        host: String,
        port: u16,
        user: String,
        cols: u16,
        rows: u16,
        host_key_algs: Vec<String>,
        x11: Option<X11Auth>,
        /// Also offer the legacy algorithm set (`LegacyAlgorithms yes`).
        legacy: bool,
        timing: Timing,
        tunnel: Option<ChannelId>,
        shell: bool,
        options: ShellOptions,
        family: Family,
        /// Try a Kerberos login (`gssapi-with-mic`) first, when the server
        /// offers it (`GSSAPIAuthentication yes`).
        gssapi: bool,
    },
    /// Open another shell on the connection that `connection` (a channel
    /// that made it with [`Message::Connect`]) belongs to. No new login:
    /// the worker answers with [`Message::Connected`], or with
    /// [`Message::Disconnected`] when that connection is gone.
    OpenChannel {
        channel: ChannelId,
        connection: ChannelId,
        cols: u16,
        rows: u16,
        x11: Option<X11Auth>,
        options: ShellOptions,
    },
    /// Open a Telnet session to `host`:`port` on a terminal of `cols` x
    /// `rows`. No login, no encryption: the worker opens the TCP
    /// connection, speaks Telnet option negotiation and answers with
    /// [`Message::Connected`], terminal [`Message::Data`] and finally
    /// [`Message::Disconnected`]; it never asks for keys or passwords.
    TelnetConnect {
        channel: ChannelId,
        host: String,
        port: u16,
        cols: u16,
        rows: u16,
        /// Seconds for the TCP connection.
        connect_timeout: u32,
    },
    /// Open a WinRM (WS-Management) shell on `host`:`port` over HTTP and
    /// run `command` in it (`powershell`, `cmd`). The worker speaks HTTP
    /// and SOAP; every login step and every message's encryption it asks
    /// the UI for with [`Message::SspiRequest`]. It answers with
    /// [`Message::Connected`], the command's output as [`Message::Data`]
    /// (stdout) and [`Message::ExecStderr`], its [`Message::ExitStatus`]
    /// and finally [`Message::Disconnected`]. [`Message::Data`] from the
    /// UI is the command's input; a lone `0x03` byte sends Ctrl+C.
    WinrmConnect {
        channel: ChannelId,
        host: String,
        port: u16,
        /// Seconds for the TCP connection.
        connect_timeout: u32,
        command: String,
    },
    /// The answer to the last [`Message::SspiRequest`] for `conn`.
    SspiReply {
        channel: ChannelId,
        conn: u8,
        result: SspiResult,
    },
    /// The user (or known_hosts) trusts, or rejects, the key in the last
    /// [`Message::HostKey`].
    HostKeyVerdict { channel: ChannelId, accept: bool },
    /// Public keys (SSH wire format) the UI can sign with, in the order to
    /// try them.
    Identities {
        channel: ChannelId,
        keys: Vec<Vec<u8>>,
    },
    /// The signature blob for the last [`Message::SignRequest`]; empty if
    /// the UI refused or the signer failed.
    Signature {
        channel: ChannelId,
        signature: Vec<u8>,
    },
    /// The user's answers to the last [`Message::Prompt`], in order.
    Answers {
        channel: ChannelId,
        answers: Secrets,
    },
    /// The answer to the last [`Message::GssapiStep`].
    GssapiReply { channel: ChannelId, reply: GssReply },

    // Worker to UI.
    /// The server's host key (SSH wire format), for the UI to check against
    /// known_hosts. The worker waits for a [`Message::HostKeyVerdict`].
    HostKey { channel: ChannelId, key: Vec<u8> },
    /// The server proved its identity with a host certificate whose CA
    /// signature, type and algorithms the worker has checked. The UI
    /// decides whether `ca` is trusted for this host (known_hosts
    /// `@cert-authority`), and checks the principals, validity and
    /// revocation; the worker waits for a [`Message::HostKeyVerdict`].
    HostCert {
        channel: ChannelId,
        /// The CA's public key, SSH wire format.
        ca: Vec<u8>,
        /// The certified host key, SSH wire format.
        key: Vec<u8>,
        principals: Vec<String>,
        /// Seconds since the Unix epoch.
        valid_after: u64,
        valid_before: u64,
        key_id: String,
        serial: u64,
    },
    /// The server's pre-login banner.
    Banner { channel: ChannelId, text: String },
    /// The server accepts public keys: which ones does the UI have?
    IdentitiesRequest { channel: ChannelId },
    /// The server accepted `key`; sign `data`, the user-auth request.
    SignRequest {
        channel: ChannelId,
        key: Vec<u8>,
        data: Vec<u8>,
    },
    /// A Kerberos login step (`gssapi-with-mic`, RFC 4462): the server
    /// chose Kerberos 5 (`first`, no `token`) or sent `token`. The UI,
    /// which holds the user's tickets, advances its security context for
    /// the host it connected to and answers with [`Message::GssapiReply`];
    /// once the context is established it signs `mic_data`, the user-auth
    /// fields the server checks.
    GssapiStep {
        channel: ChannelId,
        first: bool,
        token: Vec<u8>,
        mic_data: Vec<u8>,
    },
    /// A WinRM session's SSPI step, for its HTTP connection `conn`; the
    /// worker waits for [`Message::SspiReply`].
    SspiRequest {
        channel: ChannelId,
        conn: u8,
        op: SspiOp,
    },
    /// Ask the user something; the worker waits for [`Message::Answers`].
    Prompt {
        channel: ChannelId,
        kind: PromptKind,
        name: String,
        instructions: String,
        fields: Vec<PromptField>,
    },
    /// Something the user should see, such as a refused key.
    Notice { channel: ChannelId, text: String },
    /// Logged in and the shell is open; the negotiated algorithms.
    Connected {
        channel: ChannelId,
        kex: String,
        cipher: String,
        host_key: String,
    },
    /// The remote command exited.
    ExitStatus { channel: ChannelId, code: u32 },
    /// The connection broke under this channel's open shell (a network
    /// drop, a keepalive timeout), rather than the shell exiting or the
    /// server ending the session: the UI may connect again on its own.
    /// [`Message::Disconnected`] follows.
    Lost { channel: ChannelId },
    /// The connection is gone; no more messages for this channel follow.
    Disconnected { channel: ChannelId, reason: String },

    // SFTP.
    /// UI to worker: open an SFTP channel on the connection `connection`
    /// made. The worker answers with [`Message::SftpReady`], or with
    /// [`Message::Disconnected`] when the connection is gone or the server
    /// has no SFTP.
    SftpOpen {
        channel: ChannelId,
        connection: ChannelId,
    },
    /// UI to worker: run `command` on the connection `connection` made, on
    /// an exec channel without a terminal. [`Message::Data`] from the UI
    /// is the command's standard input and [`Message::Eof`] ends it; the
    /// worker sends its output (standard error too) as
    /// [`Message::Data`], then [`Message::ExitStatus`] and
    /// [`Message::Disconnected`]. [`Message::Close`] closes the channel.
    /// The UI runs only commands of its own (Open virtual desktop's
    /// script, a remote app's agent), never ones the server suggested.
    /// With `split_stderr`, standard error comes apart as
    /// [`Message::ExecStderr`], so the output stays a clean byte stream.
    Exec {
        channel: ChannelId,
        connection: ChannelId,
        command: Vec<u8>,
        split_stderr: bool,
    },
    /// Worker to UI: standard error of an [`Message::Exec`] command run
    /// with `split_stderr`.
    ExecStderr { channel: ChannelId, bytes: Vec<u8> },
    /// Worker to UI: SFTP is up; `home` is the server's starting directory
    /// (usually the user's home).
    SftpReady { channel: ChannelId, home: Vec<u8> },
    /// UI to worker: one operation; `id` is the UI's, for the reply.
    SftpRequest {
        channel: ChannelId,
        id: u32,
        op: SftpOp,
    },
    /// Worker to UI: the outcome of request `id`.
    SftpReply {
        channel: ChannelId,
        id: u32,
        reply: SftpReply,
    },

    // Port forwarding and X11.
    /// UI to worker: a program connected to a local listener. Open a
    /// `direct-tcpip` channel on `connection` to `target`; without a
    /// target the stream speaks SOCKS 4, 4a or 5 first and the worker
    /// opens the channel the client asks for. The worker answers with
    /// [`Message::ForwardOpened`], or with [`Message::Disconnected`].
    /// A stream that `keep`s its connection holds it open after the last
    /// shell on it closed, as a remote desktop's does.
    ForwardOpen {
        channel: ChannelId,
        connection: ChannelId,
        target: Option<(String, u16)>,
        origin_host: String,
        origin_port: u16,
        keep: bool,
    },
    /// Worker to UI: the stream is open to `host`:`port`.
    ForwardOpened {
        channel: ChannelId,
        host: String,
        port: u16,
    },
    /// UI to worker: ask the server to listen on `bind`:`port` and send
    /// connections back. The worker answers with
    /// [`Message::ForwardListening`], or with [`Message::Disconnected`];
    /// [`Message::Close`] stops the listener.
    RemoteForward {
        channel: ChannelId,
        connection: ChannelId,
        bind: String,
        port: u16,
    },
    /// Worker to UI: the server listens on `port` (the one it chose, when
    /// asked for port 0).
    ForwardListening { channel: ChannelId, port: u16 },
    /// Worker to UI: the server opened stream `channel` for remote forward
    /// `listener`, or for X11 when `x11` is set (`listener` is then the
    /// connection). The UI connects it to the forward's target, or closes
    /// it.
    ForwardIncoming {
        channel: ChannelId,
        listener: ChannelId,
        x11: bool,
        origin_host: String,
        origin_port: u16,
    },
    /// Either side: `bytes` more of the stream's data were delivered, so
    /// the other side may send that much more.
    Window { channel: ChannelId, bytes: u32 },
    /// Either side: no more data in this direction on the stream.
    Eof { channel: ChannelId },

    // ZMODEM, inside a shell's channel.
    /// Worker to UI: the server started `rz` (`upload`: it waits for
    /// files) or `sz`. Output stops until the UI's
    /// [`Message::ZmodemAnswer`], and after that until the transfer ends.
    ZmodemStart { channel: ChannelId, upload: bool },
    /// Worker to UI: the server's `sz` offers a file; the UI answers with
    /// [`Message::ZmodemFileAnswer`]. `name` is the server's and may hold
    /// a path.
    ZmodemFile {
        channel: ChannelId,
        name: Vec<u8>,
        size: Option<u64>,
    },
    /// Worker to UI: the next bytes of the file being received.
    ZmodemData { channel: ChannelId, bytes: Vec<u8> },
    /// Worker to UI: up to `len` bytes of upload `file` (an index into the
    /// answer's list) from `offset`, for [`Message::ZmodemChunk`].
    ZmodemRead {
        channel: ChannelId,
        file: u32,
        offset: u64,
        len: u32,
    },
    /// Worker to UI: the current file is done; `skipped` when the server's
    /// `rz` did not want it.
    ZmodemFileDone { channel: ChannelId, skipped: bool },
    /// Worker to UI: the transfer is over and output flows again; `error`
    /// says why it failed, empty when it did not.
    ZmodemEnd { channel: ChannelId, error: String },
    /// UI to worker: what to do about the [`Message::ZmodemStart`].
    ZmodemAnswer {
        channel: ChannelId,
        answer: ZmodemAnswer,
    },
    /// UI to worker: take, or skip, the file in the last
    /// [`Message::ZmodemFile`].
    ZmodemFileAnswer { channel: ChannelId, accept: bool },
    /// UI to worker: the bytes a [`Message::ZmodemRead`] asked for; empty
    /// when the file could not be read.
    ZmodemChunk {
        channel: ChannelId,
        offset: u64,
        bytes: Vec<u8>,
    },
    /// UI to worker: the user stopped the transfer.
    ZmodemCancel { channel: ChannelId },

    // Agent forwarding. The server opens an agent channel only on a
    // connection whose shell asked for it ([`ShellOptions::agent`]); the
    // worker reads the agent protocol and asks the UI, which owns the
    // keys, one request at a time per agent channel. `channel` is the
    // connection's login channel, `agent` the agent channel's id in the
    // worker.
    /// Worker to UI: the server wants the list of keys.
    AgentList {
        channel: ChannelId,
        agent: ChannelId,
    },
    /// Worker to UI: the server wants `data` signed with `key`; `flags`
    /// are the agent protocol's (RSA hash).
    AgentSign {
        channel: ChannelId,
        agent: ChannelId,
        key: Vec<u8>,
        data: Vec<u8>,
        flags: u32,
    },
    /// UI to worker: the keys for an [`Message::AgentList`], with their
    /// comments.
    AgentKeys {
        channel: ChannelId,
        agent: ChannelId,
        keys: Vec<(Vec<u8>, String)>,
    },
    /// UI to worker: the signature for an [`Message::AgentSign`]; empty
    /// when the user refused or signing failed.
    AgentSigned {
        channel: ChannelId,
        agent: ChannelId,
        signature: Vec<u8>,
    },
}

impl Message {
    fn tag(&self) -> u8 {
        match self {
            Message::Hello { .. } => 0,
            Message::Data { .. } => 1,
            Message::Resize { .. } => 2,
            Message::Close { .. } => 3,
            Message::Connect { .. } => 4,
            Message::HostKeyVerdict { .. } => 5,
            Message::Identities { .. } => 6,
            Message::Signature { .. } => 7,
            Message::Answers { .. } => 8,
            Message::HostKey { .. } => 9,
            Message::Banner { .. } => 10,
            Message::IdentitiesRequest { .. } => 11,
            Message::SignRequest { .. } => 12,
            Message::Prompt { .. } => 13,
            Message::Notice { .. } => 14,
            Message::Connected { .. } => 15,
            Message::ExitStatus { .. } => 16,
            Message::Disconnected { .. } => 17,
            Message::OpenChannel { .. } => 18,
            Message::SftpOpen { .. } => 19,
            Message::SftpReady { .. } => 20,
            Message::SftpRequest { .. } => 21,
            Message::SftpReply { .. } => 22,
            Message::ForwardOpen { .. } => 23,
            Message::ForwardOpened { .. } => 24,
            Message::RemoteForward { .. } => 25,
            Message::ForwardListening { .. } => 26,
            Message::ForwardIncoming { .. } => 27,
            Message::Window { .. } => 28,
            Message::Eof { .. } => 29,
            Message::HostCert { .. } => 30,
            Message::TelnetConnect { .. } => 31,
            Message::Lost { .. } => 32,
            Message::Exec { .. } => 33,
            Message::ExecStderr { .. } => 34,
            Message::AgentList { .. } => 35,
            Message::AgentSign { .. } => 36,
            Message::AgentKeys { .. } => 37,
            Message::AgentSigned { .. } => 38,
            Message::GssapiStep { .. } => 50,
            Message::GssapiReply { .. } => 51,
            Message::WinrmConnect { .. } => 60,
            Message::SspiRequest { .. } => 61,
            Message::SspiReply { .. } => 62,
            Message::ZmodemStart { .. } => 40,
            Message::ZmodemFile { .. } => 41,
            Message::ZmodemData { .. } => 42,
            Message::ZmodemRead { .. } => 43,
            Message::ZmodemFileDone { .. } => 44,
            Message::ZmodemEnd { .. } => 45,
            Message::ZmodemAnswer { .. } => 46,
            Message::ZmodemFileAnswer { .. } => 47,
            Message::ZmodemChunk { .. } => 48,
            Message::ZmodemCancel { .. } => 49,
        }
    }

    /// Which side may send the message. Each side drops the pipe when the
    /// other sends one that only it sends: an honest peer never does, so
    /// that is a compromised or confused one.
    pub fn sent_by(&self) -> Side {
        match self {
            Message::Hello { .. }
            | Message::Data { .. }
            | Message::Close { .. }
            | Message::Window { .. }
            | Message::Eof { .. } => Side::Either,
            Message::Resize { .. }
            | Message::Connect { .. }
            | Message::TelnetConnect { .. }
            | Message::WinrmConnect { .. }
            | Message::SspiReply { .. }
            | Message::OpenChannel { .. }
            | Message::HostKeyVerdict { .. }
            | Message::Identities { .. }
            | Message::Signature { .. }
            | Message::Answers { .. }
            | Message::SftpOpen { .. }
            | Message::Exec { .. }
            | Message::SftpRequest { .. }
            | Message::ForwardOpen { .. }
            | Message::RemoteForward { .. }
            | Message::ZmodemAnswer { .. }
            | Message::ZmodemFileAnswer { .. }
            | Message::ZmodemChunk { .. }
            | Message::ZmodemCancel { .. }
            | Message::AgentKeys { .. }
            | Message::AgentSigned { .. }
            | Message::GssapiReply { .. } => Side::Ui,
            Message::HostKey { .. }
            | Message::HostCert { .. }
            | Message::Banner { .. }
            | Message::IdentitiesRequest { .. }
            | Message::SignRequest { .. }
            | Message::Prompt { .. }
            | Message::Notice { .. }
            | Message::Connected { .. }
            | Message::ExitStatus { .. }
            | Message::ExecStderr { .. }
            | Message::Lost { .. }
            | Message::Disconnected { .. }
            | Message::SftpReady { .. }
            | Message::SftpReply { .. }
            | Message::ForwardOpened { .. }
            | Message::ForwardListening { .. }
            | Message::ForwardIncoming { .. }
            | Message::ZmodemStart { .. }
            | Message::ZmodemFile { .. }
            | Message::ZmodemData { .. }
            | Message::ZmodemRead { .. }
            | Message::ZmodemFileDone { .. }
            | Message::ZmodemEnd { .. }
            | Message::AgentList { .. }
            | Message::AgentSign { .. }
            | Message::GssapiStep { .. }
            | Message::SspiRequest { .. } => Side::Worker,
        }
    }

    /// The channel the message belongs to; `None` for [`Message::Hello`].
    pub fn channel(&self) -> Option<ChannelId> {
        Some(match self {
            Message::Hello { .. } => return None,
            Message::Data { channel, .. }
            | Message::Resize { channel, .. }
            | Message::Close { channel }
            | Message::Connect { channel, .. }
            | Message::TelnetConnect { channel, .. }
            | Message::WinrmConnect { channel, .. }
            | Message::SspiRequest { channel, .. }
            | Message::SspiReply { channel, .. }
            | Message::HostKeyVerdict { channel, .. }
            | Message::Identities { channel, .. }
            | Message::Signature { channel, .. }
            | Message::Answers { channel, .. }
            | Message::HostKey { channel, .. }
            | Message::HostCert { channel, .. }
            | Message::Banner { channel, .. }
            | Message::IdentitiesRequest { channel }
            | Message::SignRequest { channel, .. }
            | Message::Prompt { channel, .. }
            | Message::Notice { channel, .. }
            | Message::Connected { channel, .. }
            | Message::ExitStatus { channel, .. }
            | Message::Lost { channel }
            | Message::Disconnected { channel, .. }
            | Message::OpenChannel { channel, .. }
            | Message::SftpOpen { channel, .. }
            | Message::Exec { channel, .. }
            | Message::ExecStderr { channel, .. }
            | Message::SftpReady { channel, .. }
            | Message::SftpRequest { channel, .. }
            | Message::SftpReply { channel, .. }
            | Message::ForwardOpen { channel, .. }
            | Message::ForwardOpened { channel, .. }
            | Message::RemoteForward { channel, .. }
            | Message::ForwardListening { channel, .. }
            | Message::ForwardIncoming { channel, .. }
            | Message::Window { channel, .. }
            | Message::Eof { channel }
            | Message::ZmodemStart { channel, .. }
            | Message::ZmodemFile { channel, .. }
            | Message::ZmodemData { channel, .. }
            | Message::ZmodemRead { channel, .. }
            | Message::ZmodemFileDone { channel, .. }
            | Message::ZmodemEnd { channel, .. }
            | Message::ZmodemAnswer { channel, .. }
            | Message::ZmodemFileAnswer { channel, .. }
            | Message::ZmodemChunk { channel, .. }
            | Message::ZmodemCancel { channel }
            | Message::AgentList { channel, .. }
            | Message::AgentSign { channel, .. }
            | Message::AgentKeys { channel, .. }
            | Message::AgentSigned { channel, .. }
            | Message::GssapiStep { channel, .. }
            | Message::GssapiReply { channel, .. } => *channel,
        })
    }
}

/// Who sends a [`Message`]: see [`Message::sent_by`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ui,
    Worker,
    Either,
}

/// Why a frame was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The body length exceeds [`MAX_BODY_LEN`].
    TooLarge(usize),
    /// The frame's version is not [`PROTOCOL_VERSION`].
    Version(u8),
    /// The message tag is unknown.
    Tag(u8),
    /// The body ended before all fields were read.
    Truncated,
    /// The body has bytes left after the last field.
    Trailing(usize),
    /// A field holds a value outside its range (a boolean that is not 0
    /// or 1, a string that is not UTF-8, an unknown prompt kind).
    Invalid,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLarge(n) => write!(f, "frame body of {n} bytes exceeds {MAX_BODY_LEN}"),
            Error::Version(v) => write!(f, "unsupported protocol version {v}"),
            Error::Tag(t) => write!(f, "unknown message tag {t}"),
            Error::Truncated => f.write_str("frame body truncated"),
            Error::Trailing(n) => write!(f, "{n} trailing bytes after message"),
            Error::Invalid => f.write_str("field value out of range"),
        }
    }
}

impl std::error::Error for Error {}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    fn opt<T: Copy>(&mut self, v: Option<T>, mut item: impl FnMut(&mut Self, T)) {
        self.bool(v.is_some());
        if let Some(v) = v {
            item(self, v);
        }
    }

    fn options(&mut self, o: &ShellOptions) {
        self.list(&o.env, |w, (k, v)| {
            w.str(k);
            w.str(v);
        });
        self.bool(o.command.is_some());
        if let Some(c) = &o.command {
            self.str(c);
        }
        self.bool(o.pty);
        self.bool(o.agent);
        self.bool(o.mosh.is_some());
        if let Some(m) = &o.mosh {
            self.str(&m.server);
            self.opt(m.ports, |w, (low, high)| {
                w.u16(low);
                w.u16(high);
            });
        }
    }

    fn x11(&mut self, x: &Option<X11Auth>) {
        self.bool(x.is_some());
        if let Some(x) = x {
            self.u32(x.screen);
            self.str(&x.protocol);
            self.bytes(&x.cookie);
        }
    }

    fn attrs(&mut self, a: &FileAttrs) {
        self.opt(a.size, Self::u64);
        self.opt(a.permissions, Self::u32);
        self.opt(a.mtime, Self::u32);
    }

    fn op(&mut self, op: &SftpOp) {
        match op {
            SftpOp::RealPath { path } => {
                self.u8(0);
                self.bytes(path);
            }
            SftpOp::Stat { path, follow } => {
                self.u8(1);
                self.bytes(path);
                self.bool(*follow);
            }
            SftpOp::List { path } => {
                self.u8(2);
                self.bytes(path);
            }
            SftpOp::Open { path, mode } => {
                self.u8(3);
                self.bytes(path);
                match mode {
                    OpenMode::Read => self.u8(0),
                    OpenMode::Write { truncate: false } => self.u8(1),
                    OpenMode::Write { truncate: true } => self.u8(2),
                }
            }
            SftpOp::Read {
                handle,
                offset,
                len,
            } => {
                self.u8(4);
                self.u32(*handle);
                self.u64(*offset);
                self.u32(*len);
            }
            SftpOp::Write {
                handle,
                offset,
                data,
            } => {
                self.u8(5);
                self.u32(*handle);
                self.u64(*offset);
                self.bytes(data);
            }
            SftpOp::Close { handle } => {
                self.u8(6);
                self.u32(*handle);
            }
            SftpOp::Mkdir { path } => {
                self.u8(7);
                self.bytes(path);
            }
            SftpOp::Remove { path } => {
                self.u8(8);
                self.bytes(path);
            }
            SftpOp::Rmdir { path } => {
                self.u8(9);
                self.bytes(path);
            }
            SftpOp::Rename {
                from,
                to,
                overwrite,
            } => {
                self.u8(10);
                self.bytes(from);
                self.bytes(to);
                self.bool(*overwrite);
            }
            SftpOp::Checksum { path } => {
                self.u8(11);
                self.bytes(path);
            }
            SftpOp::Chmod { path, mode } => {
                self.u8(12);
                self.bytes(path);
                self.u32(*mode);
            }
        }
    }

    fn reply(&mut self, reply: &SftpReply) {
        match reply {
            SftpReply::Done => self.u8(0),
            SftpReply::Failed { code, message } => {
                self.u8(1);
                self.u32(*code);
                self.str(message);
            }
            SftpReply::Path(path) => {
                self.u8(2);
                self.bytes(path);
            }
            SftpReply::Attrs(a) => {
                self.u8(3);
                self.attrs(a);
            }
            SftpReply::Entries { entries, last } => {
                self.u8(4);
                self.list(entries, |w, e| {
                    w.bytes(&e.name);
                    w.attrs(&e.attrs);
                });
                self.bool(*last);
            }
            SftpReply::Opened { handle } => {
                self.u8(5);
                self.u32(*handle);
            }
            SftpReply::Data(data) => {
                self.u8(6);
                self.bytes(data);
            }
            SftpReply::Eof => self.u8(7),
            SftpReply::Checksum(hex) => {
                self.u8(8);
                self.str(hex);
            }
        }
    }

    /// A length-prefixed byte string. Lengths above `u32::MAX` cannot fit
    /// in a frame anyway; `encode` rejects the frame by its size.
    fn bytes(&mut self, v: &[u8]) {
        self.u32(u32::try_from(v.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(v);
    }

    fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }

    fn list<T>(&mut self, items: &[T], mut item: impl FnMut(&mut Self, &T)) {
        self.u32(u32::try_from(items.len()).unwrap_or(u32::MAX));
        for i in items {
            item(self, i);
        }
    }
}

/// Appends `msg` as one frame to `out`.
pub fn encode(msg: &Message, out: &mut Vec<u8>) -> Result<(), Error> {
    let mut w = Writer(vec![PROTOCOL_VERSION, msg.tag()]);
    match msg {
        Message::Hello { version } => w.u8(*version),
        Message::Data { channel, bytes } => {
            w.u32(*channel);
            w.0.extend_from_slice(bytes);
        }
        Message::Resize {
            channel,
            cols,
            rows,
        } => {
            w.u32(*channel);
            w.u16(*cols);
            w.u16(*rows);
        }
        Message::Close { channel }
        | Message::IdentitiesRequest { channel }
        | Message::Eof { channel }
        | Message::Lost { channel }
        | Message::ZmodemCancel { channel } => w.u32(*channel),
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
            options,
            family,
            gssapi,
        } => {
            w.u32(*channel);
            w.str(host);
            w.u16(*port);
            w.str(user);
            w.u16(*cols);
            w.u16(*rows);
            w.list(host_key_algs, |w, s| w.str(s));
            w.x11(x11);
            w.bool(*legacy);
            w.u32(timing.connect_timeout);
            w.u32(timing.alive_interval);
            w.u32(timing.alive_max);
            w.opt(*tunnel, Writer::u32);
            w.bool(*shell);
            w.options(options);
            w.u8(match family {
                Family::Any => 0,
                Family::V4 => 4,
                Family::V6 => 6,
            });
            w.bool(*gssapi);
        }
        Message::TelnetConnect {
            channel,
            host,
            port,
            cols,
            rows,
            connect_timeout,
        } => {
            w.u32(*channel);
            w.str(host);
            w.u16(*port);
            w.u16(*cols);
            w.u16(*rows);
            w.u32(*connect_timeout);
        }
        Message::WinrmConnect {
            channel,
            host,
            port,
            connect_timeout,
            command,
        } => {
            w.u32(*channel);
            w.str(host);
            w.u16(*port);
            w.u32(*connect_timeout);
            w.str(command);
        }
        Message::SspiRequest { channel, conn, op } => {
            w.u32(*channel);
            w.u8(*conn);
            match op {
                SspiOp::Start => w.u8(0),
                SspiOp::Step(token) => {
                    w.u8(1);
                    w.bytes(token);
                }
                SspiOp::Seal(data) => {
                    w.u8(2);
                    w.bytes(data);
                }
                SspiOp::Unseal { header, data } => {
                    w.u8(3);
                    w.bytes(header);
                    w.bytes(data);
                }
            }
        }
        Message::SspiReply {
            channel,
            conn,
            result,
        } => {
            w.u32(*channel);
            w.u8(*conn);
            match result {
                SspiResult::Continue(token) => {
                    w.u8(0);
                    w.bytes(token);
                }
                SspiResult::Done(token) => {
                    w.u8(1);
                    w.bytes(token);
                }
                SspiResult::Sealed { header, data } => {
                    w.u8(2);
                    w.bytes(header);
                    w.bytes(data);
                }
                SspiResult::Unsealed(data) => {
                    w.u8(3);
                    w.bytes(data);
                }
                SspiResult::Failed(why) => {
                    w.u8(4);
                    w.str(why);
                }
            }
        }
        Message::HostKeyVerdict { channel, accept } => {
            w.u32(*channel);
            w.bool(*accept);
        }
        Message::Identities { channel, keys } => {
            w.u32(*channel);
            w.list(keys, |w, k| w.bytes(k));
        }
        Message::Signature { channel, signature } => {
            w.u32(*channel);
            w.bytes(signature);
        }
        Message::Answers { channel, answers } => {
            w.u32(*channel);
            w.list(&answers.0, |w, s| w.str(s));
        }
        Message::HostKey { channel, key } => {
            w.u32(*channel);
            w.bytes(key);
        }
        Message::HostCert {
            channel,
            ca,
            key,
            principals,
            valid_after,
            valid_before,
            key_id,
            serial,
        } => {
            w.u32(*channel);
            w.bytes(ca);
            w.bytes(key);
            w.list(principals, |w, s| w.str(s));
            w.u64(*valid_after);
            w.u64(*valid_before);
            w.str(key_id);
            w.u64(*serial);
        }
        Message::Banner { channel, text }
        | Message::Notice { channel, text }
        | Message::Disconnected {
            channel,
            reason: text,
        } => {
            w.u32(*channel);
            w.str(text);
        }
        Message::SignRequest { channel, key, data } => {
            w.u32(*channel);
            w.bytes(key);
            w.bytes(data);
        }
        Message::Prompt {
            channel,
            kind,
            name,
            instructions,
            fields,
        } => {
            w.u32(*channel);
            w.u8(match kind {
                PromptKind::Password => 0,
                PromptKind::KeyboardInteractive => 1,
            });
            w.str(name);
            w.str(instructions);
            w.list(fields, |w, f| {
                w.str(&f.text);
                w.bool(f.echo);
            });
        }
        Message::Connected {
            channel,
            kex,
            cipher,
            host_key,
        } => {
            w.u32(*channel);
            w.str(kex);
            w.str(cipher);
            w.str(host_key);
        }
        Message::ExitStatus { channel, code } => {
            w.u32(*channel);
            w.u32(*code);
        }
        Message::OpenChannel {
            channel,
            connection,
            cols,
            rows,
            x11,
            options,
        } => {
            w.u32(*channel);
            w.u32(*connection);
            w.u16(*cols);
            w.u16(*rows);
            w.x11(x11);
            w.options(options);
        }
        Message::AgentList { channel, agent } => {
            w.u32(*channel);
            w.u32(*agent);
        }
        Message::AgentSign {
            channel,
            agent,
            key,
            data,
            flags,
        } => {
            w.u32(*channel);
            w.u32(*agent);
            w.bytes(key);
            w.bytes(data);
            w.u32(*flags);
        }
        Message::AgentKeys {
            channel,
            agent,
            keys,
        } => {
            w.u32(*channel);
            w.u32(*agent);
            w.list(keys, |w, (key, comment)| {
                w.bytes(key);
                w.str(comment);
            });
        }
        Message::AgentSigned {
            channel,
            agent,
            signature,
        } => {
            w.u32(*channel);
            w.u32(*agent);
            w.bytes(signature);
        }
        Message::GssapiStep {
            channel,
            first,
            token,
            mic_data,
        } => {
            w.u32(*channel);
            w.bool(*first);
            w.bytes(token);
            w.bytes(mic_data);
        }
        Message::GssapiReply { channel, reply } => {
            w.u32(*channel);
            match reply {
                GssReply::Continue(token) => {
                    w.u8(0);
                    w.bytes(token);
                }
                GssReply::Complete { token, mic } => {
                    w.u8(1);
                    w.bytes(token);
                    w.bytes(mic);
                }
                GssReply::Failed => w.u8(2),
            }
        }
        Message::SftpOpen {
            channel,
            connection,
        } => {
            w.u32(*channel);
            w.u32(*connection);
        }
        Message::Exec {
            channel,
            connection,
            command,
            split_stderr,
        } => {
            w.u32(*channel);
            w.u32(*connection);
            w.bytes(command);
            w.bool(*split_stderr);
        }
        Message::ExecStderr { channel, bytes } => {
            w.u32(*channel);
            w.bytes(bytes);
        }
        Message::SftpReady { channel, home } => {
            w.u32(*channel);
            w.bytes(home);
        }
        Message::SftpRequest { channel, id, op } => {
            w.u32(*channel);
            w.u32(*id);
            w.op(op);
        }
        Message::SftpReply { channel, id, reply } => {
            w.u32(*channel);
            w.u32(*id);
            w.reply(reply);
        }
        Message::ForwardOpen {
            channel,
            connection,
            target,
            origin_host,
            origin_port,
            keep,
        } => {
            w.u32(*channel);
            w.u32(*connection);
            w.bool(target.is_some());
            if let Some((host, port)) = target {
                w.str(host);
                w.u16(*port);
            }
            w.str(origin_host);
            w.u16(*origin_port);
            w.bool(*keep);
        }
        Message::ForwardOpened {
            channel,
            host,
            port,
        } => {
            w.u32(*channel);
            w.str(host);
            w.u16(*port);
        }
        Message::RemoteForward {
            channel,
            connection,
            bind,
            port,
        } => {
            w.u32(*channel);
            w.u32(*connection);
            w.str(bind);
            w.u16(*port);
        }
        Message::ForwardListening { channel, port } => {
            w.u32(*channel);
            w.u16(*port);
        }
        Message::ForwardIncoming {
            channel,
            listener,
            x11,
            origin_host,
            origin_port,
        } => {
            w.u32(*channel);
            w.u32(*listener);
            w.bool(*x11);
            w.str(origin_host);
            w.u16(*origin_port);
        }
        Message::Window { channel, bytes } => {
            w.u32(*channel);
            w.u32(*bytes);
        }
        Message::ZmodemStart {
            channel,
            upload: flag,
        }
        | Message::ZmodemFileDone {
            channel,
            skipped: flag,
        }
        | Message::ZmodemFileAnswer {
            channel,
            accept: flag,
        } => {
            w.u32(*channel);
            w.bool(*flag);
        }
        Message::ZmodemFile {
            channel,
            name,
            size,
        } => {
            w.u32(*channel);
            w.bytes(name);
            w.opt(*size, Writer::u64);
        }
        Message::ZmodemData { channel, bytes } => {
            w.u32(*channel);
            w.bytes(bytes);
        }
        Message::ZmodemRead {
            channel,
            file,
            offset,
            len,
        } => {
            w.u32(*channel);
            w.u32(*file);
            w.u64(*offset);
            w.u32(*len);
        }
        Message::ZmodemEnd { channel, error } => {
            w.u32(*channel);
            w.str(error);
        }
        Message::ZmodemAnswer { channel, answer } => {
            w.u32(*channel);
            match answer {
                ZmodemAnswer::Off => w.u8(0),
                ZmodemAnswer::Decline => w.u8(1),
                ZmodemAnswer::Receive => w.u8(2),
                ZmodemAnswer::Send(files) => {
                    w.u8(3);
                    w.list(files, |w, f| {
                        w.bytes(&f.name);
                        w.u64(f.size);
                        w.u64(f.mtime);
                    });
                }
            }
        }
        Message::ZmodemChunk {
            channel,
            offset,
            bytes,
        } => {
            w.u32(*channel);
            w.u64(*offset);
            w.bytes(bytes);
        }
    }
    let mut body = w.0;
    let result = (|| {
        if body.len() > MAX_BODY_LEN {
            return Err(Error::TooLarge(body.len()));
        }
        let len = u32::try_from(body.len()).map_err(|_| Error::TooLarge(body.len()))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&body);
        Ok(())
    })();
    // Answers carry passwords; the scratch copy goes too.
    body.zeroize();
    result
}

/// Reassembles frames from a byte stream that may split or join them.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds bytes read from the pipe.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete message, `Ok(None)` if more bytes are
    /// needed, or an error. After an error the stream is out of sync and the
    /// connection must be dropped.
    pub fn next_message(&mut self) -> Result<Option<Message>, Error> {
        let Some((prefix, rest)) = self.buf.split_first_chunk::<LEN_PREFIX>() else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(*prefix) as usize;
        if len > MAX_BODY_LEN {
            return Err(Error::TooLarge(len));
        }
        let Some(body) = rest.get(..len) else {
            return Ok(None);
        };
        let msg = decode_body(body)?;
        // The consumed frame may hold a password; wipe it before dropping.
        self.buf
            .get_mut(..LEN_PREFIX + len)
            .into_iter()
            .for_each(Zeroize::zeroize);
        self.buf.drain(..LEN_PREFIX + len);
        Ok(Some(msg))
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.buf.zeroize();
    }
}

fn decode_body(body: &[u8]) -> Result<Message, Error> {
    let mut r = Reader(body);
    let version = r.u8()?;
    if version != PROTOCOL_VERSION {
        return Err(Error::Version(version));
    }
    let msg = match r.u8()? {
        0 => Message::Hello { version: r.u8()? },
        1 => Message::Data {
            channel: r.u32()?,
            bytes: r.rest().to_vec(),
        },
        2 => Message::Resize {
            channel: r.u32()?,
            cols: r.u16()?,
            rows: r.u16()?,
        },
        3 => Message::Close { channel: r.u32()? },
        4 => Message::Connect {
            channel: r.u32()?,
            host: r.string()?,
            port: r.u16()?,
            user: r.string()?,
            cols: r.u16()?,
            rows: r.u16()?,
            host_key_algs: r.list(Reader::string)?,
            x11: r.x11()?,
            legacy: r.bool()?,
            timing: Timing {
                connect_timeout: r.u32()?,
                alive_interval: r.u32()?,
                alive_max: r.u32()?,
            },
            tunnel: r.opt(Reader::u32)?,
            shell: r.bool()?,
            options: r.options()?,
            family: match r.u8()? {
                0 => Family::Any,
                4 => Family::V4,
                6 => Family::V6,
                _ => return Err(Error::Invalid),
            },
            gssapi: r.bool()?,
        },
        5 => Message::HostKeyVerdict {
            channel: r.u32()?,
            accept: r.bool()?,
        },
        6 => Message::Identities {
            channel: r.u32()?,
            keys: r.list(|r| r.bytes().map(<[u8]>::to_vec))?,
        },
        7 => Message::Signature {
            channel: r.u32()?,
            signature: r.bytes()?.to_vec(),
        },
        8 => Message::Answers {
            channel: r.u32()?,
            answers: Secrets(r.list(Reader::string)?),
        },
        9 => Message::HostKey {
            channel: r.u32()?,
            key: r.bytes()?.to_vec(),
        },
        10 => Message::Banner {
            channel: r.u32()?,
            text: r.string()?,
        },
        11 => Message::IdentitiesRequest { channel: r.u32()? },
        12 => Message::SignRequest {
            channel: r.u32()?,
            key: r.bytes()?.to_vec(),
            data: r.bytes()?.to_vec(),
        },
        13 => Message::Prompt {
            channel: r.u32()?,
            kind: match r.u8()? {
                0 => PromptKind::Password,
                1 => PromptKind::KeyboardInteractive,
                _ => return Err(Error::Invalid),
            },
            name: r.string()?,
            instructions: r.string()?,
            fields: r.list(|r| {
                Ok(PromptField {
                    text: r.string()?,
                    echo: r.bool()?,
                })
            })?,
        },
        14 => Message::Notice {
            channel: r.u32()?,
            text: r.string()?,
        },
        15 => Message::Connected {
            channel: r.u32()?,
            kex: r.string()?,
            cipher: r.string()?,
            host_key: r.string()?,
        },
        16 => Message::ExitStatus {
            channel: r.u32()?,
            code: r.u32()?,
        },
        17 => Message::Disconnected {
            channel: r.u32()?,
            reason: r.string()?,
        },
        18 => Message::OpenChannel {
            channel: r.u32()?,
            connection: r.u32()?,
            cols: r.u16()?,
            rows: r.u16()?,
            x11: r.x11()?,
            options: r.options()?,
        },
        35 => Message::AgentList {
            channel: r.u32()?,
            agent: r.u32()?,
        },
        36 => Message::AgentSign {
            channel: r.u32()?,
            agent: r.u32()?,
            key: r.path()?,
            data: r.path()?,
            flags: r.u32()?,
        },
        37 => Message::AgentKeys {
            channel: r.u32()?,
            agent: r.u32()?,
            keys: r.list(|r| Ok((r.path()?, r.string()?)))?,
        },
        38 => Message::AgentSigned {
            channel: r.u32()?,
            agent: r.u32()?,
            signature: r.path()?,
        },
        50 => Message::GssapiStep {
            channel: r.u32()?,
            first: r.bool()?,
            token: r.bytes()?.to_vec(),
            mic_data: r.bytes()?.to_vec(),
        },
        51 => Message::GssapiReply {
            channel: r.u32()?,
            reply: match r.u8()? {
                0 => GssReply::Continue(r.bytes()?.to_vec()),
                1 => GssReply::Complete {
                    token: r.bytes()?.to_vec(),
                    mic: r.bytes()?.to_vec(),
                },
                2 => GssReply::Failed,
                _ => return Err(Error::Invalid),
            },
        },
        19 => Message::SftpOpen {
            channel: r.u32()?,
            connection: r.u32()?,
        },
        33 => Message::Exec {
            channel: r.u32()?,
            connection: r.u32()?,
            command: r.bytes()?.to_vec(),
            split_stderr: r.bool()?,
        },
        34 => Message::ExecStderr {
            channel: r.u32()?,
            bytes: r.bytes()?.to_vec(),
        },
        20 => Message::SftpReady {
            channel: r.u32()?,
            home: r.bytes()?.to_vec(),
        },
        21 => Message::SftpRequest {
            channel: r.u32()?,
            id: r.u32()?,
            op: r.op()?,
        },
        22 => Message::SftpReply {
            channel: r.u32()?,
            id: r.u32()?,
            reply: r.reply()?,
        },
        23 => Message::ForwardOpen {
            channel: r.u32()?,
            connection: r.u32()?,
            target: r.opt(|r| Ok((r.string()?, r.u16()?)))?,
            origin_host: r.string()?,
            origin_port: r.u16()?,
            keep: r.bool()?,
        },
        24 => Message::ForwardOpened {
            channel: r.u32()?,
            host: r.string()?,
            port: r.u16()?,
        },
        25 => Message::RemoteForward {
            channel: r.u32()?,
            connection: r.u32()?,
            bind: r.string()?,
            port: r.u16()?,
        },
        26 => Message::ForwardListening {
            channel: r.u32()?,
            port: r.u16()?,
        },
        27 => Message::ForwardIncoming {
            channel: r.u32()?,
            listener: r.u32()?,
            x11: r.bool()?,
            origin_host: r.string()?,
            origin_port: r.u16()?,
        },
        28 => Message::Window {
            channel: r.u32()?,
            bytes: r.u32()?,
        },
        29 => Message::Eof { channel: r.u32()? },
        30 => Message::HostCert {
            channel: r.u32()?,
            ca: r.bytes()?.to_vec(),
            key: r.bytes()?.to_vec(),
            principals: r.list(Reader::string)?,
            valid_after: r.u64()?,
            valid_before: r.u64()?,
            key_id: r.string()?,
            serial: r.u64()?,
        },
        31 => Message::TelnetConnect {
            channel: r.u32()?,
            host: r.string()?,
            port: r.u16()?,
            cols: r.u16()?,
            rows: r.u16()?,
            connect_timeout: r.u32()?,
        },
        32 => Message::Lost { channel: r.u32()? },
        60 => Message::WinrmConnect {
            channel: r.u32()?,
            host: r.string()?,
            port: r.u16()?,
            connect_timeout: r.u32()?,
            command: r.string()?,
        },
        61 => Message::SspiRequest {
            channel: r.u32()?,
            conn: r.u8()?,
            op: match r.u8()? {
                0 => SspiOp::Start,
                1 => SspiOp::Step(r.bytes()?.to_vec()),
                2 => SspiOp::Seal(r.bytes()?.to_vec()),
                3 => SspiOp::Unseal {
                    header: r.bytes()?.to_vec(),
                    data: r.bytes()?.to_vec(),
                },
                _ => return Err(Error::Invalid),
            },
        },
        62 => Message::SspiReply {
            channel: r.u32()?,
            conn: r.u8()?,
            result: match r.u8()? {
                0 => SspiResult::Continue(r.bytes()?.to_vec()),
                1 => SspiResult::Done(r.bytes()?.to_vec()),
                2 => SspiResult::Sealed {
                    header: r.bytes()?.to_vec(),
                    data: r.bytes()?.to_vec(),
                },
                3 => SspiResult::Unsealed(r.bytes()?.to_vec()),
                4 => SspiResult::Failed(r.string()?),
                _ => return Err(Error::Invalid),
            },
        },
        40 => Message::ZmodemStart {
            channel: r.u32()?,
            upload: r.bool()?,
        },
        41 => Message::ZmodemFile {
            channel: r.u32()?,
            name: r.bytes()?.to_vec(),
            size: r.opt(Reader::u64)?,
        },
        42 => Message::ZmodemData {
            channel: r.u32()?,
            bytes: r.bytes()?.to_vec(),
        },
        43 => Message::ZmodemRead {
            channel: r.u32()?,
            file: r.u32()?,
            offset: r.u64()?,
            len: r.u32()?,
        },
        44 => Message::ZmodemFileDone {
            channel: r.u32()?,
            skipped: r.bool()?,
        },
        45 => Message::ZmodemEnd {
            channel: r.u32()?,
            error: r.string()?,
        },
        46 => Message::ZmodemAnswer {
            channel: r.u32()?,
            answer: match r.u8()? {
                0 => ZmodemAnswer::Off,
                1 => ZmodemAnswer::Decline,
                2 => ZmodemAnswer::Receive,
                3 => ZmodemAnswer::Send(r.list(|r| {
                    Ok(ZmodemUpload {
                        name: r.bytes()?.to_vec(),
                        size: r.u64()?,
                        mtime: r.u64()?,
                    })
                })?),
                _ => return Err(Error::Invalid),
            },
        },
        47 => Message::ZmodemFileAnswer {
            channel: r.u32()?,
            accept: r.bool()?,
        },
        48 => Message::ZmodemChunk {
            channel: r.u32()?,
            offset: r.u64()?,
            bytes: r.bytes()?.to_vec(),
        },
        49 => Message::ZmodemCancel { channel: r.u32()? },
        tag => return Err(Error::Tag(tag)),
    };
    match r.0.len() {
        0 => Ok(msg),
        n => Err(Error::Trailing(n)),
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, tail) = self.0.split_first_chunk::<N>().ok_or(Error::Truncated)?;
        self.0 = tail;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        self.take::<1>().map(|[b]| b)
    }

    fn u16(&mut self) -> Result<u16, Error> {
        self.take().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        self.take().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, Error> {
        self.take().map(u64::from_le_bytes)
    }

    fn bool(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Invalid),
        }
    }

    fn opt<T>(
        &mut self,
        item: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<Option<T>, Error> {
        if self.bool()? {
            item(self).map(Some)
        } else {
            Ok(None)
        }
    }

    fn options(&mut self) -> Result<ShellOptions, Error> {
        Ok(ShellOptions {
            env: self.list(|r| Ok((r.string()?, r.string()?)))?,
            command: self.opt(Reader::string)?,
            pty: self.bool()?,
            agent: self.bool()?,
            mosh: self.opt(|r| {
                Ok(Box::new(MoshOptions {
                    server: r.string()?,
                    ports: r.opt(|r| Ok((r.u16()?, r.u16()?)))?,
                }))
            })?,
        })
    }

    fn x11(&mut self) -> Result<Option<X11Auth>, Error> {
        self.opt(|r| {
            Ok(X11Auth {
                screen: r.u32()?,
                protocol: r.string()?,
                cookie: r.path()?,
            })
        })
    }

    fn path(&mut self) -> Result<Vec<u8>, Error> {
        self.bytes().map(<[u8]>::to_vec)
    }

    fn attrs(&mut self) -> Result<FileAttrs, Error> {
        Ok(FileAttrs {
            size: self.opt(Self::u64)?,
            permissions: self.opt(Self::u32)?,
            mtime: self.opt(Self::u32)?,
        })
    }

    fn op(&mut self) -> Result<SftpOp, Error> {
        Ok(match self.u8()? {
            0 => SftpOp::RealPath { path: self.path()? },
            1 => SftpOp::Stat {
                path: self.path()?,
                follow: self.bool()?,
            },
            2 => SftpOp::List { path: self.path()? },
            3 => SftpOp::Open {
                path: self.path()?,
                mode: match self.u8()? {
                    0 => OpenMode::Read,
                    1 => OpenMode::Write { truncate: false },
                    2 => OpenMode::Write { truncate: true },
                    _ => return Err(Error::Invalid),
                },
            },
            4 => SftpOp::Read {
                handle: self.u32()?,
                offset: self.u64()?,
                len: self.u32()?,
            },
            5 => SftpOp::Write {
                handle: self.u32()?,
                offset: self.u64()?,
                data: self.path()?,
            },
            6 => SftpOp::Close {
                handle: self.u32()?,
            },
            7 => SftpOp::Mkdir { path: self.path()? },
            8 => SftpOp::Remove { path: self.path()? },
            9 => SftpOp::Rmdir { path: self.path()? },
            10 => SftpOp::Rename {
                from: self.path()?,
                to: self.path()?,
                overwrite: self.bool()?,
            },
            11 => SftpOp::Checksum { path: self.path()? },
            12 => SftpOp::Chmod {
                path: self.path()?,
                mode: self.u32()?,
            },
            _ => return Err(Error::Invalid),
        })
    }

    fn reply(&mut self) -> Result<SftpReply, Error> {
        Ok(match self.u8()? {
            0 => SftpReply::Done,
            1 => SftpReply::Failed {
                code: self.u32()?,
                message: self.string()?,
            },
            2 => SftpReply::Path(self.path()?),
            3 => SftpReply::Attrs(self.attrs()?),
            4 => SftpReply::Entries {
                entries: self.list(|r| {
                    Ok(DirEntry {
                        name: r.path()?,
                        attrs: r.attrs()?,
                    })
                })?,
                last: self.bool()?,
            },
            5 => SftpReply::Opened {
                handle: self.u32()?,
            },
            6 => SftpReply::Data(self.path()?),
            7 => SftpReply::Eof,
            8 => SftpReply::Checksum(self.string()?),
            _ => return Err(Error::Invalid),
        })
    }

    fn bytes(&mut self) -> Result<&'a [u8], Error> {
        let len = self.u32()? as usize;
        if len > self.0.len() {
            return Err(Error::Truncated);
        }
        let (head, tail) = self.0.split_at(len);
        self.0 = tail;
        Ok(head)
    }

    fn string(&mut self) -> Result<String, Error> {
        let bytes = self.bytes()?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| Error::Invalid)
    }

    /// A counted list. The count is not trusted for allocation: every item
    /// takes at least one byte, so a count beyond the body fails as
    /// truncated before memory grows.
    fn list<T>(
        &mut self,
        mut item: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let count = self.u32()? as usize;
        if count > self.0.len() {
            return Err(Error::Truncated);
        }
        let mut out = Vec::new();
        for _ in 0..count {
            out.push(item(self)?);
        }
        Ok(out)
    }

    fn rest(&mut self) -> &[u8] {
        std::mem::take(&mut self.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn samples() -> Vec<Message> {
        let mut v = vec![
            Message::Hello {
                version: PROTOCOL_VERSION,
            },
            Message::Data {
                channel: 7,
                bytes: b"ls -la\r".to_vec(),
            },
            Message::Data {
                channel: 0,
                bytes: Vec::new(),
            },
            Message::Resize {
                channel: 7,
                cols: 120,
                rows: 40,
            },
            Message::Close { channel: u32::MAX },
            Message::Connect {
                channel: 1,
                host: "example.org".into(),
                port: 22,
                user: "jürgen".into(),
                cols: 80,
                rows: 24,
                host_key_algs: vec!["ssh-ed25519".into(), "rsa-sha2-512".into()],
                x11: None,
                legacy: false,
                timing: Timing::default(),
                tunnel: None,
                shell: true,
                options: ShellOptions::default(),
                family: Family::Any,
                gssapi: false,
            },
            Message::Connect {
                channel: 3,
                host: "x".into(),
                port: 2222,
                user: "u".into(),
                cols: 1,
                rows: 1,
                host_key_algs: Vec::new(),
                x11: Some(X11Auth {
                    screen: 0,
                    protocol: "MIT-MAGIC-COOKIE-1".into(),
                    cookie: vec![0xab; 16],
                }),
                legacy: true,
                timing: Timing {
                    connect_timeout: 5,
                    alive_interval: 0,
                    alive_max: 1,
                },
                tunnel: Some(0x1234),
                shell: false,
                options: ShellOptions {
                    env: vec![("LANG".into(), "nl_BE.UTF-8".into())],
                    command: Some("tmux new -A -s \"my s\"".into()),
                    pty: false,
                    agent: true,
                    mosh: Some(Box::new(MoshOptions {
                        server: "/opt/bin/mosh-server".into(),
                        ports: Some((60001, 60010)),
                    })),
                },
                family: Family::V6,
                gssapi: true,
            },
            Message::HostKeyVerdict {
                channel: 1,
                accept: true,
            },
            Message::Identities {
                channel: 1,
                keys: vec![vec![1, 2, 3], Vec::new()],
            },
            Message::Signature {
                channel: 1,
                signature: Vec::new(),
            },
            Message::Answers {
                channel: 1,
                answers: Secrets(vec!["hunter2".into(), String::new()]),
            },
            Message::HostKey {
                channel: 1,
                key: vec![0; 51],
            },
            Message::HostCert {
                channel: 1,
                ca: vec![1; 51],
                key: vec![2; 51],
                principals: vec!["host.example.org".into(), "10.0.0.1".into()],
                valid_after: 0,
                valid_before: u64::MAX,
                key_id: "host-key".into(),
                serial: 7,
            },
            Message::Banner {
                channel: 1,
                text: "Authorised users only\n".into(),
            },
            Message::TelnetConnect {
                channel: 4,
                host: "console-server.lan".into(),
                port: 23,
                cols: 80,
                rows: 24,
                connect_timeout: 20,
            },
            Message::IdentitiesRequest { channel: 1 },
            Message::SignRequest {
                channel: 1,
                key: vec![9; 4],
                data: vec![8; 100],
            },
            Message::Prompt {
                channel: 1,
                kind: PromptKind::KeyboardInteractive,
                name: String::new(),
                instructions: "Use your token".into(),
                fields: vec![
                    PromptField {
                        text: "Password: ".into(),
                        echo: false,
                    },
                    PromptField {
                        text: "Code: ".into(),
                        echo: true,
                    },
                ],
            },
            Message::Notice {
                channel: 1,
                text: "key refused".into(),
            },
            Message::Connected {
                channel: 1,
                kex: "mlkem768x25519-sha256".into(),
                cipher: "chacha20-poly1305@openssh.com".into(),
                host_key: "ssh-ed25519".into(),
            },
            Message::ExitStatus {
                channel: 1,
                code: 130,
            },
            Message::Lost { channel: 1 },
            Message::Disconnected {
                channel: 1,
                reason: "Connection closed".into(),
            },
            Message::OpenChannel {
                channel: 2,
                connection: 1,
                cols: 100,
                rows: 30,
                x11: Some(X11Auth::default()),
                options: ShellOptions::default(),
            },
            Message::AgentList {
                channel: 1,
                agent: WORKER_IDS,
            },
            Message::AgentSign {
                channel: 1,
                agent: WORKER_IDS + 1,
                key: vec![1, 2, 3],
                data: vec![4; 40],
                flags: 4,
            },
            Message::AgentKeys {
                channel: 1,
                agent: WORKER_IDS,
                keys: vec![(vec![9; 51], "me@pc".into()), (Vec::new(), String::new())],
            },
            Message::AgentSigned {
                channel: 1,
                agent: WORKER_IDS + 1,
                signature: Vec::new(),
            },
            Message::GssapiStep {
                channel: 1,
                first: true,
                token: Vec::new(),
                mic_data: vec![7; 90],
            },
            Message::GssapiStep {
                channel: 1,
                first: false,
                token: vec![0x60; 300],
                mic_data: vec![7; 90],
            },
            Message::GssapiReply {
                channel: 1,
                reply: GssReply::Continue(vec![0x60; 1200]),
            },
            Message::WinrmConnect {
                channel: 4,
                host: "win-srv.lab.test".into(),
                port: 5985,
                connect_timeout: 20,
                command: "powershell -NoLogo".into(),
            },
            Message::SspiRequest {
                channel: 4,
                conn: 0,
                op: SspiOp::Start,
            },
            Message::SspiRequest {
                channel: 4,
                conn: 1,
                op: SspiOp::Step(vec![0xa1; 200]),
            },
            Message::SspiRequest {
                channel: 4,
                conn: 1,
                op: SspiOp::Seal(b"<s:Envelope/>".to_vec()),
            },
            Message::SspiRequest {
                channel: 4,
                conn: 0,
                op: SspiOp::Unseal {
                    header: vec![1; 16],
                    data: vec![2; 40],
                },
            },
            Message::SspiReply {
                channel: 4,
                conn: 0,
                result: SspiResult::Continue(vec![3; 30]),
            },
            Message::SspiReply {
                channel: 4,
                conn: 1,
                result: SspiResult::Done(Vec::new()),
            },
            Message::SspiReply {
                channel: 4,
                conn: 1,
                result: SspiResult::Sealed {
                    header: vec![1; 16],
                    data: vec![2; 40],
                },
            },
            Message::SspiReply {
                channel: 4,
                conn: 0,
                result: SspiResult::Unsealed(b"<s:Envelope/>".to_vec()),
            },
            Message::SspiReply {
                channel: 4,
                conn: 0,
                result: SspiResult::Failed("no credentials".into()),
            },
            Message::GssapiReply {
                channel: 1,
                reply: GssReply::Complete {
                    token: Vec::new(),
                    mic: vec![4; 28],
                },
            },
            Message::GssapiReply {
                channel: 1,
                reply: GssReply::Failed,
            },
            Message::ForwardOpen {
                channel: FORWARD_IDS,
                connection: 1,
                target: Some(("db.internal".into(), 5432)),
                origin_host: "127.0.0.1".into(),
                origin_port: 50123,
                keep: false,
            },
            Message::ForwardOpen {
                channel: FORWARD_IDS + 1,
                connection: 1,
                target: None,
                origin_host: "::1".into(),
                origin_port: 1,
                keep: true,
            },
            Message::ForwardOpened {
                channel: FORWARD_IDS,
                host: "db.internal".into(),
                port: 5432,
            },
            Message::RemoteForward {
                channel: FORWARD_IDS + 2,
                connection: 1,
                bind: "localhost".into(),
                port: 0,
            },
            Message::ForwardListening {
                channel: FORWARD_IDS + 2,
                port: 40001,
            },
            Message::ForwardIncoming {
                channel: WORKER_IDS,
                listener: FORWARD_IDS + 2,
                x11: false,
                origin_host: "10.0.0.9".into(),
                origin_port: 33000,
            },
            Message::Window {
                channel: WORKER_IDS,
                bytes: STREAM_WINDOW,
            },
            Message::Eof {
                channel: WORKER_IDS,
            },
            Message::SftpOpen {
                channel: 5,
                connection: 1,
            },
            Message::SftpReady {
                channel: 5,
                home: b"/home/j\xfcrgen".to_vec(),
            },
            Message::Exec {
                channel: 6,
                connection: 1,
                command: b"sh -c 'echo hi'".to_vec(),
                split_stderr: true,
            },
            Message::ExecStderr {
                channel: 6,
                bytes: b"oops\n".to_vec(),
            },
            Message::ZmodemStart {
                channel: 3,
                upload: true,
            },
            Message::ZmodemFile {
                channel: 3,
                name: b"dir/r\xe9sum\xe9.pdf".to_vec(),
                size: Some(1 << 33),
            },
            Message::ZmodemFile {
                channel: 3,
                name: b"x".to_vec(),
                size: None,
            },
            Message::ZmodemData {
                channel: 3,
                bytes: vec![0x18; 9],
            },
            Message::ZmodemRead {
                channel: 3,
                file: 2,
                offset: 1 << 31,
                len: 32768,
            },
            Message::ZmodemFileDone {
                channel: 3,
                skipped: false,
            },
            Message::ZmodemEnd {
                channel: 3,
                error: String::new(),
            },
            Message::ZmodemAnswer {
                channel: 3,
                answer: ZmodemAnswer::Off,
            },
            Message::ZmodemAnswer {
                channel: 3,
                answer: ZmodemAnswer::Decline,
            },
            Message::ZmodemAnswer {
                channel: 3,
                answer: ZmodemAnswer::Receive,
            },
            Message::ZmodemAnswer {
                channel: 3,
                answer: ZmodemAnswer::Send(vec![ZmodemUpload {
                    name: b"notes.txt".to_vec(),
                    size: 12,
                    mtime: 1_700_000_000,
                }]),
            },
            Message::ZmodemFileAnswer {
                channel: 3,
                accept: true,
            },
            Message::ZmodemChunk {
                channel: 3,
                offset: 7,
                bytes: b"chunk".to_vec(),
            },
            Message::ZmodemCancel { channel: 3 },
        ];
        let ops = [
            SftpOp::RealPath {
                path: b".".to_vec(),
            },
            SftpOp::Stat {
                path: b"/etc".to_vec(),
                follow: true,
            },
            SftpOp::List {
                path: b"/".to_vec(),
            },
            SftpOp::Open {
                path: b"a".to_vec(),
                mode: OpenMode::Read,
            },
            SftpOp::Open {
                path: b"a".to_vec(),
                mode: OpenMode::Write { truncate: false },
            },
            SftpOp::Open {
                path: b"a".to_vec(),
                mode: OpenMode::Write { truncate: true },
            },
            SftpOp::Read {
                handle: 3,
                offset: u64::MAX,
                len: 32768,
            },
            SftpOp::Write {
                handle: 3,
                offset: 1 << 40,
                data: vec![7; 10],
            },
            SftpOp::Close { handle: 3 },
            SftpOp::Mkdir {
                path: b"d".to_vec(),
            },
            SftpOp::Remove {
                path: b"f".to_vec(),
            },
            SftpOp::Rmdir {
                path: b"d".to_vec(),
            },
            SftpOp::Rename {
                from: b"a.filepart".to_vec(),
                to: b"a".to_vec(),
                overwrite: true,
            },
            SftpOp::Checksum {
                path: b"a".to_vec(),
            },
            SftpOp::Chmod {
                path: b"a".to_vec(),
                mode: 0o755,
            },
        ];
        let replies = [
            SftpReply::Done,
            SftpReply::Failed {
                code: sftp_code::NO_SUCH_FILE,
                message: "No such file".into(),
            },
            SftpReply::Path(b"/home/j".to_vec()),
            SftpReply::Attrs(FileAttrs {
                size: Some(5),
                permissions: Some(0o100_644),
                mtime: None,
            }),
            SftpReply::Attrs(FileAttrs::default()),
            SftpReply::Entries {
                entries: vec![DirEntry {
                    name: b"x\xff".to_vec(),
                    attrs: FileAttrs {
                        size: None,
                        permissions: Some(0o040_755),
                        mtime: Some(1_700_000_000),
                    },
                }],
                last: true,
            },
            SftpReply::Opened { handle: 9 },
            SftpReply::Data(vec![1, 2, 3]),
            SftpReply::Eof,
            SftpReply::Checksum("ab".repeat(32)),
        ];
        for (id, op) in (1..).zip(ops) {
            v.push(Message::SftpRequest { channel: 5, id, op });
        }
        for (id, reply) in (1..).zip(replies) {
            v.push(Message::SftpReply {
                channel: 5,
                id,
                reply,
            });
        }
        v
    }

    #[test]
    fn round_trips_every_message() {
        for msg in samples() {
            let mut wire = Vec::new();
            encode(&msg, &mut wire).unwrap();
            let mut dec = Decoder::new();
            dec.feed(&wire);
            assert_eq!(dec.next_message().unwrap(), Some(msg));
            assert_eq!(dec.next_message().unwrap(), None);
        }
    }

    #[test]
    fn worker_streams_are_sent_by_the_worker() {
        // The ones a compromised worker could abuse most if the UI took
        // them from it; and what the UI must never take from the worker.
        for msg in samples() {
            let side = msg.sent_by();
            match msg {
                Message::SignRequest { .. }
                | Message::Prompt { .. }
                | Message::HostKey { .. }
                | Message::GssapiStep { .. }
                | Message::SspiRequest { .. } => {
                    assert_eq!(side, Side::Worker);
                }
                Message::Connect { .. }
                | Message::Answers { .. }
                | Message::Signature { .. }
                | Message::GssapiReply { .. }
                | Message::WinrmConnect { .. }
                | Message::SspiReply { .. } => {
                    assert_eq!(side, Side::Ui);
                }
                Message::Hello { .. } | Message::Data { .. } | Message::Close { .. } => {
                    assert_eq!(side, Side::Either);
                }
                _ => {}
            }
        }
    }

    #[test]
    fn every_tag_has_a_sample() {
        let mut tags: Vec<u8> = samples().iter().map(Message::tag).collect();
        tags.sort_unstable();
        tags.dedup();
        // ZMODEM starts at 40, leaving room below; Kerberos at 50, WinRM
        // at 60.
        let want: Vec<u8> = (0..=38).chain(40..=51).chain(60..=62).collect();
        assert_eq!(tags, want);
    }

    #[test]
    fn reassembles_byte_by_byte() {
        let mut wire = Vec::new();
        for msg in samples() {
            encode(&msg, &mut wire).unwrap();
        }
        let mut dec = Decoder::new();
        let mut got = Vec::new();
        for b in wire {
            dec.feed(&[b]);
            while let Some(msg) = dec.next_message().unwrap() {
                got.push(msg);
            }
        }
        assert_eq!(got, samples());
    }

    #[test]
    fn rejects_oversized_length_before_buffering_body() {
        let mut dec = Decoder::new();
        dec.feed(&(MAX_BODY_LEN as u32 + 1).to_le_bytes());
        assert_eq!(dec.next_message(), Err(Error::TooLarge(MAX_BODY_LEN + 1)));
    }

    #[test]
    fn rejects_oversized_encode() {
        let msg = Message::Data {
            channel: 1,
            bytes: vec![0; MAX_BODY_LEN],
        };
        assert!(matches!(
            encode(&msg, &mut Vec::new()),
            Err(Error::TooLarge(_))
        ));
    }

    #[test]
    fn rejects_malformed_bodies() {
        let v = PROTOCOL_VERSION;
        let cases: [(&[u8], Error); 11] = [
            (&[v + 1, 0, 1], Error::Version(v + 1)),
            (&[v, 200], Error::Tag(200)),
            (&[v, 2, 1, 0], Error::Truncated),
            (&[v, 0, 1, 0xff], Error::Trailing(1)),
            // HostKeyVerdict with a boolean of 2.
            (&[v, 5, 1, 0, 0, 0, 2], Error::Invalid),
            // Banner whose text is not UTF-8.
            (&[v, 10, 1, 0, 0, 0, 1, 0, 0, 0, 0xff], Error::Invalid),
            // Banner claiming more bytes than the body holds.
            (&[v, 10, 1, 0, 0, 0, 9, 0, 0, 0, b'a'], Error::Truncated),
            // Identities claiming four billion keys.
            (
                &[v, 6, 1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
                Error::Truncated,
            ),
            // Prompt of an unknown kind.
            (&[v, 13, 1, 0, 0, 0, 7], Error::Invalid),
            // An SFTP request with an unknown operation.
            (&[v, 21, 1, 0, 0, 0, 1, 0, 0, 0, 99], Error::Invalid),
            // An SFTP open with an unknown mode.
            (
                &[v, 21, 1, 0, 0, 0, 1, 0, 0, 0, 3, 1, 0, 0, 0, b'a', 3],
                Error::Invalid,
            ),
        ];
        for (body, want) in cases {
            let mut dec = Decoder::new();
            dec.feed(&(body.len() as u32).to_le_bytes());
            dec.feed(body);
            assert_eq!(dec.next_message(), Err(want), "{body:?}");
        }
    }

    #[test]
    fn x11_cookies_are_not_printed() {
        let auth = X11Auth {
            screen: 0,
            protocol: "MIT-MAGIC-COOKIE-1".into(),
            cookie: b"sekrit-cookie".to_vec(),
        };
        assert!(!format!("{auth:?}").contains("sekrit"));
    }

    #[test]
    fn channel_ranges() {
        assert!(!is_forward_channel(1));
        assert!(!is_forward_channel(FORWARD_IDS - 1));
        assert!(is_forward_channel(FORWARD_IDS));
        assert!(is_forward_channel(WORKER_IDS));
        assert!(is_forward_channel(u32::MAX));
    }

    #[test]
    fn secrets_are_not_printed() {
        let msg = Message::Answers {
            channel: 1,
            answers: Secrets(vec!["hunter2".into()]),
        };
        assert!(!format!("{msg:?}").contains("hunter2"));
    }
}
