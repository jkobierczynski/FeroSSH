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
//! underlined while the link is very slow (from about 80 ms), to mark
//! them as not yet confirmed.

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
/// Characters a prompt may show in place of hidden input: typing one of
/// these and seeing it proves nothing, so it never earns trust.
const MASKS: [char; 5] = ['*', '\u{2022}', '\u{25cf}', '\u{2219}', '\u{00b7}'];

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
    /// The cell showed something else when the key was typed, so the
    /// screen showing the character later proves the server echoed it.
    proves: bool,
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
    /// The server's screen at the last update.
    last: Option<Screen>,
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
                    let proves = cell.is_some_and(|(row, col, c)| {
                        c.is_some_and(|k| !MASKS.contains(&k)) && !shows(screen, row, col, c)
                    });
                    self.guesses.push(Guess {
                        pos,
                        cell,
                        cursor,
                        at: now,
                        proves,
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
        let cursor = (screen.ds.cursor_row, screen.ds.cursor_col);
        let last = self.last.replace(screen.clone());
        if last.is_some_and(|last| self.unexplained(&last, screen)) {
            // The program wrote something that is not one of our keys
            // (the server may echo before it acknowledges, so pending
            // guesses count): perhaps a password prompt. Start over.
            if !self.guesses.is_empty() {
                self.changed();
            }
            self.guesses.clear();
            self.trusted = false;
        }
        let done = self.guesses.iter().take_while(|g| g.pos <= echoed).count();
        if done > 0 {
            let checked: Vec<Guess> = self.guesses.drain(..done).collect();
            // Only the last guess for each cell must still show: an
            // earlier one was typed over or erased by a later keystroke.
            let mut cells = Vec::new();
            // The server may already show keys it has not acknowledged:
            // the cursor and a cell may be where a pending guess put them.
            let mut right = checked.last().is_some_and(|g| cursor == g.cursor)
                || self.guesses.iter().any(|g| g.cursor == cursor);
            // Trust needs a typed character that came true: an erased
            // cell or a cursor move also "holds" where the server echoes
            // nothing, as at a password prompt.
            let mut typed = false;
            for g in checked.iter().rev() {
                let Some((row, col, _)) = g.cell else {
                    continue;
                };
                if cells.contains(&(row, col)) {
                    continue;
                }
                cells.push((row, col));
                if !holds(screen, g) {
                    let later = |p: &Guess| {
                        p.cell.is_some_and(|(r, k, _)| (r, k) == (row, col)) && holds(screen, p)
                    };
                    if self.guesses.iter().any(later) {
                        continue;
                    }
                    right = false;
                    break;
                }
                typed |= g.proves;
            }
            if !right {
                self.guesses.clear();
                self.trusted = false;
            } else if typed {
                self.trusted = true;
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

    /// Whether the screen changed from `last` other than as a guess
    /// (checked or pending) types, erases or moves the cursor.
    fn unexplained(&self, last: &Screen, screen: &Screen) -> bool {
        let cursor = (screen.ds.cursor_row, screen.ds.cursor_col);
        if cursor != (last.ds.cursor_row, last.ds.cursor_col)
            && !self.guesses.iter().any(|g| g.cursor == cursor)
        {
            return true;
        }
        if last.rows.len() != screen.rows.len() {
            return true;
        }
        // A changed cell is ours only if it now shows what a guess for
        // that cell types or erases.
        let guessed = |row: usize, col: usize| {
            self.guesses.iter().any(|g| {
                g.cell
                    .is_some_and(|(r, k, c)| (r, k) == (row, col) && shows(screen, r, k, c))
            })
        };
        last.rows
            .iter()
            .zip(&screen.rows)
            .enumerate()
            .any(|(row, (a, b))| {
                !Arc::ptr_eq(a, b)
                    && (a.cells.len() != b.cells.len()
                        || a.cells
                            .iter()
                            .zip(&b.cells)
                            .enumerate()
                            .any(|(col, (x, y))| x != y && !guessed(row, col)))
            })
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
    g.cell
        .is_none_or(|(row, col, c)| shows(screen, row, col, c))
}

/// Whether the cell at `row`, `col` shows `c` (`None`: is blank).
fn shows(screen: &Screen, row: usize, col: usize, c: Option<char>) -> bool {
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
    fn a_typed_over_cell_counts_only_its_last_guess() {
        let mut server = Screen::new(20, 1);
        server.apply(b"$ ");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"l", 1, &server, 0);
        server.apply(b"l");
        p.update(&server, 1, 100.0, 0);
        // "x" then Backspace, both echoed in one go: the cell is empty
        // again, and that is right.
        p.typed(b"x\x7f", 3, &server, 0);
        server.apply(b"x\x08\x1b[K");
        p.update(&server, 3, 100.0, 0);
        p.typed(b"s", 4, &server, 0);
        p.update(&server, 3, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "$ ls");
    }

    #[test]
    fn erasing_at_a_password_prompt_earns_no_trust() {
        let mut server = Screen::new(30, 1);
        server.apply(b"Password: ");
        let mut p = Predictor::new(Mode::Always);
        // Typed and erased: the screen is unchanged, as it would be
        // whether or not the server echoes.
        p.typed(b"a\x7f", 2, &server, 0);
        p.update(&server, 2, 100.0, 0);
        p.typed(b"b", 3, &server, 0);
        p.update(&server, 2, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "Password:");
    }

    #[test]
    fn a_prompt_drawn_under_the_cursor_resets_trust() {
        let mut server = Screen::new(30, 1);
        server.apply(b"$ ");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"l", 1, &server, 0);
        server.apply(b"l");
        p.update(&server, 1, 100.0, 0);
        // The line is redrawn as a prompt, the cursor where it was.
        server.apply(b"\rPw:");
        assert_eq!(server.ds.cursor_col, 3);
        p.update(&server, 1, 100.0, 0);
        p.typed(b"h", 2, &server, 0);
        p.update(&server, 1, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "Pw:");
    }

    #[test]
    fn other_text_in_a_guessed_cell_resets_trust() {
        let mut server = Screen::new(30, 1);
        server.apply(b"$ ");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"l", 1, &server, 0);
        server.apply(b"l");
        p.update(&server, 1, 100.0, 0);
        // "s" is pending at column 3, but the program writes something
        // else there, leaving the cursor where "s" would.
        p.typed(b"s", 2, &server, 0);
        server.apply(b"*");
        p.update(&server, 1, 100.0, 0);
        p.typed(b"h", 3, &server, 0);
        p.update(&server, 1, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "$ l*");
    }

    #[test]
    fn a_mask_character_earns_no_trust() {
        // A prompt that shows "*" for each key: a password starting with
        // "*" looks echoed, but must not unlock the rest.
        let mut server = Screen::new(30, 1);
        server.apply(b"Password: ");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"*", 1, &server, 0);
        server.apply(b"*");
        p.update(&server, 1, 100.0, 0);
        p.typed(b"s", 2, &server, 0);
        p.update(&server, 1, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "Password: *");
    }

    #[test]
    fn a_character_already_there_earns_no_trust() {
        // A hidden prompt with an "x" under the cursor: typing x and Left,
        // both acknowledged and neither echoed, leaves the screen as is.
        let mut server = Screen::new(30, 1);
        server.apply(b"Password: x\x08");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"x\x1b[D", 2, &server, 0);
        p.update(&server, 2, 100.0, 0);
        p.typed(b"s", 3, &server, 0);
        p.update(&server, 2, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "Password: x");
    }

    #[test]
    fn a_screen_ahead_of_the_acknowledgement_keeps_trust() {
        let mut server = Screen::new(20, 1);
        server.apply(b"$ ");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"l", 1, &server, 0);
        server.apply(b"l");
        p.update(&server, 1, 100.0, 0);
        // The server shows a, b and x-then-erased, but has acknowledged
        // only up to x.
        for (i, k) in [&b"a"[..], b"b", b"x", b"\x7f"].into_iter().enumerate() {
            p.typed(k, 2 + i as u64, &server, 0);
        }
        server.apply(b"abx\x08\x1b[K");
        p.update(&server, 4, 100.0, 0);
        p.typed(b"s", 6, &server, 0);
        p.update(&server, 4, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 0), "$ labs");
    }

    #[test]
    fn a_prompt_appearing_on_its_own_resets_trust() {
        let mut server = Screen::new(30, 2);
        server.apply(b"$ ");
        let mut p = Predictor::new(Mode::Always);
        p.typed(b"l", 1, &server, 0);
        server.apply(b"l");
        p.update(&server, 1, 100.0, 0);
        // No keystroke of ours, yet the screen moves on: say a locked
        // tmux asking for the password.
        server.apply(b"\r\nPassword: ");
        p.update(&server, 1, 100.0, 0);
        p.typed(b"h", 2, &server, 0);
        p.update(&server, 1, 100.0, 0);
        assert_eq!(text(&shown(&p, &server), 1), "Password:");
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
