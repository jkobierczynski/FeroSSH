//! Session > Copy public key to this server, as `ssh-copy-id` does: a
//! fixed script on an exec channel of the pane's connection adds the
//! key's line to `~/.ssh/authorized_keys` unless the key is there
//! already, making the folder (700) and the file (600) when missing.
//!
//! The key line goes on the script's standard input, never into the
//! command, so nothing in it reaches a shell as code. The line is checked
//! and written out again first ([`ferossh_auth::keygen::public_line`]).

/// The command run on the server. The login shell only starts `sh`
/// with it, so csh and fish run it too; cmd.exe and PowerShell cannot.
/// Its last line of output says what happened.
pub fn command() -> Vec<u8> {
    // No single quote inside, and nothing csh would expand outside them.
    let script = concat!(
        "umask 077; ",
        "d=\"$HOME/.ssh\"; f=\"$d/authorized_keys\"; ",
        "IFS= read -r k || [ -n \"$k\" ] || exit 3; ",
        "mkdir -p \"$d\" || exit 4; ",
        "[ -f \"$f\" ] || : >> \"$f\" || exit 5; ",
        // The type and the key itself, as two whole fields anywhere on a
        // line that is not a comment: options before them and a comment
        // after them do not matter. `\$` keeps sh from expanding awk's.
        "t=${k%% *}; r=${k#* }; y=${r%% *}; ",
        "if awk -v t=\"$t\" -v y=\"$y\" ",
        "\"BEGIN { r = 1 } /^[ \t]*#/ { next } ",
        "{ for (i = 1; i < NF; i++) if (\\$i == t && \\$(i + 1) == y) r = 0 } END { exit r }\" ",
        "\"$f\"; then echo FEROSSH-KEY-PRESENT; exit 0; fi; ",
        // A last line without its newline would run into the new one.
        "if [ -s \"$f\" ] && [ -n \"$(tail -c 1 \"$f\")\" ]; then echo >> \"$f\" || exit 5; fi; ",
        "printf \"%s\\n\" \"$k\" >> \"$f\" || exit 5; ",
        "if command -v restorecon >/dev/null 2>&1; then restorecon -F \"$d\" \"$f\" >/dev/null 2>&1; fi; ",
        "echo FEROSSH-KEY-ADDED",
    );
    format!("exec sh -c '{script}'").into_bytes()
}

/// What goes on the command's standard input: the key's line.
pub fn input(line: &str) -> Vec<u8> {
    format!("{}\n", line.trim_end()).into_bytes()
}

/// How the copy went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Added,
    /// The server had the key already; nothing changed.
    Present,
    Failed(String),
}

/// What the command's exit `status` and `output` mean.
pub fn outcome(status: Option<u32>, output: &[u8]) -> Outcome {
    let text = String::from_utf8_lossy(output);
    let last = text.lines().map(str::trim).rfind(|l| !l.is_empty());
    match (status, last) {
        (Some(0), Some("FEROSSH-KEY-ADDED")) => Outcome::Added,
        (Some(0), Some("FEROSSH-KEY-PRESENT")) => Outcome::Present,
        (Some(4), _) => Outcome::Failed("could not make the ~/.ssh folder".into()),
        (Some(5), _) => Outcome::Failed("could not write ~/.ssh/authorized_keys".into()),
        (_, said) => {
            let said = said.map(crate::ssh::sanitize).unwrap_or_default();
            let mut why = "the server has no Unix shell (sh) to run it".to_owned();
            if !said.is_empty() {
                why.push_str(&format!("; it said: {said}"));
            }
            Outcome::Failed(why)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_happened() {
        assert_eq!(outcome(Some(0), b"FEROSSH-KEY-ADDED\n"), Outcome::Added);
        assert_eq!(
            outcome(Some(0), b"motd\nFEROSSH-KEY-PRESENT\n"),
            Outcome::Present
        );
        assert!(
            matches!(outcome(Some(5), b""), Outcome::Failed(w) if w.contains("authorized_keys"))
        );
        assert!(
            matches!(outcome(Some(1), b"'exec' is not recognized\r\n"), Outcome::Failed(w) if w.contains("no Unix shell") && w.contains("not recognized"))
        );
        assert!(matches!(outcome(None, b""), Outcome::Failed(_)));
    }

    #[test]
    fn no_single_quote_inside_the_script() {
        let c = String::from_utf8(command()).unwrap();
        let inner = c
            .strip_prefix("exec sh -c '")
            .unwrap()
            .strip_suffix('\'')
            .unwrap();
        assert!(!inner.contains('\''));
        assert!(!inner.contains('!'), "csh would expand history");
    }

    /// Runs the real script with `sh`, in a home folder of its own.
    #[cfg(unix)]
    fn run(home: &std::path::Path, line: &str) -> Outcome {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(String::from_utf8(command()).unwrap())
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&input(line)).unwrap();
        let out = child.wait_with_output().unwrap();
        let status = out.status.code().and_then(|c| u32::try_from(c).ok());
        outcome(status, &out.stdout)
    }

    #[test]
    #[cfg(unix)]
    fn adds_a_key_once_and_keeps_the_others() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("ferossh-copy-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let a = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIH1XxkKbD7mqSIALnVuV3uqrI63sOrdwTB27FzdZjEEp me@pc";
        let b = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl $(touch pwned) `touch pwned2`";
        assert_eq!(run(&home, a), Outcome::Added);
        let dir = home.join(".ssh");
        let file = dir.join("authorized_keys");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);
        // The same key with another comment is there already.
        assert_eq!(run(&home, &a.replace("me@pc", "other")), Outcome::Present);
        // Only whole fields count: not a comment line, nor a longer key
        // that starts with the same text; options before it are fine.
        let c =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGb1kTFnfWj0eRbsfFjvnU6oSo8mfx5h1JcsnEgD35Ax c";
        let text = std::fs::read_to_string(&file).unwrap();
        let longer = c.replace("Ax c", "Axyz c");
        std::fs::write(&file, format!("{text}# {c}\n{longer}\n")).unwrap();
        assert_eq!(run(&home, c), Outcome::Added);
        assert_eq!(run(&home, c), Outcome::Present);
        std::fs::write(&file, format!("{text}no-pty,command=\"a b\" {c}\n")).unwrap();
        assert_eq!(run(&home, c), Outcome::Present);
        std::fs::write(&file, &text).unwrap();
        // A last line without its newline.
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}ssh-rsa AAAAB3 old")).unwrap();
        assert_eq!(run(&home, b), Outcome::Added);
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text, format!("{a}\nssh-rsa AAAAB3 old\n{b}\n"));
        // The key's text is data, never run.
        assert!(!home.join("pwned").exists() && !home.join("pwned2").exists());
        let _ = std::fs::remove_dir_all(&home);
    }
}
