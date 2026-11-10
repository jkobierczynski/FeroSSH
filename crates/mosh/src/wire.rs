//! What goes inside Mosh's encrypted packets.
//!
//! A packet's plaintext is two 16-bit timestamps (for the round-trip
//! estimate) and a fragment. Fragments carry pieces of one zlib-compressed
//! transport instruction, a small protobuf message: which state the diff
//! goes from and to, which of ours the sender has, and the diff itself,
//! another protobuf message (keystrokes and resizes from the client;
//! screen updates, resizes and echo acknowledgements from the server).
//!
//! Everything here reads input that passed the packet's integrity check,
//! so only the server, holding the session key, can send it; it is still
//! parsed as hostile: lengths are checked, sizes capped, nothing panics.

/// The protocol version both ends must speak (2 since echo acks).
pub const PROTOCOL_VERSION: u32 = 2;

/// Bytes a fragment's header takes: instruction id and number.
pub const FRAGMENT_HEADER: usize = 10;

/// The most an instruction may inflate to; Mosh's own buffer size.
pub const MAX_INSTRUCTION: usize = 2048 * 2048;

/// Bit 63 of the nonce: the packet goes to the client.
pub const TO_CLIENT: u64 = 1 << 63;

/// Why a packet's contents were refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Shorter than its fixed fields.
    Short,
    /// A packet meant for the server (a reflection of our own).
    WrongDirection,
    /// The compressed instruction did not inflate, or was too big.
    Inflate,
    /// The protobuf message is malformed.
    Protobuf,
    /// The other end speaks another protocol version.
    Version(u32),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Short => f.write_str("a truncated packet"),
            WireError::WrongDirection => f.write_str("a packet sent the wrong way"),
            WireError::Inflate => f.write_str("an instruction that would not decompress"),
            WireError::Protobuf => f.write_str("a malformed instruction"),
            WireError::Version(v) => write!(
                f,
                "the server speaks Mosh protocol {v}, not {PROTOCOL_VERSION}; \
                 update mosh-server or FeroSSH"
            ),
        }
    }
}

/// A decrypted packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// The sequence number, without the direction bit.
    pub seq: u64,
    pub timestamp: u16,
    pub timestamp_reply: u16,
    pub payload: Vec<u8>,
}

impl Packet {
    /// Reads a packet the server sent: `nonce` and the plaintext.
    pub fn from_server(nonce: u64, plaintext: &[u8]) -> Result<Self, WireError> {
        if nonce & TO_CLIENT == 0 {
            return Err(WireError::WrongDirection);
        }
        let (Some(ts), Some(reply), Some(payload)) = (
            plaintext.get(0..2),
            plaintext.get(2..4),
            plaintext.get(4..),
        ) else {
            return Err(WireError::Short);
        };
        Ok(Self {
            seq: nonce & !TO_CLIENT,
            timestamp: be16(ts),
            timestamp_reply: be16(reply),
            payload: payload.to_vec(),
        })
    }

    /// The nonce and plaintext of a packet to the server.
    pub fn to_server(&self) -> (u64, Vec<u8>) {
        let mut text = Vec::with_capacity(4 + self.payload.len());
        text.extend_from_slice(&self.timestamp.to_be_bytes());
        text.extend_from_slice(&self.timestamp_reply.to_be_bytes());
        text.extend_from_slice(&self.payload);
        (self.seq & !TO_CLIENT, text)
    }
}

fn be16(two: &[u8]) -> u16 {
    match two {
        [a, b] => u16::from_be_bytes([*a, *b]),
        _ => 0,
    }
}

/// A piece of an instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub id: u64,
    pub num: u16,
    pub last: bool,
    pub contents: Vec<u8>,
}

impl Fragment {
    pub fn parse(bytes: &[u8]) -> Result<Self, WireError> {
        let (Some(id), Some(num), Some(contents)) =
            (bytes.get(0..8), bytes.get(8..10), bytes.get(10..))
        else {
            return Err(WireError::Short);
        };
        let mut id8 = [0u8; 8];
        id8.copy_from_slice(id);
        let num = be16(num);
        Ok(Self {
            id: u64::from_be_bytes(id8),
            num: num & 0x7FFF,
            last: num & 0x8000 != 0,
            contents: contents.to_vec(),
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FRAGMENT_HEADER + self.contents.len());
        out.extend_from_slice(&self.id.to_be_bytes());
        out.extend_from_slice(&((u16::from(self.last) << 15) | (self.num & 0x7FFF)).to_be_bytes());
        out.extend_from_slice(&self.contents);
        out
    }
}

/// Puts an instruction's fragments back together. A fragment of another
/// instruction starts over, as in Mosh: only the newest one matters.
#[derive(Debug, Default)]
pub struct Assembly {
    id: Option<u64>,
    parts: Vec<Option<Vec<u8>>>,
    arrived: usize,
    total: Option<usize>,
    bytes: usize,
}

impl Assembly {
    /// Adds `frag`; the whole compressed instruction once all arrived.
    pub fn add(&mut self, frag: Fragment) -> Option<Vec<u8>> {
        let num = usize::from(frag.num);
        if self.id != Some(frag.id) {
            *self = Self {
                id: Some(frag.id),
                ..Self::default()
            };
        }
        if let Some(total) = self.total
            && num >= total
        {
            // Past the end this instruction already announced.
            *self = Self::default();
            return None;
        }
        if frag.last {
            if self.parts.len() > num + 1 {
                // A part beyond the one that says it is last.
                *self = Self::default();
                return None;
            }
            self.total = Some(num + 1);
        }
        if self.parts.len() <= num {
            self.parts.resize(num + 1, None);
        }
        let slot = self.parts.get_mut(num)?;
        match slot {
            Some(have) if *have != frag.contents => {
                // The same piece twice, different: drop the instruction.
                *self = Self::default();
                return None;
            }
            Some(_) => {}
            None => {
                self.bytes += frag.contents.len();
                if self.bytes > MAX_INSTRUCTION {
                    *self = Self::default();
                    return None;
                }
                *slot = Some(frag.contents);
                self.arrived += 1;
            }
        }
        if Some(self.arrived) != self.total {
            return None;
        }
        let whole: Vec<u8> = self.parts.iter().flatten().flatten().copied().collect();
        self.parts.clear();
        self.arrived = 0;
        self.total = None;
        self.bytes = 0;
        Some(whole)
    }
}

/// Inflates a compressed instruction (zlib, as Mosh's `compress()`).
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, WireError> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes, MAX_INSTRUCTION)
        .map_err(|_| WireError::Inflate)
}

/// Compresses an instruction for sending.
pub fn deflate(bytes: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(bytes, 6)
}

/// A transport instruction (`TransportBuffers.Instruction`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Instruction {
    pub protocol_version: u32,
    pub old_num: u64,
    pub new_num: u64,
    pub ack_num: u64,
    pub throwaway_num: u64,
    pub diff: Vec<u8>,
    pub chaff: Vec<u8>,
}

impl Instruction {
    pub fn parse(bytes: &[u8]) -> Result<Self, WireError> {
        let mut inst = Self::default();
        let mut r = Reader(bytes);
        while let Some((field, value)) = r.next()? {
            match (field, value) {
                (1, Value::Varint(v)) => inst.protocol_version = v as u32,
                (2, Value::Varint(v)) => inst.old_num = v,
                (3, Value::Varint(v)) => inst.new_num = v,
                (4, Value::Varint(v)) => inst.ack_num = v,
                (5, Value::Varint(v)) => inst.throwaway_num = v,
                (6, Value::Bytes(b)) => inst.diff = b.to_vec(),
                (7, Value::Bytes(b)) => inst.chaff = b.to_vec(),
                _ => {}
            }
        }
        Ok(inst)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.diff.len() + self.chaff.len() + 48);
        put_varint_field(&mut out, 1, self.protocol_version.into());
        put_varint_field(&mut out, 2, self.old_num);
        put_varint_field(&mut out, 3, self.new_num);
        put_varint_field(&mut out, 4, self.ack_num);
        put_varint_field(&mut out, 5, self.throwaway_num);
        put_bytes_field(&mut out, 6, &self.diff);
        put_bytes_field(&mut out, 7, &self.chaff);
        out
    }
}

/// One part of the server's diff (`HostBuffers.HostMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// Terminal output that turns the old state's screen into the new.
    Bytes(Vec<u8>),
    /// The server's terminal is this size now.
    Resize { cols: i32, rows: i32 },
    /// The server has echoed the keystrokes of our states up to this one.
    EchoAck(u64),
}

/// Reads the server's diff.
pub fn parse_host_message(bytes: &[u8]) -> Result<Vec<HostEvent>, WireError> {
    let mut out = Vec::new();
    let mut r = Reader(bytes);
    while let Some((field, value)) = r.next()? {
        let (1, Value::Bytes(inst)) = (field, value) else {
            continue;
        };
        let mut r = Reader(inst);
        while let Some((ext, value)) = r.next()? {
            let Value::Bytes(body) = value else {
                continue;
            };
            let mut body = Reader(body);
            match ext {
                2 => {
                    let mut bytes = Vec::new();
                    while let Some((f, v)) = body.next()? {
                        if let (4, Value::Bytes(b)) = (f, v) {
                            bytes = b.to_vec();
                        }
                    }
                    out.push(HostEvent::Bytes(bytes));
                }
                3 => {
                    let (mut cols, mut rows) = (0, 0);
                    while let Some((f, v)) = body.next()? {
                        match (f, v) {
                            (5, Value::Varint(v)) => cols = v as i32,
                            (6, Value::Varint(v)) => rows = v as i32,
                            _ => {}
                        }
                    }
                    out.push(HostEvent::Resize { cols, rows });
                }
                7 => {
                    let mut ack = 0;
                    while let Some((f, v)) = body.next()? {
                        if let (8, Value::Varint(v)) = (f, v) {
                            ack = v;
                        }
                    }
                    out.push(HostEvent::EchoAck(ack));
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

/// One part of our diff (`ClientBuffers.UserMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    Keys(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// Writes our diff: keystrokes run together, as Mosh's client does.
pub fn user_message(events: &[UserEvent]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut keys: Vec<u8> = Vec::new();
    let flush = |out: &mut Vec<u8>, keys: &mut Vec<u8>| {
        if keys.is_empty() {
            return;
        }
        let mut stroke = Vec::new();
        put_bytes_field(&mut stroke, 4, keys);
        let mut inst = Vec::new();
        put_bytes_field(&mut inst, 2, &stroke);
        put_bytes_field(out, 1, &inst);
        keys.clear();
    };
    for e in events {
        match e {
            UserEvent::Keys(k) => keys.extend_from_slice(k),
            UserEvent::Resize { cols, rows } => {
                flush(&mut out, &mut keys);
                let mut size = Vec::new();
                put_varint_field(&mut size, 5, (*cols).into());
                put_varint_field(&mut size, 6, (*rows).into());
                let mut inst = Vec::new();
                put_bytes_field(&mut inst, 3, &size);
                put_bytes_field(&mut out, 1, &inst);
            }
        }
    }
    flush(&mut out, &mut keys);
    out
}

/// A protobuf field's value.
enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64, WireError> {
        let mut v: u64 = 0;
        for i in 0..10 {
            let (&b, rest) = self.0.split_first().ok_or(WireError::Protobuf)?;
            self.0 = rest;
            v |= u64::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(WireError::Protobuf)
    }

    fn take(&mut self, n: u64) -> Result<&'a [u8], WireError> {
        let n = usize::try_from(n).map_err(|_| WireError::Protobuf)?;
        if n > self.0.len() {
            return Err(WireError::Protobuf);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    /// The next field number and value, or `None` at the end.
    fn next(&mut self) -> Result<Option<(u64, Value<'a>)>, WireError> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = key >> 3;
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Value::Fixed
            }
            2 => {
                let n = self.varint()?;
                Value::Bytes(self.take(n)?)
            }
            5 => {
                self.take(4)?;
                Value::Fixed
            }
            _ => return Err(WireError::Protobuf),
        };
        if field == 0 {
            return Err(WireError::Protobuf);
        }
        Ok(Some((field, value)))
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_varint_field(out: &mut Vec<u8>, field: u64, v: u64) {
    put_varint(out, field << 3);
    put_varint(out, v);
}

fn put_bytes_field(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn instructions_round_trip() {
        let inst = Instruction {
            protocol_version: 2,
            old_num: 3,
            new_num: u64::MAX,
            ack_num: 300,
            throwaway_num: 1,
            diff: b"diff".to_vec(),
            chaff: vec![1, 2, 3],
        };
        let bytes = inst.to_bytes();
        assert_eq!(Instruction::parse(&bytes).unwrap(), inst);
        let packed = deflate(&bytes);
        assert_eq!(inflate(&packed).unwrap(), bytes);
    }

    /// The bytes protobuf writes for a HostMessage with an echo ack, a
    /// resize to 80x24 and the host string "hi".
    #[test]
    fn reads_host_messages() {
        let msg = [
            0x0A, 0x04, 0x3A, 0x02, 0x40, 0x07, // echoack 7
            0x0A, 0x06, 0x1A, 0x04, 0x28, 0x50, 0x30, 0x18, // resize 80x24
            0x0A, 0x06, 0x12, 0x04, 0x22, 0x02, b'h', b'i', // hostbytes
        ];
        assert_eq!(
            parse_host_message(&msg).unwrap(),
            vec![
                HostEvent::EchoAck(7),
                HostEvent::Resize { cols: 80, rows: 24 },
                HostEvent::Bytes(b"hi".to_vec()),
            ]
        );
        assert_eq!(parse_host_message(&msg[..5]), Err(WireError::Protobuf));
        assert_eq!(parse_host_message(&[0x0A, 0xFF]), Err(WireError::Protobuf));
    }

    #[test]
    fn writes_user_messages_as_mosh_does() {
        let msg = user_message(&[
            UserEvent::Resize { cols: 80, rows: 24 },
            UserEvent::Keys(b"l".to_vec()),
            UserEvent::Keys(b"s".to_vec()),
        ]);
        assert_eq!(
            msg,
            [
                0x0A, 0x06, 0x1A, 0x04, 0x28, 0x50, 0x30, 0x18, // resize
                0x0A, 0x06, 0x12, 0x04, 0x22, 0x02, b'l', b's', // keystrokes
            ]
        );
    }

    #[test]
    fn fragments_assemble_in_any_order() {
        let frags: Vec<Fragment> = (0..3)
            .map(|i| Fragment {
                id: 9,
                num: i,
                last: i == 2,
                contents: vec![i as u8; 3],
            })
            .collect();
        for f in &frags {
            assert_eq!(Fragment::parse(&f.to_bytes()).unwrap(), *f);
        }
        let mut a = Assembly::default();
        assert_eq!(a.add(frags[2].clone()), None);
        assert_eq!(a.add(frags[0].clone()), None);
        assert_eq!(a.add(frags[0].clone()), None);
        assert_eq!(a.add(frags[1].clone()), Some(vec![0, 0, 0, 1, 1, 1, 2, 2, 2]));
        // A newer instruction replaces a half-built one.
        assert_eq!(a.add(frags[0].clone()), None);
        let lone = Fragment {
            id: 10,
            num: 0,
            last: true,
            contents: vec![5],
        };
        assert_eq!(a.add(lone), Some(vec![5]));
    }

    #[test]
    fn packets_keep_their_direction() {
        let p = Packet {
            seq: 5,
            timestamp: 1,
            timestamp_reply: 0xFFFF,
            payload: b"x".to_vec(),
        };
        let (nonce, text) = p.to_server();
        assert_eq!(nonce, 5);
        assert_eq!(
            Packet::from_server(nonce, &text),
            Err(WireError::WrongDirection)
        );
        assert_eq!(Packet::from_server(nonce | TO_CLIENT, &text).unwrap(), p);
        assert_eq!(Packet::from_server(TO_CLIENT, &[1, 2, 3]), Err(WireError::Short));
    }
}
