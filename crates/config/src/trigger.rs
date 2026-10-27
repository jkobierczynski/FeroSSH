//! Auto-responses: snippets that answer the host by themselves when it
//! prints their `When` text, such as a space for `--More--` or Enter for
//! `[confirm]` (see [`crate::login::Snippet::when`]).
//!
//! The host decides when one fires, never what it types: a [`Watch`] only
//! says which of the user's snippets matched, and those may type lines and
//! keys the user wrote, no secrets, no clipboard and no arguments (see
//! [`crate::login::Snippet::auto_problem`]). A host that keeps printing the
//! text cannot make FeroSSH type without end: past [`BURST`] answers in a
//! second the watch pauses until the user types.

use std::collections::VecDeque;

use crate::login::MAX_TEXT;

/// Answers in one second, in one pane, before the watch pauses.
pub const BURST: usize = 20;

/// Escape sequence state, so a sequence split across reads is still
/// dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Lex {
    #[default]
    Text,
    Esc,
    /// CSI: up to a final byte.
    Csi,
    /// OSC, DCS and the like: up to BEL or ST.
    Str,
    StrEsc,
}

/// The text a host prints, without escape sequences (colours, titles),
/// as login scripts and auto-responses match it.
#[derive(Debug, Clone, Default)]
pub struct OutputText {
    esc: Lex,
    /// Bytes of an unfinished UTF-8 character.
    partial: Vec<u8>,
}

impl OutputText {
    /// Adds the text in `bytes` to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut String) {
        for &b in bytes {
            self.esc = match (self.esc, b) {
                (Lex::Text, 0x1b) => Lex::Esc,
                (Lex::Text, b) if b >= 0x20 || matches!(b, b'\r' | b'\n' | b'\t') => {
                    self.partial.push(b);
                    if b < 0x80 || std::str::from_utf8(&self.partial).is_ok() {
                        out.push_str(&String::from_utf8_lossy(&self.partial));
                        self.partial.clear();
                    } else if self.partial.len() >= 4 {
                        self.partial.clear();
                    }
                    Lex::Text
                }
                (Lex::Text, _) => Lex::Text,
                (Lex::Esc, b'[') => Lex::Csi,
                (Lex::Esc, b']' | b'P' | b'X' | b'^' | b'_') => Lex::Str,
                (Lex::Esc, _) => Lex::Text,
                (Lex::Csi, 0x40..=0x7e) => Lex::Text,
                (Lex::Csi, _) => Lex::Csi,
                (Lex::Str, 0x07) => Lex::Text,
                (Lex::Str, 0x1b) => Lex::StrEsc,
                (Lex::Str, _) => Lex::Str,
                (Lex::StrEsc, b'\\') => Lex::Text,
                (Lex::StrEsc, _) => Lex::Str,
            };
        }
    }
}

/// What a [`Watch`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fire {
    /// The snippet with this number answers.
    Snippet(usize),
    /// Too many answers too fast: the watch pauses until the user types.
    Paused,
}

/// One pane's watch for auto-response texts.
#[derive(Debug, Clone, Default)]
pub struct Watch {
    text: OutputText,
    /// What the host printed since the last match, at most
    /// [`MAX_TEXT`] characters of it.
    seen: String,
    /// When the last answers fired, in milliseconds.
    fired: VecDeque<u64>,
    paused: bool,
}

impl Watch {
    /// Whether it paused after too many answers.
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The user typed: a paused watch starts again, on new text only.
    pub fn resume(&mut self) {
        if self.paused {
            self.paused = false;
            self.seen.clear();
            self.fired.clear();
        }
    }

    /// What the host printed, at `now_ms`; `whens` are the texts of the
    /// snippets that apply, with their numbers. The text that completes
    /// first fires first (the longest when two end together), and what it
    /// matched is used up, so one prompt fires once.
    pub fn feed(&mut self, bytes: &[u8], whens: &[(usize, &str)], now_ms: u64) -> Vec<Fire> {
        let mut out = Vec::new();
        self.text.feed(bytes, &mut self.seen);
        if self.paused || whens.is_empty() {
            self.seen.clear();
            return out;
        }
        while let Some((index, end)) = whens
            .iter()
            .filter(|(_, w)| !w.is_empty())
            .filter_map(|&(i, w)| self.seen.find(w).map(|at| (i, at + w.len(), w.len())))
            .min_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)))
            .map(|(i, end, _)| (i, end))
        {
            self.seen.drain(..end);
            while self
                .fired
                .front()
                .is_some_and(|&t| now_ms.saturating_sub(t) >= 1000)
            {
                self.fired.pop_front();
            }
            if self.fired.len() >= BURST {
                self.paused = true;
                self.seen.clear();
                out.push(Fire::Paused);
                return out;
            }
            self.fired.push_back(now_ms);
            out.push(Fire::Snippet(index));
        }
        // Only the tail can still start a match.
        let keep = MAX_TEXT;
        if self.seen.chars().count() > keep {
            let cut = self
                .seen
                .char_indices()
                .rev()
                .nth(keep - 1)
                .map_or(0, |(b, _)| b);
            self.seen.drain(..cut);
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const MORE: (usize, &str) = (4, "--More--");
    const CONFIRM: (usize, &str) = (7, "[confirm]");

    #[test]
    fn fires_once_per_prompt_even_in_pieces() {
        let mut w = Watch::default();
        let whens = [MORE, CONFIRM];
        assert!(w.feed(b"line 1\r\n \x1b[7m--Mo", &whens, 0).is_empty());
        assert_eq!(w.feed(b"re--\x1b[0m", &whens, 0), [Fire::Snippet(4)]);
        assert!(w.feed(b"\x08\x08\x08 line 2", &whens, 0).is_empty());
        assert_eq!(
            w.feed(b"Proceed? [confirm]x --More--", &whens, 0),
            [Fire::Snippet(7), Fire::Snippet(4)]
        );
        // Not in a title or other escape string.
        assert!(w.feed(b"\x1b]0;--More--\x07", &whens, 0).is_empty());
        assert!(w.feed(b"--More--", &[], 0).is_empty(), "none apply");
        assert!(
            w.feed(b"", &whens, 0).is_empty(),
            "text seen with none applying is gone"
        );
    }

    #[test]
    fn the_first_to_complete_wins() {
        let mut w = Watch::default();
        let whens = [(0, "abcdef"), (1, "cd"), (2, "bcd")];
        assert_eq!(w.feed(b"abcdef", &whens, 0), [Fire::Snippet(2)]);
    }

    #[test]
    fn a_flood_pauses_until_the_user_types() {
        let mut w = Watch::default();
        let flood = "--More--".repeat(BURST + 5);
        let out = w.feed(flood.as_bytes(), &[MORE], 0);
        assert_eq!(out.len(), BURST + 1);
        assert_eq!(out.last(), Some(&Fire::Paused));
        assert!(w.paused());
        assert!(w.feed(b"--More--", &[MORE], 5000).is_empty());
        w.resume();
        assert_eq!(w.feed(b"--More--", &[MORE], 5000), [Fire::Snippet(4)]);
        // Spread over time, as many as the host asks for.
        let mut w = Watch::default();
        for t in 0..100 {
            assert_eq!(w.feed(b"--More--", &[MORE], t * 100), [Fire::Snippet(4)]);
        }
    }

    #[test]
    fn long_output_keeps_only_a_tail() {
        let mut w = Watch::default();
        let junk = "x".repeat(100_000);
        assert!(w.feed(junk.as_bytes(), &[MORE], 0).is_empty());
        assert!(w.seen.chars().count() <= MAX_TEXT);
        assert_eq!(
            w.feed("é--More--".as_bytes(), &[MORE], 0),
            [Fire::Snippet(4)]
        );
    }
}
