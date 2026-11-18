//! Desktop notifications a host asks for with an escape sequence, which
//! coding agents and shell hooks send when a task ends or input is
//! wanted:
//!
//! - OSC 9 (iTerm2): `ESC ] 9 ; text BEL`. ConEmu's commands, `OSC 9 ; N`
//!   or `OSC 9 ; N ; ...` with N from 1 to 12 (progress is `9;4`), are not
//!   notifications.
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

/// Notifications sent in parts kept at once; the oldest goes for a new
/// one.
const MAX_PENDING: usize = 4;
/// Bytes kept of a title or body sent in parts, before decoding.
const MAX_PART_BYTES: usize = 4 * MAX_NOTIFICATION * 4;

/// Collects kitty's notifications sent in parts.
#[derive(Debug, Default)]
pub(crate) struct Kitty {
    /// The notifications with parts still to come (`d=0`), by id, oldest
    /// first.
    pending: Vec<(String, Parts)>,
}

/// A kitty notification's title and body so far.
#[derive(Debug, Default)]
struct Parts {
    title: Text,
    body: Text,
}

/// Text sent in parts: what is decoded, and base64 not decoded yet (a
/// chunk need not end on a whole group of four).
#[derive(Debug, Default)]
struct Text {
    plain: Vec<u8>,
    base64: Vec<u8>,
    /// Base64 characters of the current encoding, all of them (the ones
    /// past the cap too), modulo four: where its padding has to fall.
    group: usize,
}

impl Text {
    /// Adds a chunk; `None` when its base64 is not.
    fn push(&mut self, chunk: &[u8], base64: bool) -> Option<()> {
        if base64 {
            // Padding only at the end, at most two.
            let data = chunk.iter().take_while(|&&c| c != b'=').count();
            let pad = chunk.len() - data;
            if pad > 2
                || chunk
                    .get(data..)
                    .is_some_and(|p| p.iter().any(|&c| c != b'='))
                || !chunk
                    .iter()
                    .take(data)
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/'))
            {
                return None;
            }
            // Padding fills the last group of four exactly.
            if pad > 0 && (self.group + data) % 4 + pad != 4 {
                return None;
            }
            self.group = (self.group + data) % 4;
            let room = MAX_PART_BYTES.saturating_sub(self.base64.len());
            self.base64
                .extend(chunk.iter().filter(|&&c| c != b'=').take(room));
            // Padding ends a whole encoding: what follows is a new one.
            if chunk.ends_with(b"=") {
                self.flush()?;
            }
        } else {
            self.flush()?;
            let room = MAX_PART_BYTES.saturating_sub(self.plain.len());
            self.plain.extend(chunk.iter().take(room));
        }
        Some(())
    }

    /// Decodes the base64 kept so far.
    fn flush(&mut self) -> Option<()> {
        self.group = 0;
        if !self.base64.is_empty() {
            let decoded = decode_base64(&self.base64)?;
            self.base64.clear();
            let room = MAX_PART_BYTES.saturating_sub(self.plain.len());
            self.plain.extend(decoded.into_iter().take(room));
        }
        Some(())
    }

    fn finish(mut self) -> String {
        if self.flush().is_none() {
            return String::new();
        }
        clean(&self.plain)
    }
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
        let command = std::str::from_utf8(text.get(..digits)?)
            .ok()
            .and_then(|n| n.parse::<u32>().ok());
        if command.is_some_and(|n| (1..=12).contains(&n))
            && matches!(text.get(digits), None | Some(b';'))
        {
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
        let (mut id, mut done, mut base64) = (String::new(), true, false);
        // `None`: a part with nothing to show (icon, buttons, a query).
        let mut body = Some(false);
        for kv in meta.split(':') {
            match kv.split_once('=') {
                Some(("i", v)) => id = v.to_owned(),
                Some(("d", v)) => done = v != "0",
                Some(("e", v)) => base64 = v == "1",
                Some(("p", v)) => {
                    body = match v {
                        "title" => Some(false),
                        "body" => Some(true),
                        // Parts of a notification with nothing to show
                        // here; the last one still ends it.
                        "icon" | "buttons" => None,
                        // Queries and requests (`?`, `alive`, `close`):
                        // not part of any notification.
                        _ => return None,
                    }
                }
                _ => {}
            }
        }
        let at = self.pending.iter().position(|(p, _)| *p == id);
        let mut parts = match at {
            Some(i) => self.pending.remove(i).1,
            // A part with nothing to show starts nothing.
            None if body.is_none() => return None,
            None => Parts::default(),
        };
        if let Some(body) = body {
            let text = if body {
                &mut parts.body
            } else {
                &mut parts.title
            };
            // Bad base64 drops the whole notification.
            text.push(payload, base64)?;
        }
        if !done {
            if self.pending.len() >= MAX_PENDING {
                self.pending.remove(0);
            }
            self.pending.push((id, parts));
            return None;
        }
        let title = parts.title.finish();
        let body = parts.body.finish();
        (!title.is_empty() || !body.is_empty()).then(|| Notification {
            title: (!title.is_empty()).then_some(title),
            body,
        })
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
        // Not a ConEmu command: the number is the message.
        assert_eq!(parse(b"9;42", &mut k), n(None, "42"));
        assert_eq!(parse(b"9;0", &mut k), n(None, "0"));
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
    fn kitty_parts_keep_their_spaces_and_ids() {
        let mut k = Kitty::default();
        assert_eq!(parse(b"99;i=a:d=0;Build ", &mut k), None);
        assert_eq!(parse(b"99;i=b:d=0;Deploy", &mut k), None);
        assert_eq!(parse(b"99;i=a:d=0;finished", &mut k), None);
        assert_eq!(parse(b"99;i=b:p=body;ok", &mut k), n(Some("Deploy"), "ok"));
        // An icon part ends a notification too.
        assert_eq!(
            parse(b"99;i=a:p=icon;abc", &mut k),
            n(Some("Build finished"), "")
        );
        // An icon alone is nothing.
        assert_eq!(parse(b"99;i=c:p=icon;abc", &mut k), None);
        // A query does not end a notification in parts.
        assert_eq!(parse(b"99;i=q:d=0;Pending", &mut k), None);
        assert_eq!(parse(b"99;i=q:p=?;", &mut k), None);
        assert_eq!(parse(b"99;i=q:p=alive;", &mut k), None);
        assert_eq!(
            parse(b"99;i=q:p=body;now", &mut k),
            n(Some("Pending"), "now")
        );
        // Ids are told apart in full.
        let long = "x".repeat(70);
        let a = format!("99;i={long}a:d=0;First");
        let b = format!("99;i={long}b;Second");
        assert_eq!(parse(a.as_bytes(), &mut k), None);
        assert_eq!(parse(b.as_bytes(), &mut k), n(Some("Second"), ""));
    }

    #[test]
    fn kitty_base64_may_split_anywhere() {
        let mut k = Kitty::default();
        // "Hello there" is SGVsbG8gdGhlcmU=, cut mid-group.
        assert_eq!(parse(b"99;i=x:d=0:e=1;SGVsb", &mut k), None);
        assert_eq!(
            parse(b"99;i=x:e=1;G8gdGhlcmU=", &mut k),
            n(Some("Hello there"), "")
        );
        // Chunks each with their own padding: "Hi" then "!".
        assert_eq!(parse(b"99;i=y:d=0:e=1;SGk=", &mut k), None);
        assert_eq!(parse(b"99;i=y:e=1;IQ==", &mut k), n(Some("Hi!"), ""));
        // Padding inside a chunk is not base64.
        assert_eq!(parse(b"99;i=z:e=1;SG=k=", &mut k), None);
        assert_eq!(parse(b"99;i=z:e=1;SGk===", &mut k), None);
        // One `=` too many for its group.
        assert_eq!(parse(b"99;i=z:e=1;SGk==", &mut k), None);
    }

    #[test]
    fn kitty_padding_counts_base64_past_the_cap() {
        // The cap keeps a whole number of groups; the one extra `A` past
        // it still decides where the padding falls.
        let mut t = Text::default();
        let full = vec![b'Q'; MAX_PART_BYTES + 1];
        assert_eq!(t.push(&full, true), Some(()));
        assert_eq!(t.base64.len(), MAX_PART_BYTES);
        assert_eq!(t.push(b"AA=", true), Some(()));
        assert!(t.base64.is_empty());
        // And a stray `=` is still refused after it.
        let mut t = Text::default();
        assert_eq!(t.push(&full, true), Some(()));
        assert_eq!(t.push(b"AA==", true), None);
    }

    #[test]
    fn many_pending_kitty_parts_stay_bounded() {
        let mut k = Kitty::default();
        for i in 0..100 {
            let osc = format!("99;i={i}:d=0;{}", "x".repeat(5000));
            assert_eq!(parse(osc.as_bytes(), &mut k), None);
        }
        assert_eq!(k.pending.len(), MAX_PENDING);
        assert!(
            k.pending
                .iter()
                .all(|(_, p)| p.title.plain.len() <= MAX_PART_BYTES)
        );
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
