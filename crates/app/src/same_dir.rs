//! New tabs and panes start where the original pane's shell is: the line
//! typed at a new remote shell, the tmux `-c` option, and the folder a
//! local Windows shell shows in its prompt.

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

/// The folder in a cmd (`C:\Users\me>`) or PowerShell
/// (`PS C:\Users\me> `) prompt at the cursor, for local shells, which
/// report no directory. Only absolute paths (a drive or a UNC share).
pub fn local_prompt_dir(line: &str) -> Option<String> {
    let body = line.strip_suffix(' ').unwrap_or(line).strip_suffix('>')?;
    let path = body.strip_prefix("PS ").unwrap_or(body);
    let mut chars = path.chars();
    let drive = matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(d), Some(':'), Some('\\')) if d.is_ascii_alphabetic()
    );
    let unc = path.starts_with("\\\\") && path.len() > 2;
    let bad = |c: char| c.is_control() || matches!(c, '<' | '>' | '|' | '"' | '*' | '?');
    ((drive || unc) && !path.chars().any(bad)).then(|| path.to_owned())
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
    fn reads_local_prompts() {
        assert_eq!(
            local_prompt_dir("C:\\Users\\me>").as_deref(),
            Some("C:\\Users\\me")
        );
        assert_eq!(
            local_prompt_dir("PS D:\\src\\my repo> ").as_deref(),
            Some("D:\\src\\my repo")
        );
        assert_eq!(local_prompt_dir("C:\\>").as_deref(), Some("C:\\"));
        assert_eq!(
            local_prompt_dir("\\\\nas\\share\\x>").as_deref(),
            Some("\\\\nas\\share\\x")
        );
        assert!(local_prompt_dir("user@host:~$ ").is_none());
        assert!(local_prompt_dir("C:\\Users\\me").is_none());
        assert!(local_prompt_dir("More? >").is_none());
        assert!(local_prompt_dir("echo a > C:\\x>").is_none());
        assert!(local_prompt_dir("relative\\dir>").is_none());
    }

    #[test]
    fn tmux_option() {
        assert_eq!(tmux_cwd("/srv/www\n"), " -c '/srv/www'");
        assert_eq!(tmux_cwd(""), "");
        assert_eq!(tmux_cwd("/tmp/it's"), "");
        assert_eq!(tmux_cwd("/tmp/#{x}"), " -c '/tmp/##{x}'");
    }
}
