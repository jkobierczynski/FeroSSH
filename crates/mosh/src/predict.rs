//! Local echo: what the user types shows at once, before the server's
//! echo comes back over a slow link.
//!
//! Each keystroke that only types a character, erases one with
//! Backspace, or moves the cursor with Left or Right becomes a guess:
//! the cell it changes and where the cursor ends up. Any other key (Enter,
//! a control key, another escape sequence) makes the screen's next state
//! unknowable, so guessing stops until the server has echoed that key.
//!
//! Guesses are checked once the server says it has echoed the keystroke
//! (its `echo_ack`): a guess the screen bears out is dropped as done; one
//! it does not (a password prompt that echoes nothing, an editor that
//! maps the key) drops every guess and makes the rest tentative again.
//! Tentative guesses are made but not shown until one of them comes true,
//! so nothing a program keeps hidden is ever drawn. Shown guesses are
//! underlined while the link is slow, to mark them as not yet confirmed.

use std::sync::Arc;

use unicode_width::UnicodeWidthChar;

use crate::screen::{Cell, Screen, UNDERLINE};

/// When typed text is shown before the server echoes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Only when the link is slow (or an echo is late).
    #[default]
    Adaptive,
    /// Whenever the guesses have been right.
    Always,
    /// Never: only what the server sends.
    Never,
}

/// Show guesses from this round-trip time on (ms)…
const SHOW_RTT: f64 = 30.0;
/// …until it falls below this.
const HIDE_RTT: f64 = 20.0;
/// Underline them from this round-trip time on…
const FLAG_RTT: f64 = 80.0;
/// …until it falls below this.
const UNFLAG_RTT: f64 = 50.0;
/// An echo this late (ms) shows guesses whatever the round-trip time.
const LATE_ECHO: u64 = 250;
/// Guesses kept at most; typing past this is not guessed.
const MAX_GUESSES: usize = 512;

/// One keystroke's guess.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Guess {
    /// The event-stream position just after the keystroke.
    pos: u64,
    /// The cell typed into or erased, and what it then holds (`None`
    /// for an erased cell); none for a cursor move.
    cell: Option<(usize, usize, Option<char>)>,
    /// Where the cursor is after the keystroke.
    cursor: (usize, usize),
    /// When it was made (ms).
    at: u64,
}

/// The guesses not yet confirmed, and whether they may be shown.
#[derive(Debug, Default)]
pub struct Predictor {
    mode: Mode,
    guesses: Vec<Guess>,
    /// Guessing waits until the server has echoed up to here.
    frozen_until: Option<u64>,
    /// A guess came true since guessing (re)started: the rest are shown.
    trusted: bool,
    showing: bool,
    flagging: bool,
    /// Bumped whenever what [`Predictor::overlay`] draws may change.
    version: u64,
    /// Undecoded bytes of a UTF-8 character or escape sequence.
    pending: Vec<u8>,
}

impl Predictor {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Changes whenever the overlay may look different.
    pub fn version(&self) -> u64 {
        self.version
    }

    fn changed(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    /// Stops guessing until the server has echoed up to `pos`; what was
    /// guessed so far goes.
    fn freeze(&mut self, pos: u64) {
        if !self.guesses.is_empty() {
            self.changed();
        }
        self.guesses.clear();
        self.frozen_until = Some(pos);
        self.trusted = false;
    }

    /// The pane changed size: every guess is off.
    pub fn resized(&mut self, pos: u64) {
        self.freeze(pos);
    }

    /// Keystrokes `bytes` were queued, ending at event position `pos`;
    /// `screen` is the newest state from the server.
    pub fn typed(&mut self, bytes: &[u8], pos: u64, screen: &Screen, now: u64) {
        if self.mode == Mode::Never {
            return;
        }
        let mut cursor = self
            .guesses
            .last()
            .map_or((screen.ds.cursor_row, screen.ds.cursor_col), |g| g.cursor);
        let (w, h) = (screen.width(), screen.height());
        self.pending.extend_from_slice(bytes);
        let input = std::mem::take(&mut self.pending);
        let mut rest = input.as_slice();
        while !rest.is_empty() {
            let (key, used) = match next_key(rest) {
                Next::Key(key, used) => (key, used),
                Next::Partial => {
                    self.pending = rest.to_vec();
                    break;
                }
            };
            rest = rest.get(used..).unwrap_or_default();
            if self.frozen_until.is_some() {
                // Still waiting for the echo of a key that could not be
                // guessed: later keys move the wait along.
                self.frozen_until = Some(pos);
                continue;
            }
            let guessable =
                screen.ds.cursor_visible && cursor.0 < h && self.guesses.len() < MAX_GUESSES;
            let (row, col) = cursor;
            let guess = match key {
                Key::Char(c) if guessable && c.width() == Some(1) && col + 1 < w => {
                    cursor = (row, col + 1);
                    Some(Some((row, col, Some(c))))
                }
                Key::Backspace if guessable && col > 0 && col < w => {
                    cursor = (row, col - 1);
                    Some(Some((row, col - 1, None)))
                }
                Key::Left if guessable && col > 0 => {
                    cursor = (row, col - 1);
                    Some(None)
                }
                Key::Right if guessable && col + 1 < w => {
                    cursor = (row, col + 1);
                    Some(None)
                }
                _ => None,
            };
            match guess {
                Some(cell) => {
                    self.guesses.push(Guess {
                        pos,
                        cell,
                        cursor,
                        at: now,
                    });
                    if self.trusted {
                        self.changed();
                    }
                }
                None => self.freeze(pos),
            }
        }
    }

    /// Checks the guesses against the newest screen: the server has
    /// echoed every keystroke up to event position `echoed`. `srtt` is
    /// the smoothed round-trip time.
    pub fn update(&mut self, screen: &Screen, echoed: u64, srtt: f64, now: u64) {
        if self.frozen_until.is_some_and(|f| echoed >= f) {
            // Caught up: guess again, tentatively.
            self.frozen_until = None;
            self.trusted = false;
        }
        let done = self.guesses.iter().take_while(|g| g.pos <= echoed).count();
        if done > 0 {
            let checked: Vec<Guess> = self.guesses.drain(..done).collect();
            let right = checked.iter().all(|g| holds(screen, g))
                && checked
                    .last()
                    .is_some_and(|g| (screen.ds.cursor_row, screen.ds.cursor_col) == g.cursor);
            if right {
                if !self.trusted {
                    self.trusted = true;
                }
            } else {
                self.guesses.clear();
                self.trusted = false;
            }
            self.changed();
        }
        let late = self
            .guesses
            .first()
            .is_some_and(|g| now.saturating_sub(g.at) >= LATE_ECHO);
        let showing = match self.mode {
            Mode::Never => false,
            Mode::Always => true,
            Mode::Adaptive => late || srtt >= SHOW_RTT || (self.showing && srtt >= HIDE_RTT),
        };
        let flagging = srtt >= FLAG_RTT || (self.flagging && srtt >= UNFLAG_RTT);
        if (showing, flagging) != (self.showing, self.flagging) {
            self.showing = showing;
            self.flagging = flagging;
            self.changed();
        }
    }

    /// Whether guesses are drawn now.
    pub fn active(&self) -> bool {
        self.showing && self.trusted && !self.guesses.is_empty()
    }

    /// Draws the guesses over `screen` (a copy of the newest state).
    pub fn overlay(&self, screen: &mut Screen) {
        if !self.active() {
            return;
        }
        let (w, h) = (screen.width(), screen.height());
        for g in &self.guesses {
            let Some((row, col, c)) = g.cell else {
                continue;
            };
            if row >= h || col >= w {
                continue;
            }
            let Some(r) = screen.rows.get_mut(row) else {
                continue;
            };
            let Some(cell) = Arc::make_mut(r).cells.get_mut(col) else {
                continue;
            };
            let mut rend = cell.rend;
            if self.flagging {
                rend.attrs |= UNDERLINE;
            }
            *cell = Cell {
                text: c.map(String::from).unwrap_or_default(),
                rend,
                ..Cell::default()
            };
        }
        if let Some(g) = self.guesses.last() {
            screen.ds.cursor_row = g.cursor.0.min(h.saturating_sub(1));
            screen.ds.cursor_col = g.cursor.1.min(w.saturating_sub(1));
        }
    }
}

/// Whether the screen shows what guess `g` put in its cell.
fn holds(screen: &Screen, g: &Guess) -> bool {
    let Some((row, col, c)) = g.cell else {
        return true;
    };
    let Some(cell) = screen.cell(row, col) else {
        return false;
    };
    match c {
        Some(c) => cell.text.chars().eq(std::iter::once(c)),
        None => cell.text.is_empty() || cell.text == " ",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Char(char),
    Backspace,
    Left,
    Right,
    /// Anything else.
    Other,
}

enum Next {
    /// A key, and the bytes it took.
    Key(Key, usize),
    /// The bytes end inside a character or escape sequence.
    Partial,
}

/// The first key in `bytes`.
fn next_key(bytes: &[u8]) -> Next {
    let Some(&first) = bytes.first() else {
        return Next::Partial;
    };
    match first {
        0x08 | 0x7F => Next::Key(Key::Backspace, 1),
        0x1B => match bytes.get(1) {
            None => Next::Partial,
            Some(b'[' | b'O') => match bytes.get(2) {
                None => Next::Partial,
                Some(b'C') => Next::Key(Key::Right, 3),
                Some(b'D') => Next::Key(Key::Left, 3),
                Some(_) => {
                    // Some other sequence: up to its final byte.
                    let len = bytes
                        .iter()
                        .skip(2)
                        .position(|b| (0x40..=0x7E).contains(b))
                        .map_or(bytes.len(), |i| i + 3);
                    Next::Key(Key::Other, len)
                }
            },
            Some(_) => Next::Key(Key::Other, 2),
        },
        0x00..=0x1F => Next::Key(Key::Other, 1),
        _ => {
            let need = match first {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                _ => 1,
            };
            let Some(chunk) = bytes.get(..need) else {
                return Next::Partial;
            };
            match std::str::from_utf8(chunk)
                .ok()
                .and_then(|s| s.chars().next())
            {
                Some(c) => Next::Key(Key::Char(c), need),
                None => Next::Key(Key::Other, 1),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn text(s: &Screen, row: usize) -> String {
        s.rows[row]
            .cells
            .iter()
            .map(|c| {
                if c.text.is_empty() {
                    " "
                } else {
                    c.text.as_str()
                }
            })
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn shown(p: &Predictor, s: &Screen) -> Screen {
        let mut out = s.clone();
        p.overlay(&mut out);
        out
    }

    #[test]
    fn typing_shows_once_a_guess_comes_true() {
        let mut server = Screen::new(20, 2);
        server.apply(b"$ ");
        let mut p = Predictor::new(Mode::Always);
        // Tentative: made, not shown.
        p.typed(b"l", 1, &server, 0);
        p.update(&server, 0, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "$");
        // The server echoes it: trusted from now on.
        server.apply(b"l");
        p.update(&server, 1, 100.0, 10);
        p.typed(b"s -", 4, &server, 20);
        p.update(&server, 1, 100.0, 20);
        let s = shown(&p, &server);
        assert_eq!(text(&s, 0), "$ ls -");
        assert_eq!((s.ds.cursor_row, s.ds.cursor_col), (0, 6));
        // Slow link: underlined.
        assert_ne!(s.cell(0, 3).unwrap().rend.attrs & UNDERLINE, 0);
        // The echo arrives; nothing left to draw.
        server.apply(b"s -");
        p.update(&server, 4, 100.0, 30);
        assert!(!p.active());
    }

    #[test]
    fn backspace_and_arrows_move_the_guess() {
        let mut server = Screen::new(20, 1);
        server.apply(b"> ab");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"c", 1, &server, 0);
        server.apply(b"c");
        p.update(&server, 1, 100.0, 0);
        p.typed(b"\x7f\x7f\x1b[DX\x1bOC", 6, &server, 0);
        p.update(&server, 1, 100.0, 0);
        let s = shown(&p, &server);
        // "> abc" -> erase c, b -> left -> X over "a"... then right.
        assert_eq!(text(&s, 0), "> X");
        assert_eq!(s.ds.cursor_col, 4);
    }

    #[test]
    fn a_hidden_echo_shows_nothing() {
        // A password prompt: the server echoes nothing.
        let mut server = Screen::new(30, 1);
        server.apply(b"Password: ");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"s", 1, &server, 0);
        p.update(&server, 1, 100.0, 0);
        p.typed(b"ecret", 6, &server, 0);
        p.update(&server, 1, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "Password:");
        p.update(&server, 6, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "Password:");
    }

    #[test]
    fn a_wrong_guess_drops_the_rest() {
        let mut server = Screen::new(20, 1);
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"a", 1, &server, 0);
        server.apply(b"a");
        p.update(&server, 1, 100.0, 0);
        for (i, c) in [b"b", b"c", b"d"].into_iter().enumerate() {
            p.typed(c, 2 + i as u64, &server, 0);
        }
        p.update(&server, 1, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "abcd");
        // The program maps b to something else.
        server.apply(b"B");
        p.update(&server, 2, 100.0, 0);
        assert!(!p.active());
        assert_eq!(text(&shown(&p, &server), 0), "aB");
    }

    #[test]
    fn enter_waits_for_the_echo() {
        let mut server = Screen::new(20, 3);
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"x", 1, &server, 0);
        server.apply(b"x");
        p.update(&server, 1, 100.0, 0);
        p.typed(b"\rnext", 6, &server, 0);
        p.update(&server, 1, 100.0, 0);
        // Nothing guessed after Enter until it is echoed.
        assert_eq!(text(&shown(&p, &server), 0), "x");
        assert_eq!(text(&shown(&p, &server), 1), "");
        server.apply(b"\r\nnext");
        p.update(&server, 6, 100.0, 0);
        p.typed(b"!", 7, &server, 0);
        p.update(&server, 6, 100.0, 0);
        // Tentative again after the wait.
        assert_eq!(text(&shown(&p, &server), 1), "next");
    }

    #[test]
    fn adaptive_shows_only_on_a_slow_link() {
        let mut server = Screen::new(20, 1);
        let mut p = Predictor::new(Mode::Adaptive);
        p.typed(b"a", 1, &server, 0);
        server.apply(b"a");
        p.update(&server, 1, 5.0, 0);
        p.typed(b"b", 2, &server, 0);
        p.update(&server, 1, 5.0, 10);
        assert!(!p.active(), "fast link");
        p.update(&server, 1, 5.0, 10 + LATE_ECHO);
        assert!(p.active(), "late echo");
        let mut q = Predictor::new(Mode::Never);
        q.typed(b"a", 1, &server, 0);
        q.update(&server, 1, 500.0, 0);
        q.typed(b"b", 2, &server, 0);
        q.update(&server, 1, 500.0, 0);
        assert!(!q.active());
    }

    #[test]
    fn split_utf8_and_escapes_are_put_together() {
        let mut server = Screen::new(20, 1);
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"a", 1, &server, 0);
        server.apply(b"a");
        p.update(&server, 1, 100.0, 0);
        p.typed(&[0xC3], 2, &server, 0);
        p.typed(&[0xA9], 3, &server, 0);
        p.typed(b"\x1b", 4, &server, 0);
        p.typed(b"[D", 5, &server, 0);
        p.update(&server, 1, 100.0, 0);
        let s = shown(&p, &server);
        assert_eq!(text(&s, 0), "aé");
        assert_eq!(s.ds.cursor_col, 1);
    }

    #[test]
    fn hostile_input_is_bounded() {
        let server = Screen::new(5, 1);
        let mut p = Predictor::new(Mode::Always);
        for i in 0..2000u64 {
            p.typed(
                &[b'a' + (i % 26) as u8, 0x7F, 0xFF, 0x1B, b'['],
                i,
                &server,
                0,
            );
        }
        assert!(p.guesses.len() <= MAX_GUESSES);
        assert!(p.pending.len() < 16);
    }
}
