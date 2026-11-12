//! tmux keybindings for FeroSSH's own tabs and panes (Session > Use tmux
//! keybindings): the tmux prefix, then a key such as `c`, `%` or `n`.
//!
//! In control mode tmux never sees the prefix: what is typed reaches the
//! pane through `send-keys`. So FeroSSH reads the user's prefix from the
//! server and does the window and pane keys itself, on its native tabs
//! (tmux windows) and panes.

use crate::layout::Dir;

/// A key as tmux names it in `prefix`: a character, with Meta (Alt) or
/// not. Control keys are their control character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    pub ch: char,
    pub alt: bool,
}

impl Prefix {
    /// tmux's default, `C-b`.
    pub const DEFAULT: Prefix = Prefix {
        ch: '\x02',
        alt: false,
    };

    /// A tmux key name (`C-b`, `C-Space`, `M-a`, `` ` ``); `None` for
    /// `None` and for keys that type no character, such as `F12`.
    pub fn parse(name: &str) -> Option<Prefix> {
        let (alt, rest) = match name.strip_prefix("M-") {
            Some(r) if !r.is_empty() => (true, r),
            _ => (false, name),
        };
        let (ctrl, rest) = match rest.strip_prefix("C-") {
            Some(r) if !r.is_empty() => (true, r),
            _ => match rest.strip_prefix('^') {
                Some(r) if r.chars().count() == 1 => (true, r),
                _ => (false, rest),
            },
        };
        let ch = match rest {
            "Space" => ' ',
            "Escape" if !ctrl => '\x1b',
            "Tab" if !ctrl => '\t',
            "Enter" if !ctrl => '\r',
            r => {
                let mut chars = r.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                c
            }
        };
        let ch = if ctrl { control(ch)? } else { ch };
        Some(Prefix { ch, alt })
    }

    /// The bytes that send this key to the pane.
    pub fn bytes(self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.alt {
            out.push(0x1b);
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(self.ch.encode_utf8(&mut buf).as_bytes());
        out
    }
}

/// The control character Ctrl+`c` types, as in a terminal.
fn control(c: char) -> Option<char> {
    let code = match c {
        ' ' | '@' | '2' => 0x00,
        'a'..='z' => c as u32 - 'a' as u32 + 1,
        'A'..='Z' => c as u32 - 'A' as u32 + 1,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    };
    char::from_u32(code)
}

/// What the server said: `prefix`, `prefix2`, `base-index` and
/// `repeat-time`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    pub prefixes: Vec<Prefix>,
    /// The number of the first window (tab), for the digit keys.
    pub base_index: usize,
    /// How long, in milliseconds, a repeatable key (an arrow, PageUp)
    /// goes on working without the prefix, counted from the last one.
    pub repeat_ms: u64,
}

/// tmux's default `repeat-time`.
pub const DEFAULT_REPEAT_MS: u64 = 500;

impl Default for Keys {
    fn default() -> Self {
        Self {
            prefixes: vec![Prefix::DEFAULT],
            base_index: 0,
            repeat_ms: DEFAULT_REPEAT_MS,
        }
    }
}

/// The format FeroSSH asks tmux to expand; [`Keys::parse`] reads it.
pub const FORMAT: &str = "#{prefix} #{prefix2} #{base-index} #{repeat-time}";

impl Keys {
    /// The answer to [`FORMAT`], such as `C-b None 0 500`. A prefix
    /// FeroSSH cannot take (a function key) leaves just the other one;
    /// with neither, the keybindings have no prefix and do nothing.
    pub fn parse(reply: &str) -> Option<Keys> {
        let words: Vec<&str> = reply.split_whitespace().collect();
        let (prefix, prefix2, base, repeat) = match words.as_slice() {
            [p, p2, b] => (p, p2, b, None),
            [p, p2, b, r] => (p, p2, b, Some(r)),
            _ => return None,
        };
        let base_index = base.parse().ok()?;
        let repeat_ms = match repeat {
            Some(r) => r.parse().ok()?,
            None => DEFAULT_REPEAT_MS,
        };
        let prefixes = [prefix, prefix2]
            .into_iter()
            .filter_map(|p| Prefix::parse(p))
            .collect();
        Some(Keys {
            prefixes,
            base_index,
            repeat_ms,
        })
    }

    pub fn is_prefix(&self, key: Prefix) -> bool {
        self.prefixes.contains(&key)
    }
}

/// A key typed after the prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Text, from WM_CHAR, with Alt held or not.
    Char(char, bool),
    Arrow {
        dir: Dir,
        ctrl: bool,
        alt: bool,
    },
    PageUp,
    PageDown,
    Escape,
}

/// What a key after the prefix does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The prefix itself goes to the pane (the prefix typed twice).
    SendPrefix(Prefix),
    /// `c`: a new tab, which is a new tmux window.
    NewTab,
    /// `%`
    SplitRight,
    /// `"`
    SplitDown,
    /// `n`, `p`
    NextTab,
    PrevTab,
    /// `l`: the tab that had the focus before.
    LastTab,
    /// `0` to `9`: the tab with that tmux window number.
    Tab(usize),
    /// `o`: the next pane in the tab.
    NextPane,
    /// `;`: the pane in the tab that had the focus before.
    LastPane,
    /// Prefix, then an arrow.
    Focus(Dir),
    /// Prefix, then Ctrl+arrow or Alt+arrow.
    Resize(Dir),
    /// `x`, after a yes.
    ClosePane,
    /// `&`, after a yes.
    CloseTab,
    /// `!`: the pane to a tab of its own.
    BreakPane,
    /// `d`
    Detach,
    /// `s` and `w`: the other tmux sessions.
    ChooseSession,
    /// `[` and PageUp: scroll back a page.
    ScrollBack,
    /// PageDown: scroll forward a page (useful while repeating).
    ScrollForward,
    /// `:`: the command palette.
    Palette,
    /// `f`: find in scrollback.
    Find,
    /// `?`: the list of keys.
    Help,
    /// Escape, or a key without a binding: nothing happens, as in tmux.
    Nothing,
}

/// What `key`, typed after the prefix, does.
pub fn action(keys: &Keys, key: Key) -> Action {
    match key {
        Key::Escape => Action::Nothing,
        Key::PageUp => Action::ScrollBack,
        Key::PageDown => Action::ScrollForward,
        Key::Arrow { dir, ctrl, alt } => {
            if ctrl || alt {
                Action::Resize(dir)
            } else {
                Action::Focus(dir)
            }
        }
        Key::Char(ch, alt) => {
            let typed = Prefix { ch, alt };
            if keys.is_prefix(typed) {
                return Action::SendPrefix(typed);
            }
            if alt {
                return Action::Nothing;
            }
            match ch {
                'c' => Action::NewTab,
                '%' => Action::SplitRight,
                '"' => Action::SplitDown,
                'n' => Action::NextTab,
                'p' => Action::PrevTab,
                'l' => Action::LastTab,
                '0'..='9' => {
                    let n = ch as usize - '0' as usize;
                    n.checked_sub(keys.base_index)
                        .map_or(Action::Nothing, Action::Tab)
                }
                'o' => Action::NextPane,
                ';' => Action::LastPane,
                'x' => Action::ClosePane,
                '&' => Action::CloseTab,
                '!' => Action::BreakPane,
                'd' => Action::Detach,
                's' | 'w' => Action::ChooseSession,
                '[' => Action::ScrollBack,
                ':' => Action::Palette,
                'f' => Action::Find,
                '?' => Action::Help,
                _ => Action::Nothing,
            }
        }
    }
}

/// Whether `action` is repeatable, as tmux binds it with `bind -r`: once
/// done, more of these keys work without the prefix until `repeat-time`
/// has passed since the last one. Moving between panes and resizing are
/// `-r` in tmux's defaults; scrolling is here too, as in copy mode.
pub fn repeatable(action: Action) -> bool {
    matches!(
        action,
        Action::Focus(_) | Action::Resize(_) | Action::ScrollBack | Action::ScrollForward
    )
}

/// Shown for prefix `?`, with the prefix as tmux names it.
pub fn help(prefix: &str, repeat_ms: u64) -> String {
    format!(
        "tmux keybindings: press {prefix}, then\n\n\
         c\tnew tab (tmux window)\n\
         %\tsplit right\n\
         \"\tsplit down\n\
         n / p\tnext / previous tab\n\
         l\tlast tab\n\
         0-9\ttab by tmux window number\n\
         o\tnext pane\n\
         ;\tlast pane\n\
         arrows\tmove to the pane in that direction\n\
         Ctrl or Alt+arrows\tresize the pane\n\
         x\tclose the pane\n\
         &\tclose the tab\n\
         !\tmove the pane to its own tab\n\
         d\tdetach tmux\n\
         s / w\tswitch tmux session\n\
         [ / PageUp\tscroll back a page (PageDown forward)\n\
         :\tcommand palette\n\
         f\tfind in scrollback\n\
         {prefix} again\tsend {prefix} to the pane\n\n\
         Arrows, Ctrl or Alt+arrows, [, PageUp and PageDown repeat: \
         press them again without {prefix} while they keep coming \
         (tmux's repeat-time, {repeat_ms} ms).\n\n\
         Session > tmux > Use tmux keybindings turns them off."
    )
}

/// The prefix as tmux would write it, for the help text.
pub fn name(p: Prefix) -> String {
    let base = match p.ch {
        '\x00' => "C-Space".to_owned(),
        '\x1b' => "Escape".to_owned(),
        '\t' => "Tab".to_owned(),
        '\r' => "Enter".to_owned(),
        ' ' => "Space".to_owned(),
        c @ '\x01'..='\x1a' => format!("C-{}", char::from(b'a' + c as u8 - 1)),
        '\x1c' => "C-\\".to_owned(),
        '\x1d' => "C-]".to_owned(),
        '\x1e' => "C-^".to_owned(),
        '\x1f' => "C-_".to_owned(),
        '\x7f' => "C-?".to_owned(),
        c => c.to_string(),
    };
    if p.alt { format!("M-{base}") } else { base }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn p(ch: char, alt: bool) -> Prefix {
        Prefix { ch, alt }
    }

    #[test]
    fn reads_tmux_key_names() {
        assert_eq!(Prefix::parse("C-b"), Some(p('\x02', false)));
        assert_eq!(Prefix::parse("C-a"), Some(p('\x01', false)));
        assert_eq!(Prefix::parse("C-Space"), Some(p('\x00', false)));
        assert_eq!(Prefix::parse("C-@"), Some(p('\x00', false)));
        assert_eq!(Prefix::parse("C-\\"), Some(p('\x1c', false)));
        assert_eq!(Prefix::parse("^B"), Some(p('\x02', false)));
        assert_eq!(Prefix::parse("`"), Some(p('`', false)));
        assert_eq!(Prefix::parse("M-a"), Some(p('a', true)));
        assert_eq!(Prefix::parse("M-C-x"), Some(p('\x18', true)));
        assert_eq!(Prefix::parse("Escape"), Some(p('\x1b', false)));
        assert_eq!(Prefix::parse("None"), None);
        assert_eq!(Prefix::parse("F12"), None);
        assert_eq!(Prefix::parse(""), None);
        assert_eq!(Prefix::parse("M-"), None);
    }

    #[test]
    fn names_round_trip() {
        for n in [
            "C-b", "C-a", "C-Space", "`", "M-a", "C-\\", "Escape", "M-C-x",
        ] {
            let parsed = Prefix::parse(n).unwrap();
            assert_eq!(Prefix::parse(&name(parsed)), Some(parsed), "{n}");
        }
        assert_eq!(name(Prefix::DEFAULT), "C-b");
    }

    #[test]
    fn reads_the_server_reply() {
        // As tmux 3.4 expands FORMAT.
        assert_eq!(Keys::parse("C-b None 0"), Some(Keys::default()));
        assert_eq!(Keys::parse("C-b None 0 500"), Some(Keys::default()));
        assert_eq!(Keys::parse("C-b None 0 1200").unwrap().repeat_ms, 1200);
        assert_eq!(Keys::parse("C-b None 0 x"), None);
        let k = Keys::parse("C-a M-x 1").unwrap();
        assert_eq!(k.prefixes, vec![p('\x01', false), p('x', true)]);
        assert_eq!(k.base_index, 1);
        let k = Keys::parse("F12 None 0").unwrap();
        assert!(k.prefixes.is_empty());
        assert_eq!(Keys::parse("C-b"), None);
        assert_eq!(Keys::parse("C-b None x"), None);
    }

    #[test]
    fn the_prefix_twice_sends_it() {
        let k = Keys::default();
        assert_eq!(
            action(&k, Key::Char('\x02', false)),
            Action::SendPrefix(Prefix::DEFAULT)
        );
        assert_eq!(Prefix::DEFAULT.bytes(), vec![0x02]);
        assert_eq!(p('a', true).bytes(), b"\x1ba".to_vec());
        let k = Keys::parse("C-a M-x 0").unwrap();
        assert_eq!(
            action(&k, Key::Char('x', true)),
            Action::SendPrefix(p('x', true))
        );
        // `x` without Alt is still close pane.
        assert_eq!(action(&k, Key::Char('x', false)), Action::ClosePane);
    }

    #[test]
    fn window_and_pane_keys() {
        let k = Keys::default();
        let c = |ch| action(&k, Key::Char(ch, false));
        assert_eq!(c('c'), Action::NewTab);
        assert_eq!(c('%'), Action::SplitRight);
        assert_eq!(c('"'), Action::SplitDown);
        assert_eq!(c('n'), Action::NextTab);
        assert_eq!(c('p'), Action::PrevTab);
        assert_eq!(c('0'), Action::Tab(0));
        assert_eq!(c('9'), Action::Tab(9));
        assert_eq!(c('o'), Action::NextPane);
        assert_eq!(c('d'), Action::Detach);
        assert_eq!(c('q'), Action::Nothing);
        assert_eq!(action(&k, Key::Char('c', true)), Action::Nothing);
        assert_eq!(action(&k, Key::Escape), Action::Nothing);
        assert_eq!(
            action(
                &k,
                Key::Arrow {
                    dir: Dir::Left,
                    ctrl: false,
                    alt: false
                }
            ),
            Action::Focus(Dir::Left)
        );
        assert_eq!(
            action(
                &k,
                Key::Arrow {
                    dir: Dir::Up,
                    ctrl: true,
                    alt: false
                }
            ),
            Action::Resize(Dir::Up)
        );
    }

    #[test]
    fn digits_follow_base_index() {
        let k = Keys::parse("C-b None 1").unwrap();
        assert_eq!(action(&k, Key::Char('1', false)), Action::Tab(0));
        assert_eq!(action(&k, Key::Char('0', false)), Action::Nothing);
    }

    #[test]
    fn moving_resizing_and_scrolling_repeat() {
        let k = Keys::default();
        for key in [
            Key::Arrow {
                dir: Dir::Up,
                ctrl: false,
                alt: false,
            },
            Key::Arrow {
                dir: Dir::Left,
                ctrl: true,
                alt: false,
            },
            Key::Arrow {
                dir: Dir::Right,
                ctrl: false,
                alt: true,
            },
            Key::PageUp,
            Key::PageDown,
            Key::Char('[', false),
        ] {
            assert!(repeatable(action(&k, key)), "{key:?}");
        }
        for ch in ['c', '%', 'n', 'o', 'x', 'd', '\x02'] {
            assert!(!repeatable(action(&k, Key::Char(ch, false))), "{ch:?}");
        }
        assert!(!repeatable(action(&k, Key::Escape)));
        assert_eq!(action(&k, Key::PageDown), Action::ScrollForward);
    }

    #[test]
    fn help_names_the_prefix() {
        let text = help(&name(Prefix::DEFAULT), 500);
        assert!(text.contains("repeat-time, 500 ms"));
        assert!(text.starts_with("tmux keybindings: press C-b, then"));
        assert!(text.contains("C-b again\tsend C-b to the pane"));
    }
}
