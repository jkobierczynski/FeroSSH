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
pub const PROTOCOL_VERSION: u8 = 10;

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
    /// `sha256sum` (an exec channel, not SFTP). Fails where the server has
    /// no such command.
    Checksum {
        path: Vec<u8>,
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
            | Message::OpenChannel { .. }
            | Message::HostKeyVerdict { .. }
            | Message::Identities { .. }
            | Message::Signature { .. }
            | Message::Answers { .. }
            | Message::SftpOpen { .. }
            | Message::SftpRequest { .. }
            | Message::ForwardOpen { .. }
            | Message::RemoteForward { .. } => Side::Ui,
            Message::HostKey { .. }
            | Message::HostCert { .. }
            | Message::Banner { .. }
            | Message::IdentitiesRequest { .. }
            | Message::SignRequest { .. }
            | Message::Prompt { .. }
            | Message::Notice { .. }
            | Message::Connected { .. }
            | Message::ExitStatus { .. }
            | Message::Disconnected { .. }
            | Message::SftpReady { .. }
            | Message::SftpReply { .. }
            | Message::ForwardOpened { .. }
            | Message::ForwardListening { .. }
            | Message::ForwardIncoming { .. } => Side::Worker,
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
            | Message::Disconnected { channel, .. }
            | Message::OpenChannel { channel, .. }
            | Message::SftpOpen { channel, .. }
            | Message::SftpReady { channel, .. }
            | Message::SftpRequest { channel, .. }
            | Message::SftpReply { channel, .. }
            | Message::ForwardOpen { channel, .. }
            | Message::ForwardOpened { channel, .. }
            | Message::RemoteForward { channel, .. }
            | Message::ForwardListening { channel, .. }
            | Message::ForwardIncoming { channel, .. }
            | Message::Window { channel, .. }
            | Message::Eof { channel } => *channel,
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
        | Message::Eof { channel } => w.u32(*channel),
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
        } => {
            w.u32(*channel);
            w.u32(*connection);
            w.u16(*cols);
            w.u16(*rows);
            w.x11(x11);
        }
        Message::SftpOpen {
            channel,
            connection,
        } => {
            w.u32(*channel);
            w.u32(*connection);
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
        },
        19 => Message::SftpOpen {
            channel: r.u32()?,
            connection: r.u32()?,
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
                Message::SignRequest { .. } | Message::Prompt { .. } | Message::HostKey { .. } => {
                    assert_eq!(side, Side::Worker);
                }
                Message::Connect { .. } | Message::Answers { .. } | Message::Signature { .. } => {
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
        assert_eq!(tags, (0..=31).collect::<Vec<u8>>());
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
