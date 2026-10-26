//! ZMODEM's framing: headers (hex, and binary with CRC-16 or CRC-32), data
//! subpackets, ZDLE escaping, and an incremental reader that turns a byte
//! stream into headers and subpackets.
//!
//! Every frame type and flag here is from Chuck Forsberg's 1988 protocol
//! description, as `lrzsz` implements it. The reader takes one byte at a
//! time and keeps at most one subpacket ([`MAX_SUBPACKET`]) in memory, so
//! a hostile sender can make it allocate no more than that.

/// Starts every header.
pub const ZPAD: u8 = b'*';
/// The escape byte; also CAN, five of which cancel a transfer.
pub const ZDLE: u8 = 0x18;
const ZBIN: u8 = b'A';
const ZHEX: u8 = b'B';
const ZBIN32: u8 = b'C';
const XON: u8 = 0x11;
const XOFF: u8 = 0x13;

// Frame types.
pub const ZRQINIT: u8 = 0;
pub const ZRINIT: u8 = 1;
pub const ZSINIT: u8 = 2;
pub const ZACK: u8 = 3;
pub const ZFILE: u8 = 4;
pub const ZSKIP: u8 = 5;
pub const ZNAK: u8 = 6;
pub const ZABORT: u8 = 7;
pub const ZFIN: u8 = 8;
pub const ZRPOS: u8 = 9;
pub const ZDATA: u8 = 10;
pub const ZEOF: u8 = 11;
pub const ZFERR: u8 = 12;
pub const ZCRC: u8 = 13;
pub const ZCHALLENGE: u8 = 14;
pub const ZCOMPL: u8 = 15;
pub const ZCAN: u8 = 16;
pub const ZFREECNT: u8 = 17;
pub const ZCOMMAND: u8 = 18;

// How a data subpacket ends (the byte after its ZDLE).
/// End of frame; a header follows.
pub const ZCRCE: u8 = b'h';
/// More subpackets follow, no answer wanted.
pub const ZCRCG: u8 = b'i';
/// More subpackets follow; answer with ZACK.
pub const ZCRCQ: u8 = b'j';
/// End of frame; answer with ZACK.
pub const ZCRCW: u8 = b'k';

// ZRINIT capabilities, in ZF0.
/// Full duplex: data can stream while answers come back.
pub const CANFDX: u8 = 0x01;
/// Can receive data while writing to disk.
pub const CANOVIO: u8 = 0x02;
/// Understands CRC-32 frames.
pub const CANFC32: u8 = 0x20;
/// Wants every control character escaped.
pub const ESCCTL: u8 = 0x40;

/// ZFILE ZF0: binary, no newline conversion.
pub const ZCBIN: u8 = 1;

/// The longest data subpacket accepted. `lrzsz` sends 1 KiB, or 8 KiB
/// when asked; others stay well below this.
pub const MAX_SUBPACKET: usize = 16 * 1024;

/// Five CANs in a row cancel, as in `lrzsz`.
const CANCEL_RUN: u8 = 5;

/// What both sides send to give up: CANs, then backspaces to wipe them
/// from a terminal that shows them (as `lrzsz` does).
pub const CANCEL: [u8; 20] = [
    ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
];

/// A frame header: a type and four bytes, which are a position (ZP0 is
/// the low byte) or flags (ZF0 is the last byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub data: [u8; 4],
}

impl Header {
    /// A header that carries a file position.
    pub fn pos(kind: u8, pos: u32) -> Self {
        Self {
            kind,
            data: pos.to_le_bytes(),
        }
    }

    /// A header that carries flags, ZF0 first.
    pub fn flags(kind: u8, f0: u8, f1: u8, f2: u8, f3: u8) -> Self {
        Self {
            kind,
            data: [f3, f2, f1, f0],
        }
    }

    pub fn position(&self) -> u32 {
        u32::from_le_bytes(self.data)
    }

    pub fn f0(&self) -> u8 {
        let [.., f0] = self.data;
        f0
    }

    /// Whether data subpackets follow this header.
    fn has_data(&self) -> bool {
        matches!(self.kind, ZFILE | ZDATA | ZSINIT | ZCOMMAND)
    }

    fn bytes(&self) -> [u8; 5] {
        let [a, b, c, d] = self.data;
        [self.kind, a, b, c, d]
    }
}

/// CRC-16/XMODEM, as ZMODEM uses it.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in bytes {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// CRC-32 (IEEE), over `bytes` and then `more`.
pub fn crc32(bytes: &[u8], more: &[u8]) -> u32 {
    !crc32_update(crc32_update(0xFFFF_FFFF, bytes), more)
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_hex(out: &mut Vec<u8>, b: u8) {
    for nibble in [b >> 4, b & 15] {
        out.extend(HEX.get(usize::from(nibble)));
    }
}

/// Appends `h` as a hex header, the form for everything but data frames.
pub fn hex_header(h: &Header, out: &mut Vec<u8>) {
    out.extend_from_slice(&[ZPAD, ZPAD, ZDLE, ZHEX]);
    let body = h.bytes();
    for b in body {
        push_hex(out, b);
    }
    for b in crc16(&body).to_be_bytes() {
        push_hex(out, b);
    }
    // CR, LF with the high bit set, and XON to restart a flow-controlled
    // line; not after ZACK or ZFIN, as in the protocol description.
    out.extend_from_slice(&[b'\r', 0x8a]);
    if !matches!(h.kind, ZACK | ZFIN) {
        out.push(XON);
    }
}

/// ZDLE escaping for what the sender sends in binary.
#[derive(Debug, Clone, Default)]
pub struct Escape {
    /// Escape every control character (the receiver's ESCCTL).
    pub ctl: bool,
    last: u8,
}

impl Escape {
    pub fn byte(&mut self, out: &mut Vec<u8>, b: u8) {
        let escape = match b {
            ZDLE | 0x10 | XON | XOFF | 0x90 | 0x91 | 0x93 | 0x98 => true,
            // `@` CR is Telenet's escape, so CR after `@` goes escaped.
            0x0d | 0x8d => self.ctl || self.last & 0x7f == b'@',
            c => self.ctl && c & 0x60 == 0,
        };
        if escape {
            out.push(ZDLE);
            out.push(b ^ 0x40);
        } else {
            out.push(b);
        }
        self.last = b;
    }

    fn bytes(&mut self, out: &mut Vec<u8>, bytes: &[u8]) {
        for &b in bytes {
            self.byte(out, b);
        }
    }

    /// Appends `h` as a binary header, with a CRC-32 when `crc32`.
    pub fn bin_header(&mut self, h: &Header, crc32: bool, out: &mut Vec<u8>) {
        let body = h.bytes();
        out.extend_from_slice(&[ZPAD, ZDLE, if crc32 { ZBIN32 } else { ZBIN }]);
        self.bytes(out, &body);
        if crc32 {
            self.bytes(out, &self::crc32(&body, &[]).to_le_bytes());
        } else {
            self.bytes(out, &crc16(&body).to_be_bytes());
        }
    }

    /// Appends one data subpacket ending with `end` (ZCRCE and friends).
    pub fn subpacket(&mut self, data: &[u8], end: u8, crc32: bool, out: &mut Vec<u8>) {
        self.bytes(out, data);
        out.push(ZDLE);
        out.push(end);
        if crc32 {
            self.bytes(out, &self::crc32(data, &[end]).to_le_bytes());
        } else {
            let mut all = data.to_vec();
            all.push(end);
            self.bytes(out, &crc16(&all).to_be_bytes());
        }
    }
}

/// What the reader found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Header(Header),
    /// A data subpacket that passed its CRC, and how it ended.
    Data {
        bytes: Vec<u8>,
        end: u8,
    },
    /// A data subpacket that failed its CRC, was too long or was
    /// garbled. The rest of the frame is skipped.
    BadData,
    /// Five CANs: the other side gave up.
    Cancel,
}

#[derive(Debug, Clone)]
enum Lex {
    /// Between frames: anything but ZPAD is noise.
    Idle,
    Pad,
    PadDle,
    Hex(Vec<u8>),
    /// After a hex header: an optional CR and LF.
    HexTail {
        cr: bool,
        data: Option<bool>,
    },
    Bin {
        crc32: bool,
        buf: Vec<u8>,
        esc: bool,
    },
    Data {
        crc32: bool,
        buf: Vec<u8>,
        esc: bool,
        end: Option<u8>,
        crc: Vec<u8>,
    },
}

/// Splits the byte stream into [`Event`]s.
#[derive(Debug, Clone)]
pub struct Reader {
    lex: Lex,
    cans: u8,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            lex: Lex::Idle,
            cans: 0,
        }
    }
}

/// The byte a ZDLE escape stands for, if it is one.
fn unescape(c: u8) -> Option<u8> {
    match c {
        b'l' => Some(0x7f),
        b'm' => Some(0xff),
        c if c & 0x60 == 0x40 => Some(c ^ 0x40),
        _ => None,
    }
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn is_flow(b: u8) -> bool {
    matches!(b, XON | XOFF | 0x91 | 0x93)
}

fn data_state(crc32: bool) -> Lex {
    Lex::Data {
        crc32,
        buf: Vec::new(),
        esc: false,
        end: None,
        crc: Vec::new(),
    }
}

impl Reader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the next byte; returns what it completed, if anything.
    pub fn push(&mut self, b: u8) -> Option<Event> {
        if b == ZDLE {
            self.cans += 1;
            if self.cans >= CANCEL_RUN {
                self.cans = 0;
                self.lex = Lex::Idle;
                return Some(Event::Cancel);
            }
        } else {
            self.cans = 0;
        }
        let lex = std::mem::replace(&mut self.lex, Lex::Idle);
        let (next, event) = Self::step(lex, b);
        self.lex = next;
        event
    }

    fn step(lex: Lex, b: u8) -> (Lex, Option<Event>) {
        match lex {
            Lex::Idle => (if b == ZPAD { Lex::Pad } else { Lex::Idle }, None),
            Lex::Pad => match b {
                ZPAD => (Lex::Pad, None),
                ZDLE => (Lex::PadDle, None),
                _ => (Lex::Idle, None),
            },
            Lex::PadDle => match b {
                ZBIN => (Self::bin(false), None),
                ZBIN32 => (Self::bin(true), None),
                ZHEX => (Lex::Hex(Vec::new()), None),
                _ => (Lex::Idle, None),
            },
            Lex::Hex(mut digits) => {
                let Some(v) = hex_value(b) else {
                    return (Lex::Idle, None);
                };
                digits.push(v);
                if digits.len() < 14 {
                    return (Lex::Hex(digits), None);
                }
                let bytes: Vec<u8> = digits
                    .chunks_exact(2)
                    .map(|p| match p {
                        [hi, lo] => (hi << 4) | lo,
                        _ => 0,
                    })
                    .collect();
                match Self::header(&bytes, false) {
                    Some(h) => (
                        Lex::HexTail {
                            cr: false,
                            data: h.has_data().then_some(false),
                        },
                        Some(Event::Header(h)),
                    ),
                    None => (Lex::Idle, None),
                }
            }
            Lex::HexTail { cr, data } => {
                let next = || data.map_or(Lex::Idle, data_state);
                match b {
                    0x0d | 0x8d if !cr => (Lex::HexTail { cr: true, data }, None),
                    0x0a | 0x8a => (next(), None),
                    _ => {
                        // Not the tail after all: this byte starts what
                        // follows.
                        Self::step(next(), b)
                    }
                }
            }
            Lex::Bin {
                crc32,
                mut buf,
                esc,
            } => {
                let byte = if esc {
                    match unescape(b) {
                        Some(v) => v,
                        None => return (Lex::Idle, None),
                    }
                } else if b == ZDLE {
                    return (
                        Lex::Bin {
                            crc32,
                            buf,
                            esc: true,
                        },
                        None,
                    );
                } else if is_flow(b) {
                    return (
                        Lex::Bin {
                            crc32,
                            buf,
                            esc: false,
                        },
                        None,
                    );
                } else {
                    b
                };
                buf.push(byte);
                let want = if crc32 { 9 } else { 7 };
                if buf.len() < want {
                    return (
                        Lex::Bin {
                            crc32,
                            buf,
                            esc: false,
                        },
                        None,
                    );
                }
                match Self::header(&buf, crc32) {
                    Some(h) if h.has_data() => (data_state(crc32), Some(Event::Header(h))),
                    Some(h) => (Lex::Idle, Some(Event::Header(h))),
                    None => (Lex::Idle, None),
                }
            }
            Lex::Data {
                crc32,
                mut buf,
                esc,
                mut end,
                mut crc,
            } => {
                let byte = if esc {
                    if end.is_none() && (ZCRCE..=ZCRCW).contains(&b) {
                        end = Some(b);
                        None
                    } else {
                        match unescape(b) {
                            Some(v) => Some(v),
                            None => return (Lex::Idle, Some(Event::BadData)),
                        }
                    }
                } else if b == ZDLE {
                    return (
                        Lex::Data {
                            crc32,
                            buf,
                            esc: true,
                            end,
                            crc,
                        },
                        None,
                    );
                } else if is_flow(b) {
                    None
                } else {
                    Some(b)
                };
                if let Some(v) = byte {
                    if end.is_some() {
                        crc.push(v);
                    } else if buf.len() >= MAX_SUBPACKET {
                        return (Lex::Idle, Some(Event::BadData));
                    } else {
                        buf.push(v);
                    }
                }
                let Some(e) = end else {
                    return (
                        Lex::Data {
                            crc32,
                            buf,
                            esc: false,
                            end,
                            crc,
                        },
                        None,
                    );
                };
                let want = if crc32 { 4 } else { 2 };
                if crc.len() < want {
                    return (
                        Lex::Data {
                            crc32,
                            buf,
                            esc: false,
                            end,
                            crc,
                        },
                        None,
                    );
                }
                let good = if crc32 {
                    crc.as_slice() == crc32_of(&buf, e).as_slice()
                } else {
                    let mut all = buf.clone();
                    all.push(e);
                    crc.as_slice() == crc16(&all).to_be_bytes().as_slice()
                };
                if !good {
                    return (Lex::Idle, Some(Event::BadData));
                }
                let next = if matches!(e, ZCRCE | ZCRCW) {
                    Lex::Idle
                } else {
                    data_state(crc32)
                };
                (next, Some(Event::Data { bytes: buf, end: e }))
            }
        }
    }

    fn bin(crc32: bool) -> Lex {
        Lex::Bin {
            crc32,
            buf: Vec::new(),
            esc: false,
        }
    }

    /// A header from its five bytes and CRC, if the CRC matches.
    fn header(bytes: &[u8], crc32: bool) -> Option<Header> {
        let (body, sum) = bytes.split_first_chunk::<5>()?;
        let good = if crc32 {
            sum == self::crc32(body, &[]).to_le_bytes().as_slice()
        } else {
            sum == crc16(body).to_be_bytes().as_slice()
        };
        let [kind, a, b, c, d] = *body;
        good.then_some(Header {
            kind,
            data: [a, b, c, d],
        })
    }
}

fn crc32_of(data: &[u8], end: u8) -> [u8; 4] {
    crc32(data, &[end]).to_le_bytes()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn read_all(bytes: &[u8]) -> Vec<Event> {
        let mut r = Reader::new();
        bytes.iter().filter_map(|&b| r.push(b)).collect()
    }

    #[test]
    fn crcs_match_known_values() {
        assert_eq!(crc16(b"123456789"), 0x31c3);
        assert_eq!(crc32(b"123456789", &[]), 0xcbf4_3926);
        assert_eq!(crc32(b"1234", b"56789"), 0xcbf4_3926);
    }

    #[test]
    fn hex_headers_match_lrzsz() {
        // What `rz` sends first: ZRINIT with CANFDX, CANOVIO and CANFC32.
        let mut out = Vec::new();
        hex_header(
            &Header::flags(ZRINIT, CANFDX | CANOVIO | CANFC32, 0, 0, 0),
            &mut out,
        );
        assert_eq!(out, b"**\x18B0100000023be50\r\x8a\x11");
        // And `sz`: ZRQINIT.
        out.clear();
        hex_header(&Header::pos(ZRQINIT, 0), &mut out);
        assert_eq!(out, b"**\x18B00000000000000\r\x8a\x11");
        assert_eq!(read_all(&out), [Event::Header(Header::pos(ZRQINIT, 0))]);
    }

    #[test]
    fn binary_headers_and_subpackets_round_trip() {
        for crc32 in [false, true] {
            let mut esc = Escape::default();
            let mut out = b"noise*".to_vec();
            esc.bin_header(&Header::pos(ZDATA, 0x1811_1310), crc32, &mut out);
            let data: Vec<u8> = (0..=255).collect();
            esc.subpacket(&data, ZCRCG, crc32, &mut out);
            esc.subpacket(b"", ZCRCE, crc32, &mut out);
            esc.bin_header(&Header::pos(ZEOF, 256), crc32, &mut out);
            assert_eq!(
                read_all(&out),
                [
                    Event::Header(Header::pos(ZDATA, 0x1811_1310)),
                    Event::Data {
                        bytes: data.clone(),
                        end: ZCRCG
                    },
                    Event::Data {
                        bytes: Vec::new(),
                        end: ZCRCE
                    },
                    Event::Header(Header::pos(ZEOF, 256)),
                ]
            );
        }
    }

    #[test]
    fn escaped_output_has_no_flow_control_bytes() {
        let mut esc = Escape {
            ctl: true,
            ..Escape::default()
        };
        let mut out = Vec::new();
        let data: Vec<u8> = (0..=255).collect();
        esc.subpacket(&data, ZCRCW, true, &mut out);
        assert!(!out.iter().any(|&b| matches!(b, 0x10 | 0x11 | 0x13 | 0x0d)));
    }

    #[test]
    fn a_bad_crc_is_bad_data() {
        let mut esc = Escape::default();
        let mut out = Vec::new();
        esc.bin_header(&Header::pos(ZDATA, 0), true, &mut out);
        esc.subpacket(b"hello", ZCRCW, true, &mut out);
        let n = out.len();
        out[n - 1] ^= 1;
        assert_eq!(
            read_all(&out),
            [Event::Header(Header::pos(ZDATA, 0)), Event::BadData]
        );
    }

    #[test]
    fn five_cans_cancel() {
        assert_eq!(read_all(&[ZDLE; 5]), [Event::Cancel]);
        assert!(read_all(&CANCEL).contains(&Event::Cancel));
        assert_eq!(read_all(&[ZDLE; 4]), []);
    }

    #[test]
    fn long_subpackets_are_refused() {
        let mut esc = Escape::default();
        let mut out = Vec::new();
        esc.bin_header(&Header::pos(ZDATA, 0), false, &mut out);
        out.extend(std::iter::repeat_n(b'a', MAX_SUBPACKET + 1));
        assert_eq!(
            read_all(&out),
            [Event::Header(Header::pos(ZDATA, 0)), Event::BadData]
        );
    }
}
