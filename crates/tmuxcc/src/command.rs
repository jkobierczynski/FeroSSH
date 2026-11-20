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

/// Where an account's shared tmux server listens: a directory of its own
/// in `/tmp`, mode 0711 so that other accounts can reach the socket in it
/// but cannot add or swap anything there.
pub const SHARE_DIR_PREFIX: &str = "/tmp/ferossh-share-";

/// A Unix account name that is safe on a command line: letters, digits,
/// `_`, `.` and `-`, not starting with `-`, at most 32 (what `useradd`
/// takes). Anything else is refused, not cleaned: a cleaned name could be
/// someone else's account.
pub fn account_name(text: &str) -> Option<String> {
    let name = text.trim();
    let ok = !name.is_empty()
        && name.len() <= 32
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    ok.then(|| name.to_owned())
}

/// The account names in `text` (separated by spaces or commas); the
/// first one that is not a valid name comes back as the error.
pub fn account_names(text: &str) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    for word in text
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|w| !w.is_empty())
    {
        let name = account_name(word).ok_or_else(|| word.to_owned())?;
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

/// Whether `path` is the socket of a shared tmux server
/// (`/tmp/ferossh-share-<account>/default`), as tmux's `#{socket_path}`
/// gives it.
pub fn is_share_socket(path: &str) -> bool {
    path.strip_prefix(SHARE_DIR_PREFIX)
        .and_then(|rest| rest.strip_suffix("/default"))
        .is_some_and(|account| account_name(account).as_deref() == Some(account))
}

/// The shell test that `$FEROSSH_SHARE` is a real directory (not a
/// link), owned by `owner` (a shell word) and mode 0711, so nobody else
/// can have put a socket of theirs there.
fn share_dir_ok(owner: &str) -> String {
    format!(
        "[ -n \"$(find \"$FEROSSH_SHARE\" -maxdepth 0 -type d -user {owner} -perm 711 2>/dev/null)\" ]"
    )
}

/// The scripts below run in `sh -c '...'`, so that they work from any
/// login shell (fish, csh); they hold no single quote, as session and
/// account names never do.
///
/// What to type at the shell to start (or attach to) session `name` on
/// this account's shared tmux server, in control mode. The directory is
/// made first if missing, and nothing starts unless it is this
/// account's own: anyone may make directories in `/tmp`, so another
/// account could have made it first. The socket stays private until
/// FeroSSH, once it knows the tmux version, has set who may connect.
pub fn share_line(name: &str) -> String {
    format!(
        "sh -c 'FEROSSH_SHARE={SHARE_DIR_PREFIX}$(id -un); mkdir -m 711 \"$FEROSSH_SHARE\" 2>/dev/null; \
         if {}; then tmux -S \"$FEROSSH_SHARE/default\" -CC new-session -A -s {}; \
         else echo \"FeroSSH: $FEROSSH_SHARE is not yours, so nothing was shared.\"; fi'\r",
        share_dir_ok("\"$(id -un)\""),
        session_name(name)
    )
}

/// What to type at the shell to join a shared session in control mode:
/// session `session` (or tmux's most recent one) of the account `owner`
/// shares, or of this account's own tmux for `None`. Read-only unless
/// `can_type`; never sizing the owner's windows (`ignore-size`).
/// `owner` must be an [`account_name`]; its directory is checked to be
/// its own before connecting, so nobody can pose as it.
pub fn join_line(owner: Option<&str>, session: Option<&str>, can_type: bool) -> String {
    let flags = if can_type {
        "ignore-size"
    } else {
        "read-only,ignore-size"
    };
    let target = session
        .map(|s| format!(" -t ={}", session_name(s)))
        .unwrap_or_default();
    match owner.and_then(account_name) {
        None => format!("tmux -CC attach-session -f {flags}{target}\r"),
        Some(owner) => format!(
            "sh -c 'FEROSSH_SHARE={SHARE_DIR_PREFIX}{owner}; if {}; then \
             tmux -S \"$FEROSSH_SHARE/default\" -CC attach-session -f {flags}{target}; \
             else echo \"FeroSSH: {owner} is not sharing a tmux session on this host.\"; fi'\r",
            share_dir_ok(&owner)
        ),
    }
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

    #[test]
    fn accounts() {
        assert_eq!(account_name(" bob "), Some("bob".into()));
        assert_eq!(account_name("j.doe-2_x"), Some("j.doe-2_x".into()));
        for bad in [
            "",
            "-x",
            ".x",
            "a b",
            "a;b",
            "a$",
            "a'b",
            "a/b",
            &"x".repeat(33),
        ] {
            assert_eq!(account_name(bad), None, "{bad}");
        }
        assert_eq!(
            account_names("bob, carol  bob;dave"),
            Ok(vec!["bob".into(), "carol".into(), "dave".into()])
        );
        assert_eq!(account_names("bob $(rm)"), Err("$(rm)".into()));
        assert_eq!(account_names(" "), Ok(vec![]));
        assert!(is_share_socket("/tmp/ferossh-share-alice/default"));
        assert!(!is_share_socket("/tmp/tmux-1000/default"));
        assert!(!is_share_socket("/tmp/ferossh-share-a/b/default"));
        assert!(!is_share_socket("/tmp/ferossh-share-/default"));
    }

    #[test]
    fn join_and_share_lines() {
        assert_eq!(
            join_line(None, Some("pair"), false),
            "tmux -CC attach-session -f read-only,ignore-size -t =pair\r"
        );
        assert_eq!(
            join_line(None, None, true),
            "tmux -CC attach-session -f ignore-size\r"
        );
        let l = join_line(Some("alice"), Some("a b"), false);
        assert!(l.starts_with("sh -c 'FEROSSH_SHARE=/tmp/ferossh-share-alice; if [ -n "));
        assert!(l.ends_with("fi'\r"));
        assert!(l.contains("-user alice -perm 711"));
        assert!(l.contains("-CC attach-session -f read-only,ignore-size -t =a_b;"));
        // An owner that is no account name joins nothing of anyone's.
        assert_eq!(
            join_line(Some("x;y"), None, false),
            "tmux -CC attach-session -f read-only,ignore-size\r"
        );
        let s = share_line("pair");
        assert!(s.contains("mkdir -m 711 \"$FEROSSH_SHARE\""));
        assert!(s.contains("-user \"$(id -un)\" -perm 711"));
        assert!(s.contains("-CC new-session -A -s pair;"));
        assert!(s.starts_with("sh -c '"));
        assert!(s.ends_with("fi'\r"));
        assert_eq!(s.matches('\'').count(), 2);
    }
}
