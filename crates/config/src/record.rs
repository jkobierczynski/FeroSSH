//! Recording keystrokes as a snippet: what the user types into a pane
//! becomes snippet lines, ready to name and check in the snippets view.
//!
//! Each Enter ends a line. Backspace, Ctrl+U and Ctrl+W edit the line as
//! a shell would; Tab, Escape and Ctrl+ keys become `@key` steps (with the
//! text before them as an `@type` step), so tab completion replays.
//! Arrow keys, function keys and Alt combinations cannot be replayed
//! faithfully and are left out, which [`Recording::skipped`] reports.
//!
//! A line the host did not echo back was most likely a password: it is
//! recorded as `@password` instead of its text, so a recording never
//! keeps a secret.

use crate::login::MAX_TEXT;
use crate::trigger::OutputText;

/// The most lines one recording keeps.
pub const MAX_LINES: usize = 50;

/// What a recording made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recording {
    pub lines: Vec<String>,
    /// Keys that were left out (arrows, function keys, Alt+...).
    pub skipped: bool,
    /// Lines the host did not echo, recorded as `@password`.
    pub hidden: usize,
}

#[derive(Debug, Default)]
pub struct Recorder {
    out: Recording,
    /// The line being typed.
    line: String,
    /// Part of this line already went out as `@type` steps.
    split: bool,
    /// What the host printed since this line began, for the echo check.
    echo: String,
    text: OutputText,
}

/// Snippet text that types `text` as it is: `$` doubled so it is never
/// an argument or variable.
fn literal(text: &str) -> String {
    text.replace('$', "$$")
}

impl Recorder {
    /// What the host printed while recording.
    pub fn output(&mut self, bytes: &[u8]) {
        self.text.feed(bytes, &mut self.echo);
        if self.echo.len() > 64 * 1024 {
            let mut cut = self.echo.len() - 32 * 1024;
            while !self.echo.is_char_boundary(cut) {
                cut += 1;
            }
            self.echo.drain(..cut);
        }
    }

    fn push_line(&mut self, line: String) {
        if self.out.lines.len() < MAX_LINES {
            self.out.lines.push(line);
        }
    }

    /// The text typed so far goes out as an `@type` step (spaces at its
    /// ends as `@key space`, since step text is trimmed).
    fn flush_typed(&mut self) {
        if self.line.is_empty() {
            return;
        }
        let line = std::mem::take(&mut self.line);
        let lead = line.len() - line.trim_start().len();
        let trail = line.trim_start().len() - line.trim().len();
        for _ in 0..lead {
            self.push_line("@key space".into());
        }
        if !line.trim().is_empty() {
            self.push_line(format!("@type {}", literal(line.trim())));
        }
        for _ in 0..trail {
            self.push_line("@key space".into());
        }
        self.split = true;
    }

    fn key(&mut self, name: &str) {
        self.flush_typed();
        self.push_line(format!("@key {name}"));
        self.split = true;
    }

    fn enter(&mut self) {
        let line = std::mem::take(&mut self.line);
        let echoed = {
            // Half the line is surely echoed by the time Enter is pressed.
            let half: String = line
                .trim()
                .chars()
                .take(line.trim().chars().count().div_ceil(2))
                .collect();
            half.is_empty() || self.echo.contains(&half)
        };
        if !echoed && !self.split {
            if self.out.lines.len() < MAX_LINES {
                self.out.hidden += 1;
            }
            self.push_line("@password".into());
        } else if line.trim().is_empty() {
            self.push_line("@key enter".into());
        } else {
            let text = literal(&line);
            let text = if text.trim_start().starts_with('@') {
                text.replacen('@', "@@", 1)
            } else {
                text
            };
            self.push_line(text);
        }
        self.split = false;
        self.echo.clear();
    }

    fn typed_char(&mut self, c: char) {
        if self.line.chars().count() < MAX_TEXT {
            self.line.push(c);
        }
    }

    /// What the user typed (one key, or a paste).
    pub fn input(&mut self, bytes: &[u8]) {
        let mut bytes = bytes;
        // A bracketed paste is its text.
        if let Some(inner) = bytes.strip_prefix(b"\x1b[200~") {
            bytes = inner.strip_suffix(b"\x1b[201~").unwrap_or(inner);
        } else if bytes == [0x1b] {
            self.key("escape");
            return;
        } else if bytes.first() == Some(&0x1b) {
            self.out.skipped = true;
            return;
        }
        let text = String::from_utf8_lossy(bytes);
        for c in text.chars() {
            match c {
                '\r' | '\n' => self.enter(),
                '\x7f' | '\x08' => {
                    self.line.pop();
                }
                // Ctrl+U: the line goes.
                '\x15' => self.line.clear(),
                // Ctrl+W: the word before the cursor goes.
                '\x17' => {
                    let kept = self.line.trim_end().len();
                    self.line.truncate(kept);
                    let word = self.line.rfind(' ').map_or(0, |i| i + 1);
                    self.line.truncate(word);
                }
                '\t' => self.key("tab"),
                '\x1b' => self.out.skipped = true,
                c if (c as u32) < 0x20 => {
                    let key = char::from_u32(c as u32 + 0x60).unwrap_or('?');
                    let key = match key {
                        '`' => '@',
                        '{' => '[',
                        '|' => '\\',
                        '}' => ']',
                        '~' => '^',
                        '\x7f' => '_',
                        k => k,
                    };
                    self.key(&format!("ctrl+{key}"));
                }
                c if c.is_control() => self.out.skipped = true,
                c => self.typed_char(c),
            }
        }
    }

    /// The recording, with what was typed after the last Enter as an
    /// `@type` step.
    pub fn finish(mut self) -> Recording {
        self.flush_typed();
        self.out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::login::{Step, expand, steps_bytes};

    fn typing(r: &mut Recorder, text: &str, echo: bool) {
        for c in text.chars() {
            let mut b = [0; 4];
            let s = c.encode_utf8(&mut b);
            r.input(s.as_bytes());
            if echo && c != '\r' {
                r.output(s.as_bytes());
            }
        }
    }

    #[test]
    fn lines_and_edits_are_recorded() {
        let mut r = Recorder::default();
        r.output(b"sw1#");
        typing(&mut r, "conf tx\x7f\r", true);
        typing(&mut r, "echo $HOME\r", true);
        typing(&mut r, "@x\r", true);
        typing(&mut r, "junk words\x17\x17\x15int gi0/1\r", true);
        typing(&mut r, "\r", true);
        r.input(b"\x1b[A");
        r.input(b"\x1b[200~show clock\x1b[201~");
        r.output(b"show clock");
        typing(&mut r, "\r", true);
        let rec = r.finish();
        assert_eq!(
            rec.lines,
            [
                "conf t",
                "echo $$HOME",
                "@@x",
                "int gi0/1",
                "@key enter",
                "show clock"
            ]
        );
        assert!(rec.skipped, "the arrow key was left out");
        assert_eq!(rec.hidden, 0);
    }

    #[test]
    fn special_keys_become_steps_that_replay() {
        let mut r = Recorder::default();
        typing(&mut r, "sh ip int \tbr\r", true);
        typing(&mut r, "\x1a", true);
        typing(&mut r, "\x1e", true);
        r.input(b"\x1b");
        typing(&mut r, "end", true);
        let rec = r.finish();
        assert_eq!(
            rec.lines,
            [
                "@type sh ip int",
                "@key space",
                "@key tab",
                "br",
                "@key ctrl+z",
                "@key ctrl+^",
                "@key escape",
                "@type end"
            ]
        );
        let steps = expand(&rec.lines, true, |_| String::new(), |_| String::new());
        assert_eq!(
            steps_bytes(&steps).unwrap(),
            b"sh ip int \tbr\r\x1a\x1e\x1bend"
        );
    }

    #[test]
    fn unechoed_lines_are_never_kept() {
        let mut r = Recorder::default();
        typing(&mut r, "enable\r", true);
        r.output(b"\r\nPassword: ");
        typing(&mut r, "hunter2\r", false);
        r.output(b"\r\nsw1#");
        typing(&mut r, "show run\r", true);
        let rec = r.finish();
        assert_eq!(rec.lines, ["enable", "@password", "show run"]);
        assert_eq!(rec.hidden, 1);
        assert!(!rec.lines.iter().any(|l| l.contains("hunter2")));
        let steps = expand(&rec.lines, true, |_| String::new(), |_| String::new());
        assert_eq!(steps.get(1), Some(&Step::Password));
    }
}
