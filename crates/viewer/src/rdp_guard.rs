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
    /// The start of the DRDYNVC message being reassembled, while it is
    /// still too short to tell whether it is a DataFirst and what length
    /// it announces: a server may split even that header over chunks.
    dyn_head: Option<Vec<u8>>,
}

impl ChannelGuard {
    pub(crate) fn new(io_channel: u16, drdynvc: Option<u16>) -> Self {
        Self {
            io_channel,
            drdynvc,
            open: HashSet::new(),
            dyn_head: None,
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
            self.dyn_head = None;
            return Err(
                "The RDP server sent compressed channel data, which the viewer did not ask for."
                    .into(),
            );
        }
        if length > MAX_STATIC_MESSAGE {
            self.open.remove(&channel);
            self.dyn_head = None;
            return Err(format!(
                "The RDP server announced a {length}-byte channel message; the viewer takes at \
                 most {MAX_STATIC_MESSAGE}."
            ));
        }
        let first = flags & FLAG_FIRST != 0;
        let last = flags & FLAG_LAST != 0;
        // A chunk with neither flag outside a chunked message is, as
        // IronRDP takes it, a whole message.
        let whole = !first && !last && !self.open.contains(&channel);
        let starts = first || whole;
        let ends = last || whole;
        if first && !last {
            self.open.insert(channel);
        } else if last {
            self.open.remove(&channel);
        }
        if Some(channel) == self.drdynvc {
            let body = data.get(8..).unwrap_or_default();
            if starts {
                self.dyn_head = Some(Vec::new());
            }
            if let Some(head) = &mut self.dyn_head {
                let room = MAX_DYNAMIC_HEADER.saturating_sub(head.len());
                head.extend_from_slice(body.get(..room.min(body.len())).unwrap_or_default());
                match check_dynamic(head) {
                    Header::Partial if !ends => {}
                    // A message that ends inside its header is IronRDP's
                    // to reject.
                    Header::Partial | Header::Fine => self.dyn_head = None,
                    Header::TooLong(why) => {
                        self.dyn_head = None;
                        return Err(why);
                    }
                }
            }
        }
        Ok(())
    }
}

/// The most bytes a DRDYNVC header takes: the command byte, then a
/// ChannelId and a Length of up to four bytes each.
const MAX_DYNAMIC_HEADER: usize = 9;

/// What the start of a DRDYNVC message says so far.
#[derive(Debug, PartialEq, Eq)]
enum Header {
    /// Too short to tell yet.
    Partial,
    /// Not a DataFirst, or one within the cap.
    Fine,
    /// A DataFirst announcing more than the cap.
    TooLong(String),
}

/// Checks the length a DRDYNVC DataFirst PDU announces, from as much of
/// its start as has arrived.
fn check_dynamic(pdu: &[u8]) -> Header {
    let Some(&head) = pdu.first() else {
        return Header::Partial;
    };
    let cmd = head >> 4;
    if cmd != DATA_FIRST && cmd != DATA_FIRST_COMPRESSED {
        return Header::Fine;
    }
    let id_len = field_len(head & 0b11);
    let len_len = field_len((head >> 2) & 0b11);
    let Some(length) = pdu
        .get(1 + id_len..1 + id_len + len_len)
        .map(|b| b.iter().rev().fold(0u32, |n, &x| n << 8 | u32::from(x)))
    else {
        return Header::Partial;
    };
    if length > MAX_DYNAMIC_MESSAGE {
        return Header::TooLong(format!(
            "The RDP server announced a {length}-byte dynamic channel message; the viewer takes \
             at most {MAX_DYNAMIC_MESSAGE}."
        ));
    }
    Header::Fine
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

    #[test]
    fn dynamic_headers_split_over_chunks_are_checked() {
        // DataFirst, 4-byte id, 4-byte length: a 9-byte header.
        let mut df = vec![0x2a, 1, 0, 0, 0];
        df.extend((MAX_DYNAMIC_MESSAGE + 1).to_le_bytes());
        df.extend([0; 7]);
        let total = u32::try_from(df.len()).unwrap();
        // Split the message after every byte of the header.
        for cut in 0..=MAX_DYNAMIC_HEADER {
            let mut g = guard();
            let first = g.check_chunk(DRDYNVC, &chunk(total, FLAG_FIRST, &df[..cut]));
            let rest =
                first.and_then(|()| g.check_chunk(DRDYNVC, &chunk(total, FLAG_LAST, &df[cut..])));
            assert!(rest.is_err(), "cut at {cut}");
        }
        // One byte a chunk, with flagless chunks in the middle.
        let mut g = guard();
        let mut res = Ok(());
        for (i, b) in df.iter().enumerate() {
            let flags = match i {
                0 => FLAG_FIRST,
                i if i == df.len() - 1 => FLAG_LAST,
                _ => 0,
            };
            res = g.check_chunk(DRDYNVC, &chunk(total, flags, &[*b]));
            if res.is_err() {
                break;
            }
        }
        assert!(res.is_err());
        // A small DataFirst split the same way passes, and so does the
        // next message.
        let mut ok = vec![0x2a, 1, 0, 0, 0];
        ok.extend(16u32.to_le_bytes());
        let mut g = guard();
        g.check_chunk(DRDYNVC, &chunk(9, FLAG_FIRST, &ok[..3]))
            .unwrap();
        g.check_chunk(DRDYNVC, &chunk(9, FLAG_LAST, &ok[3..]))
            .unwrap();
        g.check_chunk(DRDYNVC, &chunk(3, FLAG_FIRST | FLAG_LAST, &[0x30, 7, 9]))
            .unwrap();
        // A header cut short by the end of its message is left to IronRDP,
        // and does not carry over into the next message.
        let mut g = guard();
        g.check_chunk(DRDYNVC, &chunk(2, FLAG_FIRST | FLAG_LAST, &[0x2a, 1]))
            .unwrap();
        g.check_chunk(DRDYNVC, &chunk(3, 0, &[0x30, 7, 9])).unwrap();
        g.check_chunk(DRDYNVC, &chunk(3, 0, &[0, 0, 0])).unwrap();
    }

    /// The chunks of a split DataFirst, as IronRDP itself reassembles
    /// them: the guard must stop what IronRDP would accept.
    #[test]
    fn split_headers_ironrdp_would_reassemble_are_stopped() {
        use ironrdp_pdu::gcc::ChannelName;
        use ironrdp_svc::{StaticVirtualChannel, SvcMessage, SvcProcessor};

        /// Keeps the message IronRDP hands to the channel.
        #[derive(Debug, Default)]
        struct Seen(Option<Vec<u8>>);
        ironrdp_svc::impl_as_any!(Seen);
        impl SvcProcessor for Seen {
            fn channel_name(&self) -> ChannelName {
                ChannelName::from_static(b"drdynvc\0")
            }
            fn process(&mut self, payload: &[u8]) -> ironrdp_pdu::PduResult<Vec<SvcMessage>> {
                self.0 = Some(payload.to_vec());
                Ok(Vec::new())
            }
        }

        let mut df = vec![0x2a, 1, 0, 0, 0];
        df.extend(u32::MAX.to_le_bytes());
        df.extend([0; 3]);
        let total = u32::try_from(df.len()).unwrap();
        let pieces = [
            chunk(total, FLAG_FIRST, &df[..1]),
            chunk(total, 0, &df[1..6]),
            chunk(total, FLAG_LAST, &df[6..]),
        ];
        let mut ironrdp = StaticVirtualChannel::new(Seen::default());
        for p in &pieces {
            ironrdp.process(p).unwrap();
        }
        let seen = ironrdp.channel_processor_downcast_ref::<Seen>().unwrap();
        assert_eq!(
            seen.0.as_deref(),
            Some(&df[..]),
            "IronRDP takes the split header"
        );
        let mut g = guard();
        assert!(
            pieces
                .iter()
                .try_for_each(|p| g.check_chunk(DRDYNVC, p))
                .is_err()
        );
    }
}
