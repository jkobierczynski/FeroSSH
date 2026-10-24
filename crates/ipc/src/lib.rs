//! Message types and framing for the pipe between the UI process and the
//! sandboxed SSH worker.
//!
//! Every frame is a little-endian `u32` body length followed by the body.
//! The body starts with the protocol version and a message tag; the rest is
//! the message's fields in a fixed order. Both sides treat the other as
//! untrusted: lengths are capped, unknown tags and versions are errors, and
//! decoding never panics.

use std::fmt;

/// Protocol version carried in every frame. Bump on any wire change.
pub const PROTOCOL_VERSION: u8 = 1;

/// Largest frame body accepted, in bytes. Terminal and SFTP data is chunked
/// below this, so anything larger is a bug or an attack.
pub const MAX_BODY_LEN: usize = 1 << 20;

const LEN_PREFIX: usize = 4;

/// Identifies one SSH channel (terminal, SFTP, forward) inside a worker.
pub type ChannelId = u32;

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
}

impl Message {
    fn tag(&self) -> u8 {
        match self {
            Message::Hello { .. } => 0,
            Message::Data { .. } => 1,
            Message::Resize { .. } => 2,
            Message::Close { .. } => 3,
        }
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
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLarge(n) => write!(f, "frame body of {n} bytes exceeds {MAX_BODY_LEN}"),
            Error::Version(v) => write!(f, "unsupported protocol version {v}"),
            Error::Tag(t) => write!(f, "unknown message tag {t}"),
            Error::Truncated => f.write_str("frame body truncated"),
            Error::Trailing(n) => write!(f, "{n} trailing bytes after message"),
        }
    }
}

impl std::error::Error for Error {}

/// Appends `msg` as one frame to `out`.
pub fn encode(msg: &Message, out: &mut Vec<u8>) -> Result<(), Error> {
    let mut body = vec![PROTOCOL_VERSION, msg.tag()];
    match msg {
        Message::Hello { version } => body.push(*version),
        Message::Data { channel, bytes } => {
            body.extend_from_slice(&channel.to_le_bytes());
            body.extend_from_slice(bytes);
        }
        Message::Resize {
            channel,
            cols,
            rows,
        } => {
            body.extend_from_slice(&channel.to_le_bytes());
            body.extend_from_slice(&cols.to_le_bytes());
            body.extend_from_slice(&rows.to_le_bytes());
        }
        Message::Close { channel } => body.extend_from_slice(&channel.to_le_bytes()),
    }
    if body.len() > MAX_BODY_LEN {
        return Err(Error::TooLarge(body.len()));
    }
    let len = u32::try_from(body.len()).map_err(|_| Error::TooLarge(body.len()))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(())
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
        self.buf.drain(..LEN_PREFIX + len);
        Ok(Some(msg))
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
        tag => return Err(Error::Tag(tag)),
    };
    match r.0.len() {
        0 => Ok(msg),
        n => Err(Error::Trailing(n)),
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
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
    fn rejects_bad_version_tag_truncation_and_trailing() {
        let cases: [(&[u8], Error); 4] = [
            (&[9, 0, 1], Error::Version(9)),
            (&[PROTOCOL_VERSION, 200], Error::Tag(200)),
            (&[PROTOCOL_VERSION, 2, 1, 0], Error::Truncated),
            (&[PROTOCOL_VERSION, 0, 1, 0xff], Error::Trailing(1)),
        ];
        for (body, want) in cases {
            let mut dec = Decoder::new();
            dec.feed(&(body.len() as u32).to_le_bytes());
            dec.feed(body);
            assert_eq!(dec.next_message(), Err(want));
        }
    }
}
