//! Command lines for tmux.

use std::fmt::Write as _;

use crate::PaneId;

/// Bytes per `send-keys` command; longer input is split.
pub const KEYS_PER_COMMAND: usize = 256;

/// A single-quoted argument. tmux expands nothing inside single quotes;
/// quotes and control characters, which could end the argument or the
/// line, are dropped.
pub fn quote(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| *c != '\'' && !c.is_control())
        .collect();
    format!("'{clean}'")
}

/// `send-keys` commands that type `bytes` into pane `pane` exactly, as
/// hex so that nothing in them is read as a key name.
pub fn send_keys(pane: PaneId, bytes: &[u8]) -> Vec<String> {
    bytes
        .chunks(KEYS_PER_COMMAND)
        .map(|chunk| {
            let mut line = format!("send-keys -t %{pane} -H");
            for b in chunk {
                let _ = write!(line, " {b:02x}");
            }
            line
        })
        .collect()
}

/// A session name tmux accepts and that is safe on a command line:
/// letters, digits, `-` and `_` (tmux itself refuses `:` and `.`).
pub fn session_name(text: &str) -> String {
    let name: String = text
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if name.is_empty() {
        "main".to_owned()
    } else {
        name
    }
}

/// What to type at the shell to start control mode: attach to session
/// `name`, made if it is missing.
pub fn attach_line(name: &str) -> String {
    format!("tmux -CC new-session -A -s {}\r", session_name(name))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn quotes() {
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's\n#{x}"), "'its#{x}'");
    }

    #[test]
    fn sends_hex() {
        assert_eq!(send_keys(3, b"a\x1b\r"), ["send-keys -t %3 -H 61 1b 0d"]);
        let many = send_keys(1, &[b'x'; KEYS_PER_COMMAND + 1]);
        assert_eq!(many.len(), 2);
        assert_eq!(many[1], "send-keys -t %1 -H 78");
        assert!(send_keys(1, b"").is_empty());
    }

    #[test]
    fn names() {
        assert_eq!(session_name(" work "), "work");
        assert_eq!(session_name("a.b:c;rm -rf"), "a_b_c_rm_-rf");
        assert_eq!(session_name(""), "main");
        assert_eq!(attach_line("dev"), "tmux -CC new-session -A -s dev\r");
    }
}
