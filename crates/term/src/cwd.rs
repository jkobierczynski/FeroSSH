//! OSC 7, the working directory a shell reports (`ESC ] 7 ; file://host/path
//! BEL`). The parser underneath drops it, so the host's output is watched
//! for it on the side; the side pane's file view follows it. The same
//! watch finds the notifications of [`crate::notify`].

use crate::notify::{self, Kitty, Notification};

/// Longest OSC text kept; longer ones are ignored.
const MAX_LEN: usize = 4096;
/// Room for a kitty notification's `99;metadata;` on top of its payload,
/// which may be [`MAX_LEN`] bytes on its own.
const MAX_KITTY_META: usize = 1024;

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

/// Finds OSC 7 and notifications in a byte stream fed in arbitrary
/// pieces.
#[derive(Debug, Default)]
pub(crate) struct CwdScanner {
    state: State,
    buf: Vec<u8>,
    kitty: Kitty,
}

/// What [`CwdScanner::scan`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Dir(WorkingDir),
    Notify(Notification),
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
    /// Scans `bytes`; returns the directories and notifications reported
    /// in them, in order.
    pub(crate) fn scan(&mut self, bytes: &[u8]) -> Vec<Found> {
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
                    found.extend(self.found());
                    State::Ground
                }
                (State::Osc, 0x1b) => State::OscEsc,
                (State::Osc, _) if self.buf.len() >= self.limit() => State::Skip,
                (State::Osc, _) => {
                    self.buf.push(b);
                    State::Osc
                }
                (State::OscEsc, b'\\') => {
                    found.extend(self.found());
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

impl CwdScanner {
    /// The longest the OSC being collected may grow.
    fn limit(&self) -> usize {
        if self.buf.starts_with(b"99;") {
            MAX_LEN + MAX_KITTY_META
        } else {
            MAX_LEN
        }
    }

    fn found(&mut self) -> Option<Found> {
        if let Some(dir) = parse(&self.buf) {
            return Some(Found::Dir(dir));
        }
        notify::parse(&self.buf, &mut self.kitty).map(Found::Notify)
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

/// Reads the directory from a `user@host:dir$ ` prompt (bash's default
/// on Debian and Ubuntu, `\u@\h:\w\$ `), for shells that do not report it
/// with OSC 7. `line` is the text before the cursor, so a prompt with a
/// command being typed after it, or a line of output, gives `None`. The
/// prompt may start with a `(chroot)` or `(venv)` tag. `~` and `~/…` come
/// back as they are, for the caller to resolve against the home folder.
pub fn prompt_dir(line: &str) -> Option<WorkingDir> {
    let body = line
        .strip_suffix("$ ")
        .or_else(|| line.strip_suffix("# "))?;
    // Tags like `(venv) ` or Debian's `(chroot)` before the user name.
    let mut body = body;
    while let Some(tag) = body.strip_prefix('(') {
        body = tag.split_once(')')?.1.trim_start();
    }
    let (user, rest) = body.split_once('@')?;
    let (host, path) = rest.split_once(':')?;
    let name = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if !name(user) || !name(host) {
        return None;
    }
    let tilde = path == "~" || path.starts_with("~/");
    if !(tilde || path.starts_with('/')) || path.chars().any(char::is_control) {
        return None;
    }
    // Trailing slashes of `~/dir/` are not bash's, but harmless.
    Some(WorkingDir {
        host: host.to_owned(),
        path: path.as_bytes().to_vec(),
    })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn reads_debian_prompts() {
        let d = |line: &str| prompt_dir(line).map(|w| (w.host, w.path));
        let got = |host: &str, path: &str| Some((host.to_owned(), path.as_bytes().to_vec()));
        assert_eq!(
            d("user@Debian:~/Downloads/routershift$ "),
            got("Debian", "~/Downloads/routershift")
        );
        assert_eq!(d("user@Debian:~$ "), got("Debian", "~"));
        assert_eq!(d("root@web-1:/etc/nginx# "), got("web-1", "/etc/nginx"));
        assert_eq!(d("(venv) me@box:/srv/my dir$ "), got("box", "/srv/my dir"));
        assert_eq!(d("(chroot)me@box:/$ "), got("box", "/"));
        // A command being typed, output, other prompt styles.
        assert_eq!(d("user@Debian:~$ ls"), None);
        assert_eq!(d("total 12"), None);
        assert_eq!(d("[user@rhel ~]$ "), None);
        assert_eq!(d("user@Debian:relative$ "), None);
        assert_eq!(d("a b@host:/x$ "), None);
        assert_eq!(d(""), None);
    }

    fn dir(host: &str, path: &[u8]) -> Found {
        Found::Dir(WorkingDir {
            host: host.into(),
            path: path.to_vec(),
        })
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

    #[test]
    fn a_full_size_kitty_payload_fits() {
        let mut s = CwdScanner::default();
        let osc = [&b"\x1b]99;i=1:e=1;"[..], &[b'Q'; MAX_LEN], b"\x07"].concat();
        assert_eq!(s.scan(&osc).len(), 1);
        // A long header (an application name) does not cost payload: the
        // whole text arrives. "QUFB" is "AAA".
        let header = format!("\x1b]99;i=2:e=1:f={}:t=build;", "QUFB".repeat(150));
        let body = b"QUFB".repeat(MAX_LEN / 4);
        let osc = [header.as_bytes(), &body, b"\x07"].concat();
        match s.scan(&osc).as_slice() {
            [Found::Notify(n)] => {
                let title = n.title.as_deref().unwrap_or_default();
                assert!(
                    title.len() > 100 && title.chars().all(|c| c == 'A'),
                    "{n:?}"
                );
            }
            other => panic!("{other:?}"),
        }
        // OSC 7 keeps its own limit.
        let long = [&b"\x1b]7;file://h/"[..], &[b'a'; MAX_LEN], b"\x07"].concat();
        assert_eq!(s.scan(&long), []);
    }

    #[test]
    fn finds_notifications_between_directories() {
        let mut s = CwdScanner::default();
        let mut got = Vec::new();
        for b in b"\x1b]7;file://h/a\x07\x1b]9;Waiting for you\x1b\\\x1b]9;4;1;5\x07\x07" {
            got.extend(s.scan(&[*b]));
        }
        assert_eq!(
            got,
            [
                dir("h", b"/a"),
                Found::Notify(Notification {
                    title: None,
                    body: "Waiting for you".into()
                })
            ]
        );
    }
}
