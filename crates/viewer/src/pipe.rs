//! The pipe protocol between the window and the viewer process.
//!
//! Each message is a frame: a little-endian `u32` length, then that many
//! bytes, the first of which is the message type. Both sides check every
//! frame strictly: the viewer because the window relays what the server
//! sends, the window because the viewer may have been taken over by that
//! server. A frame that does not decode ends the session.

use std::io::{self, Read, Write};

use zeroize::{Zeroize, Zeroizing};

use crate::{MAX_PIXELS, MAX_SIDE};

/// A password on its way to the viewer: wiped from memory when dropped,
/// and never shown by `Debug`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// The largest frame either side sends: big pixel rectangles go in
/// stripes below this.
pub const MAX_FRAME: usize = 16 * 1024 * 1024 + 64;
/// The most pixels in one [`FromViewer::Pixels`].
pub const MAX_STRIPE_PIXELS: usize = 4 * 1024 * 1024;
/// The most bytes in one `Net` message.
pub const MAX_NET: usize = 1024 * 1024;
/// The longest text in a message.
const MAX_TEXT: usize = 4096;
/// The longest VNC password; VNC uses only eight bytes.
const MAX_PASSWORD: usize = 256;
/// The most clipboard text either way, in UTF-8 bytes: a copy that is
/// longer stays where it was copied.
pub const MAX_CLIP: usize = 1024 * 1024;

/// Which protocol a viewer session speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Vnc,
    Rdp,
}

/// Window to viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToViewer {
    /// The first message: the protocol, the pane's size (which an RDP
    /// server gives the desktop, and a VNC server that can is asked
    /// for), the Windows keyboard layout id and the user name to offer.
    Start {
        protocol: Protocol,
        width: u16,
        height: u16,
        layout: u32,
        user: String,
    },
    /// Bytes from the server.
    Net(Vec<u8>),
    /// The server's connection closed.
    Closed,
    /// A key went down or up: its virtual-key code, scan code and
    /// whether it is an extended key.
    Key {
        vk: u16,
        scan: u16,
        extended: bool,
        down: bool,
    },
    /// A typed character (`WM_CHAR`).
    Char(char),
    Pointer {
        buttons: u8,
        x: u16,
        y: u16,
    },
    /// The user name to sign in as, sent before [`ToViewer::Password`].
    User(String),
    Password(Secret),
    /// The pane lost the keyboard: let go of every key held down.
    Release,
    /// The pane is now this size: a server that can should give the
    /// desktop that size.
    Resize {
        width: u16,
        height: u16,
    },
    /// The text on this PC's clipboard, for the server's.
    Clipboard(String),
    /// Which ways copied text may travel: `to_host` lets the viewer offer
    /// [`ToViewer::Clipboard`] to the server, `from_host` lets it fetch
    /// and pass on what was copied there. Both are off until this comes.
    ClipboardRules {
        to_host: bool,
        from_host: bool,
    },
}

/// Viewer to window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromViewer {
    /// Bytes for the VNC server.
    Net(Vec<u8>),
    Size {
        width: u16,
        height: u16,
    },
    Name(String),
    Pixels {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        data: Vec<u32>,
    },
    Copy {
        src_x: u16,
        src_y: u16,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
    Bell,
    NeedPassword,
    Failed(String),
    /// Text copied on the remote desktop.
    Clipboard(String),
    /// The VNC server let the viewer in without a password.
    NoPassword,
}

/// Writes one frame. The body is not copied, so a password in it leaves
/// no copy behind.
pub fn write_frame(w: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| io::Error::other("frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// The encoded [`ToViewer::Password`] tag.
const PASSWORD_TAG: u8 = 5;

/// Encodes `msg` for [`write_frame`]; the bytes are wiped when dropped,
/// which matters for a [`ToViewer::Password`].
pub fn encode_wiped(msg: &ToViewer) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(msg.encode())
}

/// Decodes a frame from the window and, when it held a password, wipes
/// the frame.
pub fn decode_wiped(mut frame: Vec<u8>) -> Option<ToViewer> {
    let msg = ToViewer::decode(&frame);
    if frame.first() == Some(&PASSWORD_TAG) {
        frame.zeroize();
    }
    msg
}

/// Reads one frame; `None` at a clean end of the pipe.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad frame length",
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

struct Fields<'a> {
    data: &'a [u8],
}

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.data.len() < n {
            return None;
        }
        let (a, b) = self.data.split_at(n);
        self.data = b;
        Some(a)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    /// The rest, which must be at most `max` bytes.
    fn rest(&mut self, max: usize) -> Option<&'a [u8]> {
        let r = self.data;
        self.data = &[];
        (r.len() <= max).then_some(r)
    }

    fn text(&mut self) -> Option<String> {
        let b = self.rest(MAX_TEXT)?;
        let s = std::str::from_utf8(b).ok()?;
        Some(s.chars().filter(|c| !c.is_control()).collect())
    }

    fn flag(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// A length-prefixed UTF-8 string of at most `max` bytes, without
    /// control characters.
    fn string(&mut self, max: usize) -> Option<String> {
        let len = usize::from(self.u16()?);
        if len > max {
            return None;
        }
        let s = std::str::from_utf8(self.take(len)?).ok()?;
        (!s.chars().any(char::is_control)).then(|| s.to_owned())
    }

    /// The rest as clipboard text: UTF-8, at most [`MAX_CLIP`] bytes, no
    /// NUL (which would cut it short on Windows).
    fn clip(&mut self) -> Option<String> {
        let s = std::str::from_utf8(self.rest(MAX_CLIP)?).ok()?;
        (!s.contains('\0')).then(|| s.to_owned())
    }

    fn end(&self) -> Option<()> {
        self.data.is_empty().then_some(())
    }
}

/// The longest user name.
const MAX_USER: usize = 256;

fn put_string(v: &mut Vec<u8>, text: &str) {
    let mut end = text.len().min(MAX_USER);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let part = text.get(..end).unwrap_or_default();
    v.extend(u16::try_from(part.len()).unwrap_or(0).to_le_bytes());
    v.extend(part.as_bytes());
}

/// Clipboard text as sent: up to the first NUL, at most [`MAX_CLIP`]
/// bytes (cut at a character boundary).
fn clip_bytes(text: &str) -> &[u8] {
    let text = text.split('\0').next().unwrap_or_default();
    let mut end = text.len().min(MAX_CLIP);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.as_bytes().get(..end).unwrap_or_default()
}

/// Clipboard text in the form both sides send: up to the first NUL and
/// at most [`MAX_CLIP`] bytes, or `None` when that leaves nothing.
pub fn clip_text(text: &str) -> Option<String> {
    let b = clip_bytes(text);
    (!b.is_empty()).then(|| String::from_utf8_lossy(b).into_owned())
}

impl ToViewer {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::new();
        match self {
            ToViewer::Net(b) => {
                v.push(1);
                v.extend(b);
            }
            ToViewer::Closed => v.push(2),
            ToViewer::Key {
                vk,
                scan,
                extended,
                down,
            } => {
                v.push(3);
                v.extend(vk.to_le_bytes());
                v.extend(scan.to_le_bytes());
                v.extend([u8::from(*extended), u8::from(*down)]);
            }
            ToViewer::Pointer { buttons, x, y } => {
                v.extend([4, *buttons]);
                v.extend(x.to_le_bytes());
                v.extend(y.to_le_bytes());
            }
            ToViewer::Password(p) => {
                v.push(5);
                v.extend(p.as_bytes());
            }
            ToViewer::Start {
                protocol,
                width,
                height,
                layout,
                user,
            } => {
                v.push(6);
                v.push(match protocol {
                    Protocol::Vnc => 0,
                    Protocol::Rdp => 1,
                });
                v.extend(width.to_le_bytes());
                v.extend(height.to_le_bytes());
                v.extend(layout.to_le_bytes());
                put_string(&mut v, user);
            }
            ToViewer::Char(c) => {
                v.push(7);
                v.extend(u32::from(*c).to_le_bytes());
            }
            ToViewer::User(u) => {
                v.push(8);
                put_string(&mut v, u);
            }
            ToViewer::Release => v.push(9),
            ToViewer::Resize { width, height } => {
                v.push(10);
                v.extend(width.to_le_bytes());
                v.extend(height.to_le_bytes());
            }
            ToViewer::Clipboard(text) => {
                v.push(11);
                v.extend(clip_bytes(text));
            }
            ToViewer::ClipboardRules { to_host, from_host } => {
                v.extend([12, u8::from(*to_host), u8::from(*from_host)]);
            }
        }
        v
    }

    pub fn decode(frame: &[u8]) -> Option<Self> {
        let mut f = Fields { data: frame };
        let msg = match f.u8()? {
            1 => ToViewer::Net(f.rest(MAX_NET)?.to_vec()),
            2 => ToViewer::Closed,
            3 => ToViewer::Key {
                vk: f.u16()?,
                scan: f.u16()?,
                extended: f.flag()?,
                down: f.flag()?,
            },
            4 => ToViewer::Pointer {
                buttons: f.u8()?,
                x: f.u16()?,
                y: f.u16()?,
            },
            5 => ToViewer::Password(Secret::new(f.rest(MAX_PASSWORD)?.to_vec())),
            6 => {
                let protocol = match f.u8()? {
                    0 => Protocol::Vnc,
                    1 => Protocol::Rdp,
                    _ => return None,
                };
                let (width, height) = (f.u16()?, f.u16()?);
                if width > MAX_SIDE || height > MAX_SIDE {
                    return None;
                }
                ToViewer::Start {
                    protocol,
                    width,
                    height,
                    layout: f.u32()?,
                    user: f.string(MAX_USER)?,
                }
            }
            7 => ToViewer::Char(char::from_u32(f.u32()?)?),
            8 => ToViewer::User(f.string(MAX_USER)?),
            9 => ToViewer::Release,
            10 => {
                let (width, height) = (f.u16()?, f.u16()?);
                if width > MAX_SIDE || height > MAX_SIDE {
                    return None;
                }
                ToViewer::Resize { width, height }
            }
            11 => ToViewer::Clipboard(f.clip()?),
            12 => ToViewer::ClipboardRules {
                to_host: f.flag()?,
                from_host: f.flag()?,
            },
            _ => return None,
        };
        f.end()?;
        Some(msg)
    }
}

impl FromViewer {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::new();
        let u16s = |v: &mut Vec<u8>, ns: &[u16]| {
            for n in ns {
                v.extend(n.to_le_bytes());
            }
        };
        match self {
            FromViewer::Net(b) => {
                v.push(1);
                v.extend(b);
            }
            FromViewer::Size { width, height } => {
                v.push(2);
                u16s(&mut v, &[*width, *height]);
            }
            FromViewer::Name(n) => {
                v.push(3);
                v.extend(n.as_bytes().iter().take(MAX_TEXT));
            }
            FromViewer::Pixels {
                x,
                y,
                width,
                height,
                data,
            } => {
                v.reserve(9 + data.len() * 4);
                v.push(4);
                u16s(&mut v, &[*x, *y, *width, *height]);
                for p in data {
                    v.extend(p.to_le_bytes());
                }
            }
            FromViewer::Copy {
                src_x,
                src_y,
                x,
                y,
                width,
                height,
            } => {
                v.push(5);
                u16s(&mut v, &[*src_x, *src_y, *x, *y, *width, *height]);
            }
            FromViewer::Bell => v.push(6),
            FromViewer::NeedPassword => v.push(7),
            FromViewer::Failed(why) => {
                v.push(8);
                let mut end = why.len().min(MAX_TEXT);
                while !why.is_char_boundary(end) {
                    end -= 1;
                }
                v.extend(why.as_bytes().get(..end).unwrap_or_default());
            }
            FromViewer::Clipboard(text) => {
                v.push(9);
                v.extend(clip_bytes(text));
            }
            FromViewer::NoPassword => v.push(10),
        }
        v
    }

    /// Decodes a frame from the viewer. Only checks the frame on its own:
    /// whether a rectangle fits the screen is
    /// [`Framebuffer`](crate::framebuffer::Framebuffer)'s to check.
    pub fn decode(frame: &[u8]) -> Option<Self> {
        let mut f = Fields { data: frame };
        let msg = match f.u8()? {
            1 => FromViewer::Net(f.rest(MAX_NET)?.to_vec()),
            2 => {
                let (width, height) = (f.u16()?, f.u16()?);
                if width > MAX_SIDE
                    || height > MAX_SIDE
                    || usize::from(width) * usize::from(height) > MAX_PIXELS
                {
                    return None;
                }
                FromViewer::Size { width, height }
            }
            3 => FromViewer::Name(f.text()?),
            4 => {
                let (x, y, width, height) = (f.u16()?, f.u16()?, f.u16()?, f.u16()?);
                let n = usize::from(width) * usize::from(height);
                if n == 0 || n > MAX_STRIPE_PIXELS {
                    return None;
                }
                let bytes = f.take(n * 4)?;
                let data = bytes.chunks_exact(4).map(crate::rgb).collect();
                FromViewer::Pixels {
                    x,
                    y,
                    width,
                    height,
                    data,
                }
            }
            5 => FromViewer::Copy {
                src_x: f.u16()?,
                src_y: f.u16()?,
                x: f.u16()?,
                y: f.u16()?,
                width: f.u16()?,
                height: f.u16()?,
            },
            6 => FromViewer::Bell,
            7 => FromViewer::NeedPassword,
            8 => FromViewer::Failed(f.text()?),
            9 => FromViewer::Clipboard(f.clip()?),
            10 => FromViewer::NoPassword,
            _ => return None,
        };
        f.end()?;
        Some(msg)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_never_shown_and_frames_are_wiped() {
        let msg = ToViewer::Password(Secret::new(b"hunter2".to_vec()));
        // Neither assert prints the message: a failure must not show it.
        assert!(!format!("{msg:?}").contains("hunter2"));
        let frame = encode_wiped(&msg);
        assert!(decode_wiped(frame.to_vec()) == Some(msg));
        // Other messages decode as before.
        let other = ToViewer::User("me".into());
        assert_eq!(decode_wiped(other.encode()), Some(other));
        assert_eq!(decode_wiped(vec![5; MAX_PASSWORD + 2]), None);
    }

    #[test]
    fn messages_round_trip() {
        for m in [
            ToViewer::Net(vec![1, 2, 3]),
            ToViewer::Closed,
            ToViewer::Key {
                vk: 0x0d,
                scan: 0x1c,
                extended: false,
                down: true,
            },
            ToViewer::Char('é'),
            ToViewer::User("jurgen".into()),
            ToViewer::Release,
            ToViewer::Resize {
                width: 1920,
                height: 1080,
            },
            ToViewer::Start {
                protocol: Protocol::Rdp,
                width: 1280,
                height: 800,
                layout: 0x0813,
                user: "jurgen".into(),
            },
            ToViewer::Pointer {
                buttons: 5,
                x: 1000,
                y: 2,
            },
            ToViewer::Password(Secret::new(b"pw".to_vec())),
            ToViewer::Clipboard("a\r\nb\té".into()),
            ToViewer::ClipboardRules {
                to_host: true,
                from_host: false,
            },
        ] {
            assert_eq!(ToViewer::decode(&m.encode()), Some(m));
        }
        for m in [
            FromViewer::Net(vec![9]),
            FromViewer::Size {
                width: 1920,
                height: 1080,
            },
            FromViewer::Name("desk".into()),
            FromViewer::Pixels {
                x: 1,
                y: 2,
                width: 2,
                height: 1,
                data: vec![0xffffff, 0x123456],
            },
            FromViewer::Copy {
                src_x: 0,
                src_y: 1,
                x: 2,
                y: 3,
                width: 4,
                height: 5,
            },
            FromViewer::Bell,
            FromViewer::NeedPassword,
            FromViewer::NoPassword,
            FromViewer::Failed("no".into()),
            FromViewer::Clipboard("line\nnext".into()),
        ] {
            assert_eq!(FromViewer::decode(&m.encode()), Some(m));
        }
    }

    #[test]
    fn bad_frames_are_refused() {
        assert_eq!(FromViewer::decode(&[]), None);
        assert_eq!(FromViewer::decode(&[99]), None);
        // Pixels short by a byte, or with one too many.
        let mut p = FromViewer::Pixels {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            data: vec![1],
        }
        .encode();
        assert!(FromViewer::decode(&p).is_some());
        p.push(0);
        assert_eq!(FromViewer::decode(&p), None);
        p.truncate(p.len() - 2);
        assert_eq!(FromViewer::decode(&p), None);
        // A screen beyond the limit.
        let mut s = vec![2];
        s.extend(9000u16.to_le_bytes());
        s.extend(10u16.to_le_bytes());
        assert_eq!(FromViewer::decode(&s), None);
        // Trailing bytes and bad flags.
        assert_eq!(ToViewer::decode(&[2, 0]), None);
        assert_eq!(ToViewer::decode(&[3, 0, 0, 0, 0, 2, 0]), None);
        assert_eq!(ToViewer::decode(&[7, 0, 0xd8, 0, 0]), None, "a surrogate");
        assert_eq!(
            ToViewer::decode(&[8, 1, 0, b'\n']),
            None,
            "control characters"
        );
        assert_eq!(FromViewer::decode(&[3, 0xff]), None, "not UTF-8");
        // Clipboard text: no NUL, UTF-8, within the limit.
        assert_eq!(FromViewer::decode(&[9, b'a', 0, b'b']), None);
        assert_eq!(FromViewer::decode(&[9, 0xc3]), None);
        let mut big = vec![9];
        big.resize(MAX_CLIP + 2, b'x');
        assert_eq!(FromViewer::decode(&big), None);
        assert_eq!(ToViewer::decode(&[12, 1, 2]), None);
        // Encoding cuts at a NUL and at the limit, on a character.
        let m = FromViewer::Clipboard("ab\0cd".into()).encode();
        assert_eq!(
            FromViewer::decode(&m),
            Some(FromViewer::Clipboard("ab".into()))
        );
        let long = "é".repeat(MAX_CLIP);
        let Some(FromViewer::Clipboard(t)) =
            FromViewer::decode(&FromViewer::Clipboard(long).encode())
        else {
            panic!("a long copy did not decode");
        };
        assert_eq!(t.len(), MAX_CLIP);
        assert_eq!(clip_text("\0x"), None);
    }

    #[test]
    fn frames() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"\x02").unwrap();
        write_frame(&mut buf, b"\x06").unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).unwrap(), Some(vec![2]));
        assert_eq!(read_frame(&mut r).unwrap(), Some(vec![6]));
        assert_eq!(read_frame(&mut r).unwrap(), None);
        let huge = (MAX_FRAME as u32 + 1).to_le_bytes();
        assert!(read_frame(&mut &huge[..]).is_err());
        assert!(read_frame(&mut &[0u8, 0, 0, 0][..]).is_err());
    }
}
