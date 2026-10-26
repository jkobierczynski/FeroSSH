//! Where a session's password comes from, so nobody has to type it: the
//! Windows Credential Manager, or a password manager's command line tool
//! (1Password's `op`, Bitwarden's `bw`, KeePassXC's `keepassxc-cli`).
//!
//! A command is never run through a shell. Its line is split into a
//! program and arguments here, with double quotes grouping, and these
//! tokens are filled into the arguments only:
//!
//! - `%n` the session's name (its `Host` alias)
//! - `%h` the host name connected to
//! - `%u` the user name
//! - `%p` the port
//! - `%%` a `%`

/// Where the password for a session comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PasswordSource {
    /// Typed at the prompt every time.
    #[default]
    Ask,
    /// The Windows Credential Manager, filled after a login that worked.
    Windows,
    /// The first line a password manager's command prints.
    Command(String),
}

/// A ready-made command for a password manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Template {
    pub name: &'static str,
    pub command: &'static str,
}

/// The password managers the session form offers. Each looks the session
/// up by its name; edit the command when the item is called differently.
pub const TEMPLATES: [Template; 3] = [
    Template {
        name: "1Password",
        command: "op read \"op://Private/%n/password\"",
    },
    Template {
        name: "Bitwarden",
        command: "bw get password %n",
    },
    Template {
        name: "KeePassXC",
        command: "keepassxc-cli show -s -a Password --no-password -k \
                  \"C:\\Users\\Me\\Passwords.keyx\" \"C:\\Users\\Me\\Passwords.kdbx\" %n",
    },
];

/// Splits a command line into the program and its arguments: spaces
/// separate, double quotes group (and are dropped), everything else is
/// kept as written, backslashes included.
pub fn split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c if c.is_control() => {}
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// What the `%` tokens stand for.
#[derive(Debug, Clone, Copy)]
pub struct Tokens<'a> {
    pub name: &'a str,
    pub host: &'a str,
    pub user: &'a str,
    pub port: u16,
}

/// Fills the tokens into one argument.
fn expand(arg: &str, t: &Tokens<'_>) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push_str(t.name),
            Some('h') => out.push_str(t.host),
            Some('u') => out.push_str(t.user),
            Some('p') => out.push_str(&t.port.to_string()),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// The program and arguments to run, or `None` for an empty line. The
/// program itself is never expanded, so no session name can pick it.
pub fn command(line: &str, t: &Tokens<'_>) -> Option<(String, Vec<String>)> {
    let mut parts = split(line).into_iter();
    let program = parts.next().filter(|p| !p.is_empty())?;
    Some((program, parts.map(|a| expand(&a, t)).collect()))
}

/// The password in what the command printed: its first line, without the
/// line break. `None` when it printed nothing.
pub fn first_line(output: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(output).ok()?;
    let line = text.split(['\n', '\r']).next().unwrap_or_default();
    (!line.is_empty()).then(|| line.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const T: Tokens<'static> = Tokens {
        name: "web",
        host: "web.example.org",
        user: "admin",
        port: 22,
    };

    #[test]
    fn splits_like_a_command_line_without_a_shell() {
        assert_eq!(
            split(r#"op read "op://Private/my web/password""#),
            ["op", "read", "op://Private/my web/password"]
        );
        assert_eq!(split(r#"a "" b"#), ["a", "", "b"]);
        assert_eq!(split(r"C:\Tools\bw.exe  get"), [r"C:\Tools\bw.exe", "get"]);
        assert_eq!(split("a & calc | b; c"), ["a", "&", "calc", "|", "b;", "c"]);
        assert!(split("  ").is_empty());
    }

    #[test]
    fn fills_tokens_into_arguments_only() {
        let (program, args) = command("%n get %u@%h:%p 100%% %x", &T).unwrap();
        assert_eq!(program, "%n");
        assert_eq!(args, ["get", "admin@web.example.org:22", "100%", "%x"]);
        let (program, args) = command(TEMPLATES[0].command, &T).unwrap();
        assert_eq!(
            (program.as_str(), args[1].as_str()),
            ("op", "op://Private/web/password")
        );
        assert_eq!(command("", &T), None);
    }

    #[test]
    fn takes_the_first_line() {
        assert_eq!(first_line(b"s3cret\r\nmore").as_deref(), Some("s3cret"));
        assert_eq!(first_line(b"pa ss\n").as_deref(), Some("pa ss"));
        assert_eq!(first_line(b"\nx"), None);
        assert_eq!(first_line(b""), None);
        assert_eq!(first_line(&[0xff, 0xfe]), None);
    }
}
