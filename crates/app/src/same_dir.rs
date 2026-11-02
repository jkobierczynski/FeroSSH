//! Window > Split … in same directory: the line typed at a new pane's
//! remote shell to start it where the original pane's shell is.

/// The line that changes a POSIX-like shell (bash, zsh, dash, ksh, fish,
/// tcsh) to `dir`, single-quoted; `None` when there is nothing to do or
/// the path holds control characters, which must never be typed. The
/// leading space keeps it out of the history where `HISTCONTROL` has
/// `ignorespace` (Debian and Ubuntu's default).
pub fn cd_line(dir: &[u8]) -> Option<Vec<u8>> {
    if dir.is_empty() || dir == b"." || dir.iter().any(|b| b.is_ascii_control()) {
        return None;
    }
    let mut line = b" cd '".to_vec();
    for &b in dir {
        if b == b'\'' {
            line.extend_from_slice(b"'\\''");
        } else {
            line.push(b);
        }
    }
    line.extend_from_slice(b"'\r");
    Some(line)
}

/// The tmux `-c` option for `dir`, as tmux's pane_current_path gave it;
/// empty when there is none or it cannot be quoted for tmux safely. tmux
/// expands formats in `-c`, so a `#` is doubled.
pub fn tmux_cwd(dir: &str) -> String {
    let dir = dir.trim_end_matches(['\r', '\n']);
    if dir.is_empty() || dir.chars().any(|c| c == '\'' || c.is_control()) {
        return String::new();
    }
    format!(" -c '{}'", dir.replace('#', "##"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn quotes_the_directory() {
        assert_eq!(cd_line(b"/srv/www").unwrap(), b" cd '/srv/www'\r");
        assert_eq!(
            cd_line(b"/tmp/it's here").unwrap(),
            b" cd '/tmp/it'\\''s here'\r"
        );
        assert_eq!(cd_line(b"/a $(b) `c`").unwrap(), b" cd '/a $(b) `c`'\r");
        // Relative to the home folder, where the shell starts anyway.
        assert_eq!(cd_line(b"src").unwrap(), b" cd 'src'\r");
    }

    #[test]
    fn refuses_what_must_not_be_typed() {
        assert!(cd_line(b"").is_none());
        assert!(cd_line(b".").is_none());
        assert!(cd_line(b"/tmp/x\r rm -rf ~").is_none());
        assert!(cd_line(b"/tmp/\x1b[A").is_none());
    }

    #[test]
    fn tmux_option() {
        assert_eq!(tmux_cwd("/srv/www\n"), " -c '/srv/www'");
        assert_eq!(tmux_cwd(""), "");
        assert_eq!(tmux_cwd("/tmp/it's"), "");
        assert_eq!(tmux_cwd("/tmp/#{x}"), " -c '/tmp/##{x}'");
    }
}
