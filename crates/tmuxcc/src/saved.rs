//! The layout FeroSSH keeps in the tmux session, in the user option
//! `@ferossh-layout`, so that attaching from any PC brings back the same
//! tabs, splits and per-window looks.
//!
//! One line of space-separated words:
//!
//! ```text
//! ferossh1 active=0 tab=r500(@1,c400(@2,@3))/@2 tab=@4/@4 look=@1,Cascadia%20Mono,12,Nord
//! ```
//!
//! A tab is a split tree of tmux windows (`r` side by side, `c` one above
//! the other, with the first child's share in thousandths) and its
//! focused window. A look is a window's font, text size and colour theme,
//! each percent-encoded and empty when unset. The text is only ever ASCII
//! letters, digits and `%(),./=@`, so it needs no escaping inside tmux's
//! single quotes. The session's other clients may have written it, so it
//! is read as untrusted input.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::WindowId;

/// The option's name in the tmux session.
pub const OPTION: &str = "@ferossh-layout";
/// Which layout mode the session uses: `ferossh` or `tmux`.
pub const MODE_OPTION: &str = "@ferossh-mode";

const MAGIC: &str = "ferossh1";
const MAX_TEXT: usize = 64 << 10;
const MAX_TABS: usize = 256;
const MAX_LOOKS: usize = 512;
const MAX_DEPTH: usize = 32;
const MAX_NAME: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Window(WindowId),
    Split {
        /// Side by side; otherwise one above the other.
        row: bool,
        /// The first child's share, in thousandths (100 to 900).
        ratio: u16,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    /// The windows, first to last.
    pub fn windows(&self) -> Vec<WindowId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<WindowId>) {
        match self {
            Node::Window(w) => out.push(*w),
            Node::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    /// The tree with only the windows `keep` accepts; `None` if none is
    /// left.
    pub fn retain(self, keep: &mut impl FnMut(WindowId) -> bool) -> Option<Node> {
        match self {
            Node::Window(w) => keep(w).then_some(Node::Window(w)),
            Node::Split {
                row,
                ratio,
                first,
                second,
            } => match (first.retain(keep), second.retain(keep)) {
                (Some(a), Some(b)) => Some(Node::Split {
                    row,
                    ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Node::Window(w) => {
                let _ = write!(out, "@{w}");
            }
            Node::Split {
                row,
                ratio,
                first,
                second,
            } => {
                let _ = write!(out, "{}{ratio}(", if *row { 'r' } else { 'c' });
                first.write(out);
                out.push(',');
                second.write(out);
                out.push(')');
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub root: Node,
    pub focus: WindowId,
}

/// A window's font, text size and colour theme.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Look {
    pub font: Option<String>,
    pub size: Option<f32>,
    pub theme: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub looks: BTreeMap<WindowId, Look>,
}

fn encode(text: &str, out: &mut String) {
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    let text = String::from_utf8(out).ok()?;
    // Names only: no control characters, and short.
    (!text.chars().any(char::is_control) && text.chars().count() <= MAX_NAME).then_some(text)
}

impl Saved {
    /// The option's text.
    pub fn to_text(&self) -> String {
        let mut out = format!("{MAGIC} active={}", self.active);
        for tab in &self.tabs {
            out.push_str(" tab=");
            tab.root.write(&mut out);
            let _ = write!(out, "/@{}", tab.focus);
        }
        for (w, look) in &self.looks {
            let _ = write!(out, " look=@{w},");
            if let Some(f) = &look.font {
                encode(f, &mut out);
            }
            out.push(',');
            if let Some(s) = look.size {
                let _ = write!(out, "{s}");
            }
            out.push(',');
            if let Some(t) = &look.theme {
                encode(t, &mut out);
            }
        }
        out
    }

    /// Reads the option's text; `None` if it is not FeroSSH's or is
    /// damaged. Words FeroSSH does not know are skipped, for later
    /// versions.
    pub fn parse(text: &str) -> Option<Saved> {
        if text.len() > MAX_TEXT {
            return None;
        }
        let mut words = text.split_ascii_whitespace();
        if words.next()? != MAGIC {
            return None;
        }
        let mut saved = Saved::default();
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            match key {
                "active" => saved.active = value.parse().ok()?,
                "tab" if saved.tabs.len() < MAX_TABS => {
                    let (tree, focus) = value.rsplit_once('/')?;
                    let mut p = TreeParser {
                        s: tree.as_bytes(),
                        at: 0,
                    };
                    let root = p.node(0)?;
                    if p.at != tree.len() {
                        return None;
                    }
                    let focus = window(focus)?;
                    saved.tabs.push(Tab { root, focus });
                }
                "look" if saved.looks.len() < MAX_LOOKS => {
                    let mut parts = value.split(',');
                    let w = window(parts.next()?)?;
                    let font = parts.next()?;
                    let size = parts.next()?;
                    let theme = parts.next()?;
                    let look = Look {
                        font: (!font.is_empty()).then(|| decode(font)).flatten(),
                        size: size
                            .parse::<f32>()
                            .ok()
                            .filter(|s| (4.0..=96.0).contains(s)),
                        theme: (!theme.is_empty()).then(|| decode(theme)).flatten(),
                    };
                    saved.looks.insert(w, look);
                }
                _ => {}
            }
        }
        Some(saved)
    }
}

fn window(text: &str) -> Option<WindowId> {
    text.strip_prefix('@')?.parse().ok()
}

struct TreeParser<'a> {
    s: &'a [u8],
    at: usize,
}

impl TreeParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        (self.peek()? == b).then(|| self.at += 1)
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }
        let digits = self.s.get(start..self.at)?;
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    }

    fn node(&mut self, depth: usize) -> Option<Node> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.peek()? {
            b'@' => {
                self.at += 1;
                Some(Node::Window(self.number()?))
            }
            kind @ (b'r' | b'c') => {
                self.at += 1;
                let ratio = u16::try_from(self.number()?).ok()?.clamp(100, 900);
                self.eat(b'(')?;
                let first = self.node(depth + 1)?;
                self.eat(b',')?;
                let second = self.node(depth + 1)?;
                self.eat(b')')?;
                Some(Node::Split {
                    row: kind == b'r',
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn sample() -> Saved {
        let mut looks = BTreeMap::new();
        looks.insert(
            1,
            Look {
                font: Some("Cascadia Mono".into()),
                size: Some(12.5),
                theme: Some("Rosé Pine".into()),
            },
        );
        looks.insert(4, Look::default());
        Saved {
            tabs: vec![
                Tab {
                    root: Node::Split {
                        row: true,
                        ratio: 500,
                        first: Box::new(Node::Window(1)),
                        second: Box::new(Node::Split {
                            row: false,
                            ratio: 400,
                            first: Box::new(Node::Window(2)),
                            second: Box::new(Node::Window(3)),
                        }),
                    },
                    focus: 2,
                },
                Tab {
                    root: Node::Window(4),
                    focus: 4,
                },
            ],
            active: 1,
            looks,
        }
    }

    #[test]
    fn round_trips() {
        let s = sample();
        let text = s.to_text();
        assert_eq!(
            text,
            "ferossh1 active=1 tab=r500(@1,c400(@2,@3))/@2 tab=@4/@4 \
             look=@1,Cascadia%20Mono,12.5,Ros%C3%A9%20Pine look=@4,,,"
        );
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" %(),./=@-_".contains(&b))
        );
        assert_eq!(Saved::parse(&text), Some(s));
        assert_eq!(sample().tabs[0].root.windows(), [1, 2, 3]);
    }

    #[test]
    fn refuses_damage() {
        for bad in [
            "",
            "tmux",
            "ferossh2 tab=@1/@1",
            "ferossh1 tab=@1",
            "ferossh1 tab=r500(@1)/@1",
            "ferossh1 tab=x/@1",
            "ferossh1 tab=@1junk/@1",
            "ferossh1 active=x",
            "ferossh1 look=@1,a",
        ] {
            assert_eq!(Saved::parse(bad), None, "{bad}");
        }
        let deep = format!(
            "ferossh1 tab={}@1{}/@1",
            "r500(@1,".repeat(40),
            ")".repeat(40)
        );
        assert_eq!(Saved::parse(&deep), None);
    }

    #[test]
    fn keeps_names_tame() {
        let s = Saved::parse("ferossh1 look=@1,a%1Bb,200,%0A future=1 look=@2,%zz,,").unwrap();
        assert_eq!(s.looks[&1], Look::default());
        assert_eq!(s.looks[&2], Look::default());
        let s = Saved::parse("ferossh1 tab=r5000(@1,@2)/@9").unwrap();
        assert!(matches!(s.tabs[0].root, Node::Split { ratio: 900, .. }));
    }

    #[test]
    fn retains() {
        let root = sample().tabs.remove(0).root;
        let kept = root.clone().retain(&mut |w| w != 2).unwrap();
        assert_eq!(kept.windows(), [1, 3]);
        assert_eq!(root.retain(&mut |_| false), None);
    }
}
