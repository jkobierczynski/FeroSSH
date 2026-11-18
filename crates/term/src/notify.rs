//! Desktop notifications a host asks for with an escape sequence, which
//! coding agents and shell hooks send when a task ends or input is
//! wanted:
//!
//! - OSC 9 (iTerm2): `ESC ] 9 ; text BEL`. ConEmu's `OSC 9 ; N ; ...`
//!   commands (progress, `9;4`, and the rest) start with a number and are
//!   not notifications.
//! - OSC 777 (urxvt, Ghostty, WezTerm): `ESC ] 777 ; notify ; title ; body BEL`.
//! - OSC 99 (kitty): `ESC ] 99 ; metadata ; payload BEL`, with `p=title`
//!   or `p=body`, `d=0` for a part with more to come and `e=1` for a
//!   base64 payload.
//!
//! The parser underneath drops all three; the scanner in [`crate::cwd`]
//! finds them on the side. Their text is the host's, so it comes out
//! sanitised like a title: no control or invisible formatting characters,
//! and cut short.

use crate::sanitize::sanitize_title;

/// Longest title or body passed on, in characters.
pub const MAX_NOTIFICATION: usize = 256;

/// A notification the host asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notification {
    /// The heading, when the host gave one.
    pub title: Option<String>,
    /// The text, sanitised; may be empty when only a title came.
    pub body: String,
}

/// Collects kitty's notifications sent in parts.
#[derive(Debug, Default)]
pub(crate) struct Kitty {
    /// The parts so far of a notification with `d=0`.
    pending: Option<(String, Notification)>,
}

fn clean(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut s = sanitize_title(&text);
    if let Some((i, _)) = s.char_indices().nth(MAX_NOTIFICATION) {
        s.truncate(i);
    }
    s
}

/// The notification in OSC text `osc` (between `ESC ]` and its end), if
/// it is one.
pub(crate) fn parse(osc: &[u8], kitty: &mut Kitty) -> Option<Notification> {
    if let Some(text) = osc.strip_prefix(b"9;") {
        // ConEmu: `9;4;1;50` (progress), `9;1;100` (sleep) and the rest.
        let digits = text.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 && matches!(text.get(digits), None | Some(b';')) {
            return None;
        }
        let body = clean(text);
        return (!body.is_empty()).then_some(Notification { title: None, body });
    }
    if let Some(rest) = osc.strip_prefix(b"777;") {
        let mut parts = rest.splitn(3, |&b| b == b';');
        if parts.next()? != b"notify" {
            return None;
        }
        let title = clean(parts.next().unwrap_or_default());
        let body = clean(parts.next().unwrap_or_default());
        if title.is_empty() && body.is_empty() {
            return None;
        }
        return Some(Notification {
            title: (!title.is_empty()).then_some(title),
            body,
        });
    }
    if let Some(rest) = osc.strip_prefix(b"99;") {
        return kitty.part(rest);
    }
    None
}

impl Kitty {
    fn part(&mut self, rest: &[u8]) -> Option<Notification> {
        let (meta, payload) = match rest.iter().position(|&b| b == b';') {
            Some(i) => (rest.get(..i)?, rest.get(i + 1..)?),
            None => (rest, &[][..]),
        };
        let meta = String::from_utf8_lossy(meta);
        let (mut id, mut done, mut base64, mut body) = (String::new(), true, false, false);
        for kv in meta.split(':') {
            match kv.split_once('=') {
                Some(("i", v)) => id = v.chars().take(64).collect(),
                Some(("d", v)) => done = v != "0",
                Some(("e", v)) => base64 = v == "1",
                Some(("p", v)) => match v {
                    "title" => body = false,
                    "body" => body = true,
                    // Icons, buttons, queries: nothing to show.
                    _ => return None,
                },
                _ => {}
            }
        }
        let decoded;
        let payload = if base64 {
            decoded = decode_base64(payload)?;
            &decoded[..]
        } else {
            payload
        };
        let text = clean(payload);
        let mut n = match self.pending.take() {
            Some((pending_id, n)) if pending_id == id => n,
            _ => Notification::default(),
        };
        if body {
            if n.body.chars().count() < MAX_NOTIFICATION {
                n.body.push_str(&text);
                n.body = clean(n.body.as_bytes());
            }
        } else if !text.is_empty() {
            let mut title = n.title.take().unwrap_or_default();
            title.push_str(&text);
            n.title = Some(clean(title.as_bytes()));
        }
        if !done {
            self.pending = Some((id, n));
            return None;
        }
        (n.title.is_some() || !n.body.is_empty()).then_some(n)
    }
}

/// Standard base64, padding optional; `None` when it is not base64.
fn decode_base64(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn n(title: Option<&str>, body: &str) -> Option<Notification> {
        Some(Notification {
            title: title.map(str::to_owned),
            body: body.to_owned(),
        })
    }

    #[test]
    fn reads_iterm2_and_skips_conemu() {
        let mut k = Kitty::default();
        assert_eq!(
            parse(b"9;Claude needs your permission", &mut k),
            n(None, "Claude needs your permission")
        );
        assert_eq!(parse(b"9;4;1;50", &mut k), None);
        assert_eq!(parse(b"9;12", &mut k), None);
        assert_eq!(parse(b"9;", &mut k), None);
        // A number that is the message itself, not a ConEmu command.
        assert_eq!(
            parse(b"9;3 tests failed", &mut k),
            n(None, "3 tests failed")
        );
    }

    #[test]
    fn reads_osc777() {
        let mut k = Kitty::default();
        assert_eq!(
            parse(b"777;notify;Codex;Task done; all green", &mut k),
            n(Some("Codex"), "Task done; all green")
        );
        assert_eq!(parse(b"777;notify;;", &mut k), None);
        assert_eq!(parse(b"777;preexec", &mut k), None);
    }

    #[test]
    fn reads_kitty_in_parts_and_base64() {
        let mut k = Kitty::default();
        assert_eq!(parse(b"99;;Hello", &mut k), n(Some("Hello"), ""));
        assert_eq!(parse(b"99;i=1:d=0;Build", &mut k), None);
        assert_eq!(
            parse(b"99;i=1:p=body;finished", &mut k),
            n(Some("Build"), "finished")
        );
        // "Done" in base64.
        assert_eq!(parse(b"99;e=1:p=body;RG9uZQ==", &mut k), n(None, "Done"));
        assert_eq!(parse(b"99;e=1;!!", &mut k), None);
        assert_eq!(parse(b"99;p=icon;abc", &mut k), None);
    }

    #[test]
    fn text_is_sanitised_and_cut() {
        let mut k = Kitty::default();
        let got = parse(b"9;a\x08b\xe2\x80\xaec", &mut k).unwrap();
        assert_eq!(got.body, "abc");
        let long = [b"9;".as_slice(), &[b'x'; 1000]].concat();
        assert_eq!(parse(&long, &mut k).unwrap().body.len(), MAX_NOTIFICATION);
    }
}
