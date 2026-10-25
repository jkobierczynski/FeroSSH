//! OSC 7, the working directory a shell reports (`ESC ] 7 ; file://host/path
//! BEL`). The parser underneath drops it, so the host's output is watched
//! for it on the side; the side pane's file view follows it.

/// Longest OSC 7 text kept; longer ones are ignored.
const MAX_LEN: usize = 4096;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    /// Inside an OSC, collecting it.
    Osc,
    /// ESC inside an OSC: `\` ends it.
    OscEsc,
    /// Inside an OSC too long to be ours: skipped to its end.
    Skip,
    SkipEsc,
}

/// Finds OSC 7 in a byte stream fed in arbitrary pieces.
#[derive(Debug, Default)]
pub(crate) struct CwdScanner {
    state: State,
    buf: Vec<u8>,
}

/// A reported working directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingDir {
    /// The host name the shell gave, as is; it tells a nested `ssh`
    /// session's directory from this connection's.
    pub host: String,
    /// The path, percent-decoded. Not necessarily UTF-8.
    pub path: Vec<u8>,
}

impl CwdScanner {
    /// Scans `bytes`; returns the directories reported in them, in order.
    pub(crate) fn scan(&mut self, bytes: &[u8]) -> Vec<WorkingDir> {
        let mut found = Vec::new();
        for &b in bytes {
            self.state = match (self.state, b) {
                // CAN and SUB abort any sequence.
                (_, 0x18 | 0x1a) => State::Ground,
                (State::Ground, 0x1b) => State::Esc,
                (State::Ground, _) => State::Ground,
                (State::Esc, b']') => {
                    self.buf.clear();
                    State::Osc
                }
                (State::Esc, 0x1b) => State::Esc,
                (State::Esc, _) => State::Ground,
                (State::Osc, 0x07) => {
                    found.extend(parse(&self.buf));
                    State::Ground
                }
                (State::Osc, 0x1b) => State::OscEsc,
                (State::Osc, _) if self.buf.len() >= MAX_LEN => State::Skip,
                (State::Osc, _) => {
                    self.buf.push(b);
                    State::Osc
                }
                (State::OscEsc, b'\\') => {
                    found.extend(parse(&self.buf));
                    State::Ground
                }
                // An ESC that starts something else ends the OSC unfinished.
                (State::OscEsc, b']') => {
                    self.buf.clear();
                    State::Osc
                }
                (State::OscEsc | State::SkipEsc, _) => State::Ground,
                (State::Skip, 0x07) => State::Ground,
                (State::Skip, 0x1b) => State::SkipEsc,
                (State::Skip, _) => State::Skip,
            };
        }
        if self.state == State::Ground {
            self.buf.clear();
        }
        found
    }
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `7;file://host/path`, percent-decoded; anything else is `None`.
fn parse(osc: &[u8]) -> Option<WorkingDir> {
    let url = osc.strip_prefix(b"7;")?;
    let rest = url.strip_prefix(b"file://")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    let (host, path) = rest.split_at(slash);
    let host = std::str::from_utf8(host).ok()?;
    if host.chars().any(char::is_control) {
        return None;
    }
    let mut out = Vec::with_capacity(path.len());
    let mut it = path.iter();
    while let Some(&b) = it.next() {
        if b == b'%' {
            let hi = hex(*it.next()?)?;
            let lo = hex(*it.next()?)?;
            out.push(hi << 4 | lo);
        } else {
            out.push(b);
        }
    }
    if out.contains(&0) {
        return None;
    }
    Some(WorkingDir {
        host: host.to_owned(),
        path: out,
    })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn dir(host: &str, path: &[u8]) -> WorkingDir {
        WorkingDir {
            host: host.into(),
            path: path.to_vec(),
        }
    }

    #[test]
    fn finds_osc7_with_either_terminator() {
        let mut s = CwdScanner::default();
        let got = s.scan(b"ls\r\n\x1b]7;file://web/home/j%20k\x07$ \x1b]7;file:///tmp\x1b\\");
        assert_eq!(got, [dir("web", b"/home/j k"), dir("", b"/tmp")]);
    }

    #[test]
    fn survives_splits_and_ignores_other_oscs() {
        let mut s = CwdScanner::default();
        let stream = b"\x1b]0;title\x07\x1b]7;file://h/a%C3%A9\x07";
        let mut got = Vec::new();
        for b in stream {
            got.extend(s.scan(&[*b]));
        }
        assert_eq!(got, [dir("h", "/aé".as_bytes())]);
    }

    #[test]
    fn rejects_what_is_not_a_file_url() {
        let mut s = CwdScanner::default();
        for bad in [
            &b"\x1b]7;http://h/x\x07"[..],
            b"\x1b]7;file://h\x07",
            b"\x1b]7;file://h/%zz\x07",
            b"\x1b]7;file://h/a%00b\x07",
            b"\x1b]7;file://h/a\x18\x07",
        ] {
            assert_eq!(s.scan(bad), [], "{bad:?}");
        }
        let long = [&b"\x1b]7;file://h/"[..], &[b'a'; 5000], b"\x07"].concat();
        assert_eq!(s.scan(&long), []);
        // And it still works afterwards.
        assert_eq!(s.scan(b"\x1b]7;file://h/ok\x07"), [dir("h", b"/ok")]);
    }
}
