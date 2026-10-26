//! Spotting the start of a transfer in a terminal's output.
//!
//! `sz` on the server starts with a hex ZRQINIT (`**`, ZDLE, `B00`), and
//! `rz` with a hex ZRINIT (`**`, ZDLE, `B01`). The detector passes every
//! other byte straight through. It only ever holds back a ZDLE and the up
//! to two bytes after it that could still turn into a start, so a prompt
//! that ends in `*` shows at once.

use crate::frame::{ZDLE, ZPAD};

/// What the server wants to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// `sz`: the server sends files to this side.
    Receive,
    /// `rz`: the server waits for files from this side.
    Send,
}

/// The bytes of a start after its ZPADs, but for the last.
const AFTER_PAD: [u8; 3] = [ZDLE, b'B', b'0'];

/// What [`Detector::scan`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub start: Start,
    /// The stream from the start's header on, for the receiver or sender.
    pub rest: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct Detector {
    /// A ZDLE and what followed it, at the end of the last chunk.
    held: Vec<u8>,
    /// The byte before `held` (or the last byte shown) was a ZPAD.
    star: bool,
}

impl Detector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks for a start in the next chunk of output. Returns the bytes to
    /// show, and the start with everything after it once found; the
    /// detector is then fresh for after the transfer.
    pub fn scan(&mut self, chunk: &[u8]) -> (Vec<u8>, Option<Found>) {
        let mut all = std::mem::take(&mut self.held);
        all.extend_from_slice(chunk);
        let star_before = self.star;
        let mut hold = None;
        for (i, &b) in all.iter().enumerate() {
            if b != ZDLE {
                continue;
            }
            let preceded = match i.checked_sub(1) {
                Some(j) => all.get(j) == Some(&ZPAD),
                None => star_before,
            };
            if !preceded {
                continue;
            }
            let tail = all.get(i..).unwrap_or_default();
            match tail {
                [_, b'B', b'0', kind @ (b'0' | b'1'), ..] => {
                    let start = if *kind == b'0' {
                        Start::Receive
                    } else {
                        Start::Send
                    };
                    let mut rest = vec![ZPAD, ZPAD];
                    rest.extend_from_slice(tail);
                    let mut shown = all.get(..i).unwrap_or_default().to_vec();
                    // The start's own ZPADs are not output.
                    for _ in 0..2 {
                        if shown.last() == Some(&ZPAD) {
                            shown.pop();
                        }
                    }
                    *self = Self::default();
                    return (shown, Some(Found { start, rest }));
                }
                _ if tail.len() < 4 && AFTER_PAD.starts_with(tail) => {
                    hold = Some(i);
                    break;
                }
                _ => {}
            }
        }
        if let Some(i) = hold {
            self.held = all.split_off(i);
            self.star = true;
            return (all, None);
        }
        self.star = all.last() == Some(&ZPAD);
        (all, None)
    }

    /// Whether bytes wait for the next chunk.
    pub fn holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// Gives back what is held, when output should show without waiting.
    pub fn flush(&mut self) -> Vec<u8> {
        self.star = false;
        std::mem::take(&mut self.held)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn finds_sz_and_rz() {
        let mut d = Detector::new();
        let (shown, found) = d.scan(b"rz\r**\x18B00000000000000\r\x8a\x11");
        assert_eq!(shown, b"rz\r");
        let found = found.unwrap();
        assert_eq!(found.start, Start::Receive);
        assert_eq!(found.rest, b"**\x18B00000000000000\r\x8a\x11");

        let (shown, found) = d.scan(b"rz waiting to receive.**\x18B0100000023be50\r\x8a\x11");
        assert_eq!(shown, b"rz waiting to receive.");
        assert_eq!(found.unwrap().start, Start::Send);
    }

    #[test]
    fn finds_a_start_split_anywhere() {
        let stream = b"$ sz f\r\nrz\r**\x18B00000000000000\r\x8a\x11";
        for cut in 0..stream.len() {
            let (a, b) = stream.split_at(cut);
            let mut d = Detector::new();
            let (mut shown, mut found) = d.scan(a);
            if found.is_none() {
                let (more, f) = d.scan(b);
                shown.extend(more);
                found = f;
            }
            let found = found.unwrap();
            assert_eq!(found.start, Start::Receive, "cut {cut}");
            // A ZPAD already shown before the cut stays shown.
            assert!(shown.starts_with(b"$ sz f\r\nrz\r"), "cut {cut}");
            assert!(found.rest.starts_with(b"**\x18B00"), "cut {cut}");
        }
    }

    #[test]
    fn ordinary_output_passes_through() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"prompt **").0, b"prompt **");
        assert_eq!(d.scan(b"\x18B2").0, b"\x18B2");
        let (shown, found) = d.scan(b"a*\x18");
        assert_eq!(shown, b"a*");
        assert!(found.is_none());
        assert_eq!(d.scan(b"x").0, b"\x18x");
        assert_eq!(d.scan(b"*\x18B").0, b"*");
        assert_eq!(d.flush(), b"\x18B");
    }
}
