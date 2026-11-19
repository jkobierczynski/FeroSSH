//! Inline images: finds Sixel, kitty graphics and iTerm2 image sequences
//! in the host's output before the emulator sees it, and keeps the
//! decoded pictures.
//!
//! alacritty_terminal ignores all three, so [`Scanner`] takes them out of
//! the stream and hands everything else on unchanged. A picture is placed
//! by marking the cells it covers (see [`Marker`]), so it scrolls, is
//! overwritten and goes into the scrollback the way text does.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ferossh_images::Limits;
use ferossh_render::Picture;

/// Pixel bytes one terminal keeps; the oldest pictures go first.
const STORE_BYTES: usize = 64 << 20;
/// Pictures one terminal keeps.
const STORE_PICTURES: usize = 512;
/// Bytes of an escape sequence's start held back while it is unclear
/// whether it is an image.
const MAX_HELD: usize = 64;
/// Encoded bytes taken for one picture: the limit plus room for the
/// sequence's own keys.
const MAX_PAYLOAD: usize = Limits::DEFAULT.max_encoded + 4096;
/// Bytes of a kitty command's keys kept to answer one that was too long.
const MAX_KEYS: usize = 256;

/// Picture keys, unique across every terminal in the process so the
/// renderer can keep each on the GPU under its key.
static NEXT_KEY: AtomicU32 = AtomicU32::new(1);

/// The OSC 8 id that marks a cell as part of a picture. A host cannot
/// send it: BEL ends an OSC.
pub(crate) const MARKER_ID: &str = "\x07ferossh-picture";

/// Which row of a picture a screen line shows: carried in the line's
/// picture cells as an OSC 8 link with [`MARKER_ID`] and the uri
/// `key:row:rows:cols:left`. All the cells of one row share the link,
/// so a picture costs one allocation per row, as a link printed by the
/// host does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Marker {
    pub key: u32,
    pub row: u16,
    pub rows: u16,
    pub cols: u16,
    /// The column of the picture's left edge.
    pub left: u16,
}

impl Marker {
    pub fn uri(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.key, self.row, self.rows, self.cols, self.left
        )
    }

    pub fn parse(uri: &str) -> Option<Marker> {
        let mut it = uri.split(':');
        let key = it.next()?.parse().ok()?;
        let mut n = || it.next()?.parse::<u16>().ok();
        let m = Marker {
            key,
            row: n()?,
            rows: n()?,
            cols: n()?,
            left: n()?,
        };
        (m.row < m.rows && m.cols > 0).then_some(m)
    }

    /// The picture cell at column `x`, if the picture reaches it.
    pub fn at(&self, x: usize) -> Option<ferossh_render::PictureCell> {
        let col = u16::try_from(x).ok()?.checked_sub(self.left)?;
        (col < self.cols).then_some(ferossh_render::PictureCell {
            key: self.key,
            row: self.row,
            col,
            rows: self.rows,
            cols: self.cols,
        })
    }
}

/// An image sequence taken out of the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Sequence {
    /// `ESC P <params> q <body> ESC \`.
    Sixel(Vec<u8>),
    /// `ESC _ G <keys> ; <payload> ESC \`: everything after the `G`.
    Kitty(Vec<u8>),
    /// `ESC ] 1337 ; File = <args> : <payload>`: everything after
    /// `File=`.
    Iterm(Vec<u8>),
    /// A sequence longer than [`MAX_PAYLOAD`]; dropped. Kitty's keys are
    /// kept so it can be answered.
    TooLong(Option<Vec<u8>>),
}

/// A part of the output, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Bytes of the chunk passed to [`Scanner::scan`], for the emulator.
    Text(Range<usize>),
    /// Bytes held back from an earlier chunk, for the emulator.
    Held(Vec<u8>),
    Image(Sequence),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Sixel,
    Kitty,
    Iterm,
    /// `ESC P tmux; ... ESC \`: tmux passthrough, ESCs doubled inside.
    Tmux,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    /// Held an ESC.
    Esc,
    /// `ESC P` and parameters so far.
    Dcs,
    /// `ESC _`.
    Apc,
    /// `ESC ]` and the start of `1337;File=` matched so far.
    Osc,
    /// Inside an image sequence.
    Body { kind: Kind, esc: bool },
}

const ITERM: &[u8] = b"1337;File=";
const TMUX: &[u8] = b"tmux;";

/// Splits the host's output into text and image sequences, across chunks.
#[derive(Debug, Default)]
pub(crate) struct Scanner {
    state: State,
    held: Vec<u8>,
    body: Vec<u8>,
    overflow: bool,
}

impl Scanner {
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut text = 0;
        let mut i = 0;
        while i < bytes.len() {
            let Some(&b) = bytes.get(i) else { break };
            match self.state {
                State::Ground => {
                    // Fast path to the next ESC.
                    match bytes
                        .get(i..)
                        .and_then(|r| r.iter().position(|&c| c == 0x1b))
                    {
                        Some(at) => {
                            i += at;
                            if i > text {
                                out.push(Piece::Text(text..i));
                            }
                            self.held.clear();
                            self.held.push(0x1b);
                            self.state = State::Esc;
                            i += 1;
                            text = i;
                        }
                        None => i = bytes.len(),
                    }
                    continue;
                }
                State::Esc => match b {
                    b'P' => self.state = State::Dcs,
                    b'_' => self.state = State::Apc,
                    b']' => self.state = State::Osc,
                    _ => {
                        self.not_ours(&mut out, i, &mut text);
                        continue;
                    }
                },
                State::Dcs => {
                    let params = self.held.get(2..).unwrap_or(&[]);
                    if b == b'q' && params.iter().all(|c| c.is_ascii_digit() || *c == b';') {
                        self.enter(Kind::Sixel);
                    } else if params.is_empty() && b == b't' {
                        // Could be tmux passthrough; checked below.
                    } else if params.first() == Some(&b't') {
                        // "tmux;" so far?
                        let so_far = params.len();
                        if TMUX.get(so_far) != Some(&b) {
                            self.not_ours(&mut out, i, &mut text);
                            continue;
                        }
                        if so_far + 1 == TMUX.len() {
                            self.enter(Kind::Tmux);
                            i += 1;
                            text = i;
                            continue;
                        }
                    } else if !(b.is_ascii_digit() || b == b';') || self.held.len() >= MAX_HELD {
                        self.not_ours(&mut out, i, &mut text);
                        continue;
                    }
                }
                State::Apc => {
                    if b == b'G' {
                        self.enter(Kind::Kitty);
                        i += 1;
                        text = i;
                        continue;
                    }
                    self.not_ours(&mut out, i, &mut text);
                    continue;
                }
                State::Osc => {
                    let so_far = self.held.len() - 2;
                    if ITERM.get(so_far) != Some(&b) {
                        self.not_ours(&mut out, i, &mut text);
                        continue;
                    }
                    if so_far + 1 == ITERM.len() {
                        self.enter(Kind::Iterm);
                        i += 1;
                        text = i;
                        continue;
                    }
                }
                State::Body { kind, esc } => {
                    // Bulk-copy up to the next byte that matters.
                    if !esc {
                        let rest = bytes.get(i..).unwrap_or(&[]);
                        let stop = rest
                            .iter()
                            .position(|&c| matches!(c, 0x1b | 0x07 | 0x18 | 0x1a))
                            .unwrap_or(rest.len());
                        self.push_body(rest.get(..stop).unwrap_or(&[]));
                        i += stop;
                        let Some(&c) = bytes.get(i) else { break };
                        i += 1;
                        match c {
                            0x1b => self.state = State::Body { kind, esc: true },
                            0x07 if kind == Kind::Iterm => self.finish(kind, &mut out),
                            0x07 => self.push_body(&[c]),
                            // CAN and SUB cancel a sequence.
                            _ => self.reset(),
                        }
                        text = i;
                        continue;
                    }
                    if b == b'\\' {
                        i += 1;
                        text = i;
                        self.finish(kind, &mut out);
                        continue;
                    }
                    if kind == Kind::Tmux && b == 0x1b {
                        self.push_body(&[0x1b]);
                        self.state = State::Body { kind, esc: false };
                        i += 1;
                        text = i;
                        continue;
                    }
                    // Another sequence starts: this one is cut off and
                    // dropped, as the emulator would.
                    self.reset();
                    self.held.push(0x1b);
                    self.state = State::Esc;
                    text = i;
                    continue;
                }
            }
            if self.held.len() < MAX_HELD {
                self.held.push(b);
            }
            i += 1;
            text = i;
        }
        if matches!(self.state, State::Ground) && text < bytes.len() {
            out.push(Piece::Text(text..bytes.len()));
        }
        out
    }

    fn enter(&mut self, kind: Kind) {
        self.held.clear();
        self.body.clear();
        self.overflow = false;
        self.state = State::Body { kind, esc: false };
    }

    fn push_body(&mut self, bytes: &[u8]) {
        if !self.overflow && self.body.len() + bytes.len() > MAX_PAYLOAD {
            self.overflow = true;
            // Kitty's keys come first; keep them (from what came before
            // and this slice) to answer, at most a few hundred bytes.
            let room = MAX_KEYS.saturating_sub(self.body.len());
            self.body
                .extend_from_slice(bytes.get(..room.min(bytes.len())).unwrap_or(&[]));
            let keys = self
                .body
                .iter()
                .position(|&c| c == b';')
                .unwrap_or(0)
                .min(MAX_KEYS);
            self.body.truncate(keys);
            self.body.shrink_to_fit();
            return;
        }
        if !self.overflow {
            self.body.extend_from_slice(bytes);
        }
    }

    /// The held bytes were not an image sequence: they go to the emulator,
    /// and byte `i` is looked at again from the ground state.
    fn not_ours(&mut self, out: &mut Vec<Piece>, i: usize, text: &mut usize) {
        out.push(Piece::Held(std::mem::take(&mut self.held)));
        self.state = State::Ground;
        *text = i;
    }

    fn reset(&mut self) {
        self.state = State::Ground;
        self.body = Vec::new();
        self.held.clear();
        self.overflow = false;
    }

    fn finish(&mut self, kind: Kind, out: &mut Vec<Piece>) {
        let body = std::mem::take(&mut self.body);
        let overflow = self.overflow;
        self.reset();
        if overflow {
            out.push(Piece::Image(Sequence::TooLong(
                (kind == Kind::Kitty).then_some(body),
            )));
            return;
        }
        match kind {
            Kind::Sixel => out.push(Piece::Image(Sequence::Sixel(body))),
            Kind::Kitty => out.push(Piece::Image(Sequence::Kitty(body))),
            Kind::Iterm => out.push(Piece::Image(Sequence::Iterm(body))),
            Kind::Tmux => {
                // What tmux would have passed on: only the images in it
                // count; other passthrough is dropped, as tmux does by
                // default.
                let mut inner = Scanner::default();
                for piece in inner.scan(&body) {
                    if let Piece::Image(s) = piece {
                        out.push(Piece::Image(s));
                    }
                }
            }
        }
    }
}

/// The pictures one terminal shows or has been sent.
#[derive(Debug, Default)]
pub(crate) struct Store {
    pictures: HashMap<u32, Picture>,
    order: VecDeque<u32>,
    bytes: usize,
    /// Kitty image ids to keys.
    kitty: HashMap<u32, u32>,
    /// Cropped copies made for placements of a kitty image, by its id,
    /// so deleting the image deletes them too.
    derived: HashMap<u32, Vec<u32>>,
    /// A kitty transmission whose chunks are still coming: its first
    /// chunk's keys and the payload so far.
    pub chunks: Option<(Vec<u8>, Vec<u8>)>,
}

impl Store {
    /// Keeps `picture`, dropping the oldest ones over the budget; returns
    /// its key.
    pub fn add(&mut self, picture: ferossh_images::Picture) -> Picture {
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        let size = picture.bytes();
        let picture = Picture {
            key,
            width: picture.width,
            height: picture.height,
            pixels: Arc::from(picture.pixels),
        };
        while !self.order.is_empty()
            && (self.bytes + size > STORE_BYTES || self.order.len() >= STORE_PICTURES)
        {
            if let Some(old) = self.order.pop_front() {
                self.remove(old);
            }
        }
        self.bytes += size;
        self.order.push_back(key);
        self.pictures.insert(key, picture.clone());
        picture
    }

    pub fn get(&self, key: u32) -> Option<&Picture> {
        self.pictures.get(&key)
    }

    fn remove(&mut self, key: u32) {
        if let Some(p) = self.pictures.remove(&key) {
            self.bytes = self.bytes.saturating_sub(p.pixels.len() * 4);
        }
        self.order.retain(|k| *k != key);
        self.kitty.retain(|_, k| *k != key);
        for keys in self.derived.values_mut() {
            keys.retain(|k| *k != key);
        }
        self.derived.retain(|_, keys| !keys.is_empty());
    }

    /// Notes that `key` is a cropped copy of kitty image `id`.
    pub fn derive_kitty(&mut self, id: u32, key: u32) {
        self.derived.entry(id).or_default().push(key);
    }

    /// Every key kitty image `id` shows under: its own and its copies'.
    pub fn kitty_keys(&self, id: u32) -> Vec<u32> {
        let mut keys: Vec<u32> = self.kitty.get(&id).copied().into_iter().collect();
        keys.extend(self.derived.get(&id).into_iter().flatten().copied());
        keys
    }

    pub fn set_kitty(&mut self, id: u32, key: u32) {
        if let Some(old) = self.kitty.insert(id, key)
            && old != key
        {
            self.remove(old);
            self.kitty.insert(id, key);
        }
    }

    pub fn kitty(&self, id: u32) -> Option<&Picture> {
        self.get(*self.kitty.get(&id)?)
    }

    /// Forgets kitty image `id`'s data, or every kitty image's.
    pub fn free_kitty(&mut self, id: Option<u32>) {
        let keys: Vec<u32> = match id {
            Some(id) => self.kitty_keys(id),
            None => self
                .kitty
                .values()
                .chain(self.derived.values().flatten())
                .copied()
                .collect(),
        };
        for key in keys {
            self.remove(key);
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn run(chunks: &[&[u8]]) -> (Vec<u8>, Vec<Sequence>) {
        let mut s = Scanner::default();
        let (mut text, mut seqs) = (Vec::new(), Vec::new());
        for chunk in chunks {
            for p in s.scan(chunk) {
                match p {
                    Piece::Text(r) => text.extend_from_slice(&chunk[r]),
                    Piece::Held(h) => text.extend_from_slice(&h),
                    Piece::Image(q) => seqs.push(q),
                }
            }
        }
        (text, seqs)
    }

    #[test]
    fn passes_text_and_other_sequences() {
        let input: &[u8] =
            b"a\x1b[31mb\x1bPzz\x1b\\c\x1b]0;t\x07d\x1b_X\x1b\\e\x1b]1337;SetMark\x07";
        let (text, seqs) = run(&[input]);
        assert_eq!(text, input);
        assert!(seqs.is_empty());
        // Split anywhere, the same.
        for at in 0..input.len() {
            let (a, b) = input.split_at(at);
            assert_eq!(run(&[a, b]).0, input, "split at {at}");
        }
    }

    #[test]
    fn finds_images() {
        let input: &[u8] =
            b"x\x1bP0;1;0q#0~~\x1b\\y\x1b_Ga=T,f=100;QUJD\x1b\\z\x1b]1337;File=inline=1:QUJD\x07w";
        let want = vec![
            Sequence::Sixel(b"#0~~".to_vec()),
            Sequence::Kitty(b"a=T,f=100;QUJD".to_vec()),
            Sequence::Iterm(b"inline=1:QUJD".to_vec()),
        ];
        assert_eq!(run(&[input]), (b"xyzw".to_vec(), want.clone()));
        for at in 0..input.len() {
            let (a, b) = input.split_at(at);
            assert_eq!(
                run(&[a, b]),
                (b"xyzw".to_vec(), want.clone()),
                "split at {at}"
            );
        }
    }

    #[test]
    fn tmux_passthrough_and_cancel() {
        let (text, seqs) = run(&[b"\x1bPtmux;\x1b\x1b_Gi=1;QQ==\x1b\x1b\\\x1b\\after"]);
        assert_eq!(text, b"after");
        assert_eq!(seqs, vec![Sequence::Kitty(b"i=1;QQ==".to_vec())]);
        // A sequence cut off by another one is dropped.
        let (text, seqs) = run(&[b"\x1b_Gi=1;QQ\x1b[1mX"]);
        assert_eq!(text, b"\x1b[1mX");
        assert!(seqs.is_empty());
        let (text, seqs) = run(&[b"\x1bPq~~\x18X"]);
        assert_eq!(text, b"X");
        assert!(seqs.is_empty());
    }

    #[test]
    fn too_long_keeps_kitty_keys() {
        let mut s = Scanner::default();
        let mut big = b"\x1b_Gi=42,a=q;".to_vec();
        big.resize(MAX_PAYLOAD + 10, b'A');
        big.extend_from_slice(b"\x1b\\");
        let pieces = s.scan(&big);
        assert_eq!(
            pieces,
            vec![Piece::Image(Sequence::TooLong(Some(b"i=42,a=q".to_vec())))]
        );
    }

    #[test]
    fn markers_round_trip() {
        let m = Marker {
            key: 9,
            row: 1,
            rows: 3,
            cols: 4,
            left: 2,
        };
        assert_eq!(Marker::parse(&m.uri()), Some(m));
        assert_eq!(Marker::parse("1:2"), None);
        assert_eq!(Marker::parse("1:3:3:4:0"), None, "row past the picture");
        assert_eq!(m.at(1), None);
        assert_eq!(m.at(3).map(|c| (c.col, c.row)), Some((1, 1)));
        assert_eq!(m.at(6), None);
    }

    #[test]
    fn store_budget() {
        let mut s = Store::default();
        let pic = |n: usize| ferossh_images::Picture {
            width: n as u32,
            height: 1,
            pixels: vec![0; n],
        };
        let a = s.add(pic(8 << 20)).key;
        let b = s.add(pic(8 << 20)).key;
        assert!(s.get(a).is_some());
        let c = s.add(pic(8 << 20)).key;
        assert!(s.get(a).is_none(), "oldest dropped");
        assert!(s.get(b).is_some() && s.get(c).is_some());
        s.set_kitty(5, b);
        assert_eq!(s.kitty_keys(5), vec![b]);
        s.derive_kitty(5, c);
        assert_eq!(s.kitty_keys(5), vec![b, c]);
        s.free_kitty(Some(5));
        assert!(s.get(b).is_none());
    }
}
