//! Session logs: what the host sent, written to a file per tab.
//!
//! Plain logs keep the text a reader would see: escape sequences are
//! parsed and dropped, carriage returns and backspaces overwrite as on
//! screen (so progress bars end up as their last state), and lines end in
//! CRLF for Notepad. Raw logs keep every byte, for replaying with `cat`.
//! The UI process writes the file; the worker never sees it. A log opened
//! with a chain file is also sealed against later edits (see `chain`).

use std::io::{self, Write};

use alacritty_terminal::vte::{Params, Parser, Perform};

use crate::chain::Chained;

/// Tab stops in plain logs, as on a terminal.
const TAB: usize = 8;
/// A line longer than this is written out in pieces, so a host that never
/// sends a newline cannot grow memory.
const MAX_LINE: usize = 64 * 1024;

#[derive(Default)]
struct Line {
    chars: Vec<char>,
    col: usize,
    done: Vec<String>,
}

impl Line {
    fn put(&mut self, c: char) {
        match self.chars.get_mut(self.col) {
            Some(slot) => *slot = c,
            None => {
                self.chars.resize(self.col, ' ');
                self.chars.push(c);
            }
        }
        self.col += 1;
        if self.chars.len() >= MAX_LINE {
            self.end();
        }
    }

    fn end(&mut self) {
        let line: String = self.chars.drain(..).collect();
        self.done.push(line.trim_end().to_owned());
        self.col = 0;
    }
}

impl Perform for Line {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.end(),
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => {
                let next = (self.col / TAB + 1) * TAB;
                while self.col < next {
                    self.put(' ');
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], _ignore: bool, c: char) {
        let first = params
            .iter()
            .next()
            .and_then(|p| p.first().copied())
            .unwrap_or(0);
        match c {
            // Erase in line: to the end (0) or the whole line (2).
            'K' if first == 0 => self.chars.truncate(self.col),
            'K' if first == 2 => self.chars.clear(),
            // Cursor forward and back within the line.
            'C' => self.col += usize::from(first.max(1)),
            'D' => self.col = self.col.saturating_sub(usize::from(first.max(1))),
            _ => {}
        }
    }
}

/// Where a log's bytes go: the file alone, or the file and its chain.
enum Sink<W: Write> {
    Plain(W),
    Chained(Box<Chained<W>>),
}

impl<W: Write> Write for Sink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Chained(c) => c.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Chained(c) => c.flush(),
        }
    }
}

/// How a sealed log ended: its length and last seal, for the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub bytes: u64,
    pub head: String,
}

/// Writes one tab's log.
pub struct LogWriter<W: Write> {
    out: Sink<W>,
    raw: bool,
    parser: Parser,
    line: Line,
}

impl<W: Write> std::fmt::Debug for LogWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogWriter")
            .field("raw", &self.raw)
            .finish_non_exhaustive()
    }
}

impl<W: Write> LogWriter<W> {
    /// `header` (for example the host and start time) opens the log.
    pub fn new(out: W, raw: bool, header: &str) -> io::Result<Self> {
        Self::open(Sink::Plain(out), raw, header)
    }

    /// A log sealed in `chain` as it is written; `file_name` is the log's
    /// own file name, which the chain starts from.
    pub fn sealed(out: W, chain: W, file_name: &str, raw: bool, header: &str) -> io::Result<Self> {
        let chained = Chained::new(out, chain, file_name)?;
        Self::open(Sink::Chained(Box::new(chained)), raw, header)
    }

    fn open(mut out: Sink<W>, raw: bool, header: &str) -> io::Result<Self> {
        write!(out, "{header}\r\n")?;
        out.flush()?;
        Ok(Self {
            out,
            raw,
            parser: Parser::new(),
            line: Line::default(),
        })
    }

    pub fn is_raw(&self) -> bool {
        self.raw
    }

    /// Logs bytes the host sent. The file is flushed every time, so a
    /// crash loses nothing that was on screen.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.raw {
            self.out.write_all(bytes)?;
        } else {
            self.parser.advance(&mut self.line, bytes);
            for line in self.line.done.drain(..) {
                write!(self.out, "{line}\r\n")?;
            }
        }
        self.out.flush()?;
        if let Sink::Chained(c) = &mut self.out {
            c.maybe_seal()?;
        }
        Ok(())
    }

    /// Writes the unfinished last line and a closing note.
    pub fn finish(self, footer: &str) -> io::Result<W> {
        match self.close(footer)? {
            Sink::Plain(w) => Ok(w),
            Sink::Chained(c) => c.end().map(|(w, ..)| w),
        }
    }

    /// Like `finish`, and seals the end; `None` for a log without a chain.
    pub fn finish_sealed(self, footer: &str) -> io::Result<Option<Sealed>> {
        match self.close(footer)? {
            Sink::Plain(_) => Ok(None),
            Sink::Chained(c) => {
                let (_, _, bytes, head) = c.end()?;
                Ok(Some(Sealed { bytes, head }))
            }
        }
    }

    fn close(mut self, footer: &str) -> io::Result<Sink<W>> {
        if !self.raw && !self.line.chars.is_empty() {
            self.line.end();
            for line in self.line.done.drain(..) {
                write!(self.out, "{line}\r\n")?;
            }
        }
        if self.raw {
            write!(self.out, "\r\n")?;
        }
        write!(self.out, "{footer}\r\n")?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// A file name for a tab's log, from the host, the tab number and the
/// local time (`year, month, day, hour, minute, second`), such as
/// `example.org_tab1_2026-10-24_2215.log`. Characters Windows does not
/// allow in file names are replaced.
pub fn log_file_name(host: &str, tab: usize, time: (u16, u8, u8, u8, u8, u8)) -> String {
    let host: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let (y, mo, d, h, mi, _) = time;
    format!("{host}_tab{tab}_{y:04}-{mo:02}-{d:02}_{h:02}{mi:02}.log")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn plain(chunks: &[&[u8]]) -> String {
        let mut w = LogWriter::new(Vec::new(), false, "start").unwrap();
        for c in chunks {
            w.write(c).unwrap();
        }
        String::from_utf8(w.finish("end").unwrap()).unwrap()
    }

    #[test]
    fn plain_logs_drop_escapes_and_keep_text() {
        let got = plain(&[
            b"\x1b[1;31mred\x1b[0m text\r\n",
            b"\x1b]0;title\x07prompt$ ls\r\n",
            b"a\tb\r\n",
            b"unfinished",
        ]);
        assert_eq!(
            got,
            "start\r\nred text\r\nprompt$ ls\r\na       b\r\nunfinished\r\nend\r\n"
        );
    }

    #[test]
    fn carriage_returns_and_backspaces_overwrite() {
        let got = plain(&[
            b" 10%\r 50%\r100%\r\n",
            b"lss\x08 \x08\r\n",
            b"long line\r\x1b[Kshort\n",
        ]);
        assert_eq!(got, "start\r\n100%\r\nls\r\nshort\r\nend\r\n");
    }

    #[test]
    fn utf8_split_across_writes() {
        let got = plain(&[
            "é".as_bytes().get(..1).unwrap(),
            "é\n".as_bytes().get(1..).unwrap(),
        ]);
        assert_eq!(got, "start\r\né\r\nend\r\n");
    }

    #[test]
    fn raw_logs_keep_bytes() {
        let mut w = LogWriter::new(Vec::new(), true, "s").unwrap();
        w.write(b"\x1b[31mx").unwrap();
        assert_eq!(w.finish("e").unwrap(), b"s\r\n\x1b[31mx\r\ne\r\n");
    }

    #[test]
    fn sealed_logs_verify_including_header_and_footer() {
        let mut w = LogWriter::sealed(Vec::new(), Vec::new(), "x.log", false, "start").unwrap();
        w.write(b"\x1b[1mhello\x1b[0m\r\nworld").unwrap();
        let (out, chain) = match w.close("end").unwrap() {
            Sink::Chained(c) => {
                let (log, chain, ..) = c.end().unwrap();
                (log, String::from_utf8(chain).unwrap())
            }
            Sink::Plain(_) => unreachable!(),
        };
        assert_eq!(out, b"start\r\nhello\r\nworld\r\nend\r\n");
        let got = crate::chain::verify("x.log", &out, &chain).unwrap();
        assert!(got.ended);
        assert_eq!(got.sealed, out.len() as u64);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(
            log_file_name("fe80::1%eth0", 2, (2026, 10, 4, 9, 5, 0)),
            "fe80__1_eth0_tab2_2026-10-04_0905.log"
        );
    }
}
