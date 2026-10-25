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
//! answers. One `channel` is one SSH connection with its shell.

use std::fmt;

use zeroize::Zeroize;

/// Protocol version carried in every frame. Bump on any wire change.
pub const PROTOCOL_VERSION: u8 = 2;

/// Largest frame body accepted, in bytes. Terminal and SFTP data is chunked
/// below this, so anything larger is a bug or an attack.
pub const MAX_BODY_LEN: usize = 1 << 20;

/// Largest terminal data chunk either side puts in one [`Message::Data`].
pub const MAX_DATA_CHUNK: usize = 64 * 1024;

const LEN_PREFIX: usize = 4;

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
    /// already holds for this host; the worker asks for those first.
    Connect {
        channel: ChannelId,
        host: String,
        port: u16,
        user: String,
        cols: u16,
        rows: u16,
        host_key_algs: Vec<String>,
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
            | Message::HostKeyVerdict { channel, .. }
            | Message::Identities { channel, .. }
            | Message::Signature { channel, .. }
            | Message::Answers { channel, .. }
            | Message::HostKey { channel, .. }
            | Message::Banner { channel, .. }
            | Message::IdentitiesRequest { channel }
            | Message::SignRequest { channel, .. }
            | Message::Prompt { channel, .. }
            | Message::Notice { channel, .. }
            | Message::Connected { channel, .. }
            | Message::ExitStatus { channel, .. }
            | Message::Disconnected { channel, .. } => *channel,
        })
    }
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

    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
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
        Message::Close { channel } | Message::IdentitiesRequest { channel } => w.u32(*channel),
        Message::Connect {
            channel,
            host,
            port,
            user,
            cols,
            rows,
            host_key_algs,
        } => {
            w.u32(*channel);
            w.str(host);
            w.u16(*port);
            w.str(user);
            w.u16(*cols);
            w.u16(*rows);
            w.list(host_key_algs, |w, s| w.str(s));
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

    fn bool(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Invalid),
        }
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
        vec![
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
            Message::Banner {
                channel: 1,
                text: "Authorised users only\n".into(),
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
        ]
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
    fn every_tag_has_a_sample() {
        let mut tags: Vec<u8> = samples().iter().map(Message::tag).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags, (0..=17).collect::<Vec<u8>>());
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
        let cases: [(&[u8], Error); 9] = [
            (&[9, 0, 1], Error::Version(9)),
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
        ];
        for (body, want) in cases {
            let mut dec = Decoder::new();
            dec.feed(&(body.len() as u32).to_le_bytes());
            dec.feed(body);
            assert_eq!(dec.next_message(), Err(want), "{body:?}");
        }
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
