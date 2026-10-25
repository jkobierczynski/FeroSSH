//! tmux control mode (`tmux -CC`) for FeroSSH: the protocol, tmux's layout
//! strings, and the layout FeroSSH keeps in the tmux session.
//!
//! In control mode tmux does not draw anything itself. It writes one line
//! per event (`%output %3 ...`, `%layout-change @1 ...`) and answers each
//! command line it reads with a `%begin` ... `%end` block. [`Client`]
//! finds where control mode starts and ends in a pane's output, splits it
//! into those lines and pairs every reply with the command that asked for
//! it. [`layout`] reads and writes tmux's window layouts, and [`saved`]
//! is FeroSSH's own layout (splits, and each window's font and theme),
//! kept in a tmux user option so that it comes back on any PC.
//!
//! Everything here reads what the remote host sends, so it is written for
//! hostile input: sizes, depths and counts are capped and nothing panics.

pub mod command;
pub mod layout;
pub mod protocol;
pub mod saved;

mod client;

pub use client::{Client, Event};
pub use protocol::{Notification, PaneId, SessionId, WindowId};

/// The oldest tmux that FeroSSH drives in control mode: `send-keys -H`
/// arrived in 3.0.
pub const MIN_VERSION: Version = Version { major: 3, minor: 0 };

/// The first tmux that sizes each window for a control client on its own
/// (`refresh-client -C @1:80x24`), which FeroSSH layout needs.
pub const WINDOW_SIZES_VERSION: Version = Version { major: 3, minor: 3 };

/// A tmux release, as `#{version}` gives it ("3.4", "3.3a", "next-3.5").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

impl Version {
    /// Reads `#{version}`. Builds from tmux's main branch ("next-3.5",
    /// "master") count as newer than any release; OpenBSD's base tmux
    /// ("openbsd-7.4") is current too.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text == "master" || text.starts_with("openbsd-") {
            return Some(Self {
                major: u16::MAX,
                minor: 0,
            });
        }
        let text = text.strip_prefix("next-").unwrap_or(text);
        let (major, rest) = text.split_once('.')?;
        let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        let v = |s| Version::parse(s);
        assert_eq!(v("3.4"), Some(Version { major: 3, minor: 4 }));
        assert_eq!(v("3.3a\n"), Some(Version { major: 3, minor: 3 }));
        assert_eq!(v("next-3.6"), Some(Version { major: 3, minor: 6 }));
        assert!(v("openbsd-7.5") >= Some(WINDOW_SIZES_VERSION));
        assert!(v("2.9a") < Some(MIN_VERSION));
        assert_eq!(v("tmux"), None);
        assert_eq!(v(""), None);
    }
}
