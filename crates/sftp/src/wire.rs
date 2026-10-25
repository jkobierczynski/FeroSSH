//! SFTP version 3 (draft-ietf-secsh-filexfer-02, what OpenSSH speaks), the
//! client's half: the packets a client sends, and a decoder for what the
//! server sends back.
//!
//! The worker runs this against an untrusted server, so the decoder caps
//! packet and field sizes, rejects attribute flags it cannot parse, and
//! never panics. Names stay bytes: a server's file names need not be
//! UTF-8.

use std::fmt;

use ferossh_ipc::{DirEntry, FileAttrs};

/// The protocol version FeroSSH asks for.
pub const VERSION: u32 = 3;

/// Largest packet accepted from the server. OpenSSH's own limit is 256 KiB;
/// reads ask for far less.
pub const MAX_PACKET: usize = 256 * 1024 + 1024;

/// Longest handle a server may hand out (the draft says 256 bytes).
const MAX_HANDLE: usize = 256;

pub mod pflags {
    pub const READ: u32 = 0x01;
    pub const WRITE: u32 = 0x02;
    pub const CREAT: u32 = 0x08;
    pub const TRUNC: u32 = 0x10;
}

mod kind {
    pub const INIT: u8 = 1;
    pub const VERSION: u8 = 2;
    pub const OPEN: u8 = 3;
    pub const CLOSE: u8 = 4;
    pub const READ: u8 = 5;
    pub const WRITE: u8 = 6;
    pub const LSTAT: u8 = 7;
    pub const OPENDIR: u8 = 11;
    pub const READDIR: u8 = 12;
    pub const REMOVE: u8 = 13;
    pub const MKDIR: u8 = 14;
    pub const RMDIR: u8 = 15;
    pub const REALPATH: u8 = 16;
    pub const STAT: u8 = 17;
    pub const RENAME: u8 = 18;
    pub const STATUS: u8 = 101;
    pub const HANDLE: u8 = 102;
    pub const DATA: u8 = 103;
    pub const NAME: u8 = 104;
    pub const ATTRS: u8 = 105;
    pub const EXTENDED: u8 = 200;
    pub const EXTENDED_REPLY: u8 = 201;
}

mod attr {
    pub const SIZE: u32 = 0x01;
    pub const UIDGID: u32 = 0x02;
    pub const PERMISSIONS: u32 = 0x04;
    pub const ACMODTIME: u32 = 0x08;
    pub const EXTENDED: u32 = 0x8000_0000;
    pub const KNOWN: u32 = SIZE | UIDGID | PERMISSIONS | ACMODTIME | EXTENDED;
}

/// A packet the client sends. Every request but [`Request::Init`] carries
/// an id that the server's answer repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request<'a> {
    Init,
    Open {
        id: u32,
        path: &'a [u8],
        pflags: u32,
    },
    Close {
        id: u32,
        handle: &'a [u8],
    },
    Read {
        id: u32,
        handle: &'a [u8],
        offset: u64,
        len: u32,
    },
    Write {
        id: u32,
        handle: &'a [u8],
        offset: u64,
        data: &'a [u8],
    },
    Lstat {
        id: u32,
        path: &'a [u8],
    },
    Stat {
        id: u32,
        path: &'a [u8],
    },
    OpenDir {
        id: u32,
        path: &'a [u8],
    },
    ReadDir {
        id: u32,
        handle: &'a [u8],
    },
    Remove {
        id: u32,
        path: &'a [u8],
    },
    Mkdir {
        id: u32,
        path: &'a [u8],
    },
    Rmdir {
        id: u32,
        path: &'a [u8],
    },
    RealPath {
        id: u32,
        path: &'a [u8],
    },
    Rename {
        id: u32,
        from: &'a [u8],
        to: &'a [u8],
    },
    /// `posix-rename@openssh.com`: a rename that replaces the target.
    PosixRename {
        id: u32,
        from: &'a [u8],
        to: &'a [u8],
    },
}

/// The extension that makes [`Request::PosixRename`] available.
pub const POSIX_RENAME: &[u8] = b"posix-rename@openssh.com";

struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }

    fn bytes(&mut self, v: &[u8]) {
        self.u32(u32::try_from(v.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(v);
    }

    fn id_path(&mut self, kind: u8, id: u32, path: &[u8]) {
        self.u8(kind);
        self.u32(id);
        self.bytes(path);
    }
}

/// The packet for `req`, length prefix included.
pub fn encode(req: &Request<'_>) -> Vec<u8> {
    let mut o = Out(vec![0; 4]);
    match *req {
        Request::Init => {
            o.u8(kind::INIT);
            o.u32(VERSION);
        }
        Request::Open { id, path, pflags } => {
            o.id_path(kind::OPEN, id, path);
            o.u32(pflags);
            // No attributes: the server's umask decides the mode.
            o.u32(0);
        }
        Request::Close { id, handle } => o.id_path(kind::CLOSE, id, handle),
        Request::Read {
            id,
            handle,
            offset,
            len,
        } => {
            o.id_path(kind::READ, id, handle);
            o.u64(offset);
            o.u32(len);
        }
        Request::Write {
            id,
            handle,
            offset,
            data,
        } => {
            o.id_path(kind::WRITE, id, handle);
            o.u64(offset);
            o.bytes(data);
        }
        Request::Lstat { id, path } => o.id_path(kind::LSTAT, id, path),
        Request::Stat { id, path } => o.id_path(kind::STAT, id, path),
        Request::OpenDir { id, path } => o.id_path(kind::OPENDIR, id, path),
        Request::ReadDir { id, handle } => o.id_path(kind::READDIR, id, handle),
        Request::Remove { id, path } => o.id_path(kind::REMOVE, id, path),
        Request::Mkdir { id, path } => {
            o.id_path(kind::MKDIR, id, path);
            o.u32(0);
        }
        Request::Rmdir { id, path } => o.id_path(kind::RMDIR, id, path),
        Request::RealPath { id, path } => o.id_path(kind::REALPATH, id, path),
        Request::Rename { id, from, to } => {
            o.id_path(kind::RENAME, id, from);
            o.bytes(to);
        }
        Request::PosixRename { id, from, to } => {
            o.id_path(kind::EXTENDED, id, POSIX_RENAME);
            o.bytes(from);
            o.bytes(to);
        }
    }
    let len = u32::try_from(o.0.len() - 4).unwrap_or(u32::MAX);
    if let Some(prefix) = o.0.get_mut(..4) {
        prefix.copy_from_slice(&len.to_be_bytes());
    }
    o.0
}

/// A packet from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Version {
        version: u32,
        /// Extension names the server announced.
        extensions: Vec<Vec<u8>>,
    },
    Status {
        id: u32,
        code: u32,
        /// The server's text, not yet sanitised.
        message: String,
    },
    Handle {
        id: u32,
        handle: Vec<u8>,
    },
    Data {
        id: u32,
        data: Vec<u8>,
    },
    /// Names, with `.` and `..` left out; `longname` is dropped.
    Name {
        id: u32,
        entries: Vec<DirEntry>,
    },
    Attrs {
        id: u32,
        attrs: FileAttrs,
    },
    ExtendedReply {
        id: u32,
    },
}

impl Response {
    /// The request id it answers; `None` for the version.
    pub fn id(&self) -> Option<u32> {
        match self {
            Response::Version { .. } => None,
            Response::Status { id, .. }
            | Response::Handle { id, .. }
            | Response::Data { id, .. }
            | Response::Name { id, .. }
            | Response::Attrs { id, .. }
            | Response::ExtendedReply { id } => Some(*id),
        }
    }
}

/// Why the server's stream was rejected. After an error the stream is out
/// of sync and the SFTP channel must close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    TooLarge(usize),
    Truncated,
    Trailing,
    /// An unknown packet type or attribute flag, or a field out of range.
    Invalid,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLarge(n) => write!(f, "an SFTP packet of {n} bytes"),
            Error::Truncated => f.write_str("a truncated SFTP packet"),
            Error::Trailing => f.write_str("an SFTP packet with trailing bytes"),
            Error::Invalid => f.write_str("a malformed SFTP packet"),
        }
    }
}

impl std::error::Error for Error {}

struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, tail) = self.0.split_first_chunk::<N>().ok_or(Error::Truncated)?;
        self.0 = tail;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        self.take::<1>().map(|[b]| b)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        self.take().map(u32::from_be_bytes)
    }

    fn u64(&mut self) -> Result<u64, Error> {
        self.take().map(u64::from_be_bytes)
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

    fn attrs(&mut self) -> Result<FileAttrs, Error> {
        let flags = self.u32()?;
        if flags & !attr::KNOWN != 0 {
            return Err(Error::Invalid);
        }
        let mut a = FileAttrs::default();
        if flags & attr::SIZE != 0 {
            a.size = Some(self.u64()?);
        }
        if flags & attr::UIDGID != 0 {
            self.u32()?;
            self.u32()?;
        }
        if flags & attr::PERMISSIONS != 0 {
            a.permissions = Some(self.u32()?);
        }
        if flags & attr::ACMODTIME != 0 {
            self.u32()?;
            a.mtime = Some(self.u32()?);
        }
        if flags & attr::EXTENDED != 0 {
            let count = self.u32()? as usize;
            // Each pair is at least eight bytes.
            if count > self.0.len() / 8 {
                return Err(Error::Truncated);
            }
            for _ in 0..count {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(a)
    }
}

fn decode_body(body: &[u8]) -> Result<Response, Error> {
    let mut r = In(body);
    let resp = match r.u8()? {
        kind::VERSION => {
            let version = r.u32()?;
            let mut extensions = Vec::new();
            while !r.0.is_empty() {
                extensions.push(r.bytes()?.to_vec());
                r.bytes()?;
            }
            Response::Version {
                version,
                extensions,
            }
        }
        kind::STATUS => {
            let id = r.u32()?;
            let code = r.u32()?;
            // Version 3 servers may leave out the message and language.
            let mut message = String::new();
            if !r.0.is_empty() {
                message = String::from_utf8_lossy(r.bytes()?).into_owned();
                if !r.0.is_empty() {
                    r.bytes()?;
                }
            }
            Response::Status { id, code, message }
        }
        kind::HANDLE => {
            let id = r.u32()?;
            let handle = r.bytes()?;
            if handle.len() > MAX_HANDLE {
                return Err(Error::Invalid);
            }
            Response::Handle {
                id,
                handle: handle.to_vec(),
            }
        }
        kind::DATA => Response::Data {
            id: r.u32()?,
            data: r.bytes()?.to_vec(),
        },
        kind::NAME => {
            let id = r.u32()?;
            let count = r.u32()? as usize;
            // Each name is at least twelve bytes: two lengths and flags.
            if count > r.0.len() / 12 {
                return Err(Error::Truncated);
            }
            let mut entries = Vec::new();
            for _ in 0..count {
                let name = r.bytes()?;
                r.bytes()?;
                let attrs = r.attrs()?;
                if name != b"." && name != b".." {
                    entries.push(DirEntry {
                        name: name.to_vec(),
                        attrs,
                    });
                }
            }
            Response::Name { id, entries }
        }
        kind::ATTRS => Response::Attrs {
            id: r.u32()?,
            attrs: r.attrs()?,
        },
        kind::EXTENDED_REPLY => {
            let id = r.u32()?;
            r.0 = &[];
            Response::ExtendedReply { id }
        }
        _ => return Err(Error::Invalid),
    };
    if r.0.is_empty() {
        Ok(resp)
    } else {
        Err(Error::Trailing)
    }
}

/// Reassembles the server's packets from channel data.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole packet, `Ok(None)` if more bytes are needed.
    pub fn next_response(&mut self) -> Result<Option<Response>, Error> {
        let Some((prefix, rest)) = self.buf.split_first_chunk::<4>() else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(*prefix) as usize;
        if len > MAX_PACKET {
            return Err(Error::TooLarge(len));
        }
        let Some(body) = rest.get(..len) else {
            return Ok(None);
        };
        let resp = decode_body(body)?;
        self.buf.drain(..4 + len);
        Ok(Some(resp))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn packet(body: &[u8]) -> Vec<u8> {
        let mut p = (body.len() as u32).to_be_bytes().to_vec();
        p.extend_from_slice(body);
        p
    }

    fn string(s: &[u8]) -> Vec<u8> {
        let mut v = (s.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(s);
        v
    }

    fn decode(bytes: &[u8]) -> Result<Option<Response>, Error> {
        let mut d = Decoder::new();
        d.feed(bytes);
        d.next_response()
    }

    #[test]
    fn encodes_requests_like_openssh_expects() {
        assert_eq!(encode(&Request::Init), [0, 0, 0, 5, 1, 0, 0, 0, 3]);
        let read = encode(&Request::Read {
            id: 7,
            handle: b"\0\0\0\x01",
            offset: 0x1_0000_0000,
            len: 32768,
        });
        assert_eq!(
            read,
            [
                0, 0, 0, 25, 5, 0, 0, 0, 7, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0,
                0x80, 0
            ]
        );
        let rename = encode(&Request::PosixRename {
            id: 1,
            from: b"a",
            to: b"b",
        });
        assert_eq!(rename[4], 200);
        assert!(
            rename
                .windows(POSIX_RENAME.len())
                .any(|w| w == POSIX_RENAME)
        );
    }

    #[test]
    fn decodes_names_and_drops_dot_entries() {
        let mut body = vec![104, 0, 0, 0, 9, 0, 0, 0, 3];
        for name in [&b"."[..], b"..", b"f\xff"] {
            body.extend(string(name));
            body.extend(string(b"-rw-r--r-- long"));
            // Size, permissions and times.
            body.extend([0, 0, 0, 0x0d]);
            body.extend(5u64.to_be_bytes());
            body.extend(0o100_644u32.to_be_bytes());
            body.extend(1u32.to_be_bytes());
            body.extend(2u32.to_be_bytes());
        }
        let Some(Response::Name { id, entries }) = decode(&packet(&body)).unwrap() else {
            panic!()
        };
        assert_eq!(id, 9);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, b"f\xff");
        assert_eq!(entries[0].attrs.size, Some(5));
        assert_eq!(entries[0].attrs.mtime, Some(2));
        assert!(entries[0].attrs.is_file());
    }

    #[test]
    fn decodes_version_and_status() {
        let mut body = vec![2, 0, 0, 0, 3];
        body.extend(string(POSIX_RENAME));
        body.extend(string(b"1"));
        assert_eq!(
            decode(&packet(&body)).unwrap(),
            Some(Response::Version {
                version: 3,
                extensions: vec![POSIX_RENAME.to_vec()]
            })
        );
        // A bare status, as some version 3 servers send it.
        let bare = packet(&[101, 0, 0, 0, 1, 0, 0, 0, 1]);
        assert_eq!(
            decode(&bare).unwrap(),
            Some(Response::Status {
                id: 1,
                code: 1,
                message: String::new()
            })
        );
    }

    #[test]
    fn rejects_hostile_packets() {
        // Larger than any reply should be.
        assert_eq!(
            decode(&(MAX_PACKET as u32 + 1).to_be_bytes()),
            Err(Error::TooLarge(MAX_PACKET + 1))
        );
        // A name list claiming a billion names.
        assert_eq!(
            decode(&packet(&[104, 0, 0, 0, 1, 0x40, 0, 0, 0])),
            Err(Error::Truncated)
        );
        // Attributes with a flag that would hide fields.
        assert_eq!(
            decode(&packet(&[105, 0, 0, 0, 1, 0, 0, 0x01, 0])),
            Err(Error::Invalid)
        );
        // A handle longer than allowed.
        let mut body = vec![102, 0, 0, 0, 1];
        body.extend(string(&[0; 300]));
        assert_eq!(decode(&packet(&body)), Err(Error::Invalid));
        // An unknown packet type, and trailing bytes.
        assert_eq!(decode(&packet(&[99])), Err(Error::Invalid));
        assert_eq!(
            decode(&packet(&[105, 0, 0, 0, 1, 0, 0, 0, 0, 7])),
            Err(Error::Trailing)
        );
        // Incomplete: wait for more.
        assert_eq!(decode(&[0, 0, 0, 9, 105]), Ok(None));
    }

    #[test]
    fn reassembles_split_packets() {
        let mut stream = packet(&[103, 0, 0, 0, 4, 0, 0, 0, 2, b'h', b'i']);
        stream.extend(packet(&[101, 0, 0, 0, 5, 0, 0, 0, 0]));
        let mut d = Decoder::new();
        let mut got = Vec::new();
        for b in stream {
            d.feed(&[b]);
            while let Some(r) = d.next_response().unwrap() {
                got.push(r.id());
            }
        }
        assert_eq!(got, [Some(4), Some(5)]);
    }
}
