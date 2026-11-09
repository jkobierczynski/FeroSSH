//! Limits on what an RDP server may make the viewer hold for its virtual
//! channels, checked on each PDU before IronRDP sees it.
//!
//! IronRDP reassembles a static channel message from chunks up to the
//! length the server declares, and a dynamic channel message (DRDYNVC
//! DataFirst and Data PDUs) up to the length the server declares there:
//! up to 4 GiB either way, held until the last piece arrives. And when a
//! server marks chunks compressed, IronRDP checks no length at all
//! (IronRDP issues #2081 and #2092, open as of November 2026). The viewer
//! never asks for compression, so compressed chunks end the session, and
//! a declared length over the caps below ends it too. The viewer's job
//! object (1 GiB) stays the last line.

use std::collections::HashSet;

/// The longest static channel message: the clipboard's 1 MiB of text as
/// UTF-16, with room to spare.
pub(crate) const MAX_STATIC_MESSAGE: u32 = 8 * 1024 * 1024;
/// The longest dynamic channel message. The only dynamic channel the
/// viewer opens is Display Control, whose messages are a few hundred
/// bytes.
pub(crate) const MAX_DYNAMIC_MESSAGE: u32 = 1024 * 1024;

/// CHANNEL_PDU_HEADER flags (MS-RDPBCGR 2.2.6.1.1).
const FLAG_FIRST: u32 = 0x0000_0001;
const FLAG_LAST: u32 = 0x0000_0002;
const PACKET_COMPRESSED: u32 = 0x0020_0000;

/// DRDYNVC commands that start a fragmented message (MS-RDPEDYC 2.2.3).
const DATA_FIRST: u8 = 0x02;
const DATA_FIRST_COMPRESSED: u8 = 0x06;

/// Checks the virtual channel PDUs an RDP server sends.
#[derive(Debug)]
pub(crate) struct ChannelGuard {
    /// The I/O channel, which carries no virtual channel data.
    io_channel: u16,
    /// The DRDYNVC static channel, which carries the dynamic channels.
    drdynvc: Option<u16>,
    /// Static channels in the middle of a chunked message.
    open: HashSet<u16>,
}

impl ChannelGuard {
    pub(crate) fn new(io_channel: u16, drdynvc: Option<u16>) -> Self {
        Self {
            io_channel,
            drdynvc,
            open: HashSet::new(),
        }
    }

    /// Checks one slow-path PDU from the server (an X.224 frame); `Err`
    /// says why the session must end. What does not decode as channel
    /// data is left to IronRDP.
    pub(crate) fn check(&mut self, frame: &[u8]) -> Result<(), String> {
        let Ok(ctx) = ironrdp_pdu::mcs::decode_send_data_indication(frame) else {
            return Ok(());
        };
        if ctx.channel_id == self.io_channel {
            return Ok(());
        }
        self.check_chunk(ctx.channel_id, ctx.user_data)
    }

    /// Checks one static channel chunk: its CHANNEL_PDU_HEADER, then the
    /// message itself.
    fn check_chunk(&mut self, channel: u16, data: &[u8]) -> Result<(), String> {
        let (Some(length), Some(flags)) = (u32_at(data, 0), u32_at(data, 4)) else {
            // IronRDP rejects a short header itself.
            return Ok(());
        };
        if flags & PACKET_COMPRESSED != 0 {
            self.open.remove(&channel);
            return Err(
                "The RDP server sent compressed channel data, which the viewer did not ask for."
                    .into(),
            );
        }
        if length > MAX_STATIC_MESSAGE {
            self.open.remove(&channel);
            return Err(format!(
                "The RDP server announced a {length}-byte channel message; the viewer takes at \
                 most {MAX_STATIC_MESSAGE}."
            ));
        }
        let first = flags & FLAG_FIRST != 0;
        let last = flags & FLAG_LAST != 0;
        // A message starts here: with FLAG_FIRST, or (as IronRDP takes
        // it) a chunk with neither flag outside a chunked message.
        let starts = first || (!last && !self.open.contains(&channel));
        if first && !last {
            self.open.insert(channel);
        } else if last {
            self.open.remove(&channel);
        }
        if starts && Some(channel) == self.drdynvc {
            check_dynamic(data.get(8..).unwrap_or_default())?;
        }
        Ok(())
    }
}

/// Checks the length a DRDYNVC DataFirst PDU announces.
fn check_dynamic(pdu: &[u8]) -> Result<(), String> {
    let Some(&head) = pdu.first() else {
        return Ok(());
    };
    let cmd = head >> 4;
    if cmd != DATA_FIRST && cmd != DATA_FIRST_COMPRESSED {
        return Ok(());
    }
    let id_len = field_len(head & 0b11);
    let len_len = field_len((head >> 2) & 0b11);
    let Some(length) = pdu
        .get(1 + id_len..1 + id_len + len_len)
        .map(|b| b.iter().rev().fold(0u32, |n, &x| n << 8 | u32::from(x)))
    else {
        // Too short: IronRDP rejects it.
        return Ok(());
    };
    if length > MAX_DYNAMIC_MESSAGE {
        return Err(format!(
            "The RDP server announced a {length}-byte dynamic channel message; the viewer takes \
             at most {MAX_DYNAMIC_MESSAGE}."
        ));
    }
    Ok(())
}

/// The size of a DRDYNVC ChannelId or Length field from its two bits.
fn field_len(bits: u8) -> usize {
    match bits {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    const DRDYNVC: u16 = 1005;
    const CLIP: u16 = 1004;

    fn chunk(length: u32, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut v = length.to_le_bytes().to_vec();
        v.extend(flags.to_le_bytes());
        v.extend(body);
        v
    }

    fn guard() -> ChannelGuard {
        ChannelGuard::new(1003, Some(DRDYNVC))
    }

    #[test]
    fn ordinary_chunks_pass() {
        let mut g = guard();
        let both = FLAG_FIRST | FLAG_LAST;
        g.check_chunk(CLIP, &chunk(4, both, b"abcd")).unwrap();
        g.check_chunk(CLIP, &chunk(3000, FLAG_FIRST, &[0; 1600]))
            .unwrap();
        g.check_chunk(CLIP, &chunk(3000, FLAG_LAST, &[0; 1400]))
            .unwrap();
        g.check_chunk(CLIP, &chunk(MAX_STATIC_MESSAGE, both, &[]))
            .unwrap();
        // A small DataFirst on DRDYNVC: cmd 2, 1-byte id, 2-byte length.
        let df = [0x24, 7, 0x00, 0x10, 1, 2, 3];
        g.check_chunk(DRDYNVC, &chunk(7, both, &df)).unwrap();
        // A Data PDU (cmd 3) announces no length.
        g.check_chunk(DRDYNVC, &chunk(3, both, &[0x30, 7, 9]))
            .unwrap();
    }

    #[test]
    fn compressed_chunks_end_the_session() {
        let mut g = guard();
        let err = g
            .check_chunk(CLIP, &chunk(10, FLAG_FIRST | PACKET_COMPRESSED, &[1]))
            .unwrap_err();
        assert!(err.contains("compressed"), "{err}");
    }

    #[test]
    fn huge_static_messages_end_the_session() {
        let mut g = guard();
        assert!(
            g.check_chunk(CLIP, &chunk(MAX_STATIC_MESSAGE + 1, FLAG_FIRST, &[0; 16]))
                .is_err()
        );
        assert!(g.check_chunk(CLIP, &chunk(u32::MAX, 0, &[])).is_err());
    }

    #[test]
    fn huge_dynamic_messages_end_the_session() {
        let both = FLAG_FIRST | FLAG_LAST;
        // DataFirst, 4-byte id (bits 2), 4-byte length (bits 2 << 2).
        let mut df = vec![0x2a, 1, 0, 0, 0];
        df.extend((MAX_DYNAMIC_MESSAGE + 1).to_le_bytes());
        let mut g = guard();
        assert!(g.check_chunk(DRDYNVC, &chunk(20, both, &df)).is_err());
        // The same on another channel is not a DRDYNVC PDU.
        g.check_chunk(CLIP, &chunk(20, both, &df)).unwrap();
        // DataFirstCompressed too.
        df[0] = 0x6a;
        assert!(g.check_chunk(DRDYNVC, &chunk(20, both, &df)).is_err());
        // In the middle of a chunked message, the bytes are not a header.
        let mut g = guard();
        g.check_chunk(DRDYNVC, &chunk(5000, FLAG_FIRST, &[0x30, 1]))
            .unwrap();
        g.check_chunk(DRDYNVC, &chunk(5000, 0, &df)).unwrap();
        g.check_chunk(DRDYNVC, &chunk(5000, FLAG_LAST, &[0; 4]))
            .unwrap();
        // After it, a flagless chunk is a whole message again.
        assert!(g.check_chunk(DRDYNVC, &chunk(20, 0, &df)).is_err());
    }

    #[test]
    fn other_traffic_is_left_to_ironrdp() {
        let mut g = guard();
        g.check(&[]).unwrap();
        g.check(&[3, 0, 0, 4]).unwrap();
        g.check_chunk(CLIP, &[1, 2]).unwrap();
        g.check_chunk(DRDYNVC, &chunk(1, FLAG_FIRST | FLAG_LAST, &[]))
            .unwrap();
    }
}
