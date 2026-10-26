//! Login scripts and command snippets.
//!
//! A login script answers what a device asks right after the login, as
//! `expect` scripts do: wait for a prompt, type a line, type a password.
//! Each step is one line:
//!
//! - `expect TEXT` waits for `TEXT` in what the host prints
//! - `send TEXT` types `TEXT` and Enter (just Enter when empty)
//! - `password` types the session's login password from its password
//!   source
//! - `secret NAME` types a named secret, such as an `enable` password,
//!   kept in the Windows Credential Manager or got from a password
//!   manager command, never in a FeroSSH file
//! - `pause SECONDS` waits a moment
//!
//! A snippet is a named list of command lines, typed into a pane from the
//! snippets view.

use std::fmt;

/// Seconds an `expect` waits before the script stops.
pub const EXPECT_TIMEOUT_SECS: u64 = 15;
/// The longest `pause`, in seconds.
pub const MAX_PAUSE_SECS: u32 = 60;
/// Characters in an `expect` or `send` text.
pub const MAX_TEXT: usize = 256;

/// One step of a login script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Expect(String),
    Send(String),
    Password,
    Secret(String),
    Pause(u32),
}

/// Whether `name` can name a secret: letters, digits, `.`, `_` and `-`.
pub fn valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Step {
    /// Reads a step as written in the form or the settings file.
    pub fn parse(line: &str) -> Result<Step, String> {
        let line = line.trim();
        let (verb, rest) = match line.find(char::is_whitespace) {
            Some(i) => (line.get(..i).unwrap_or(line), line.get(i..).unwrap_or("")),
            None => (line, ""),
        };
        let rest = rest.trim();
        if rest.chars().any(char::is_control) {
            return Err("Steps are one line of text.".into());
        }
        if rest.chars().count() > MAX_TEXT {
            return Err(format!("A step's text has at most {MAX_TEXT} characters."));
        }
        match verb.to_ascii_lowercase().as_str() {
            "expect" if rest.is_empty() => Err("expect needs the text to wait for.".into()),
            "expect" => Ok(Step::Expect(rest.to_owned())),
            "send" => Ok(Step::Send(rest.to_owned())),
            "password" if rest.is_empty() => Ok(Step::Password),
            "password" => Err("password takes nothing after it.".into()),
            "secret" if valid_secret_name(rest) => Ok(Step::Secret(rest.to_owned())),
            "secret" => Err("secret needs a name of letters, digits, . _ and -.".into()),
            "pause" => match rest.parse::<u32>() {
                Ok(n) if (1..=MAX_PAUSE_SECS).contains(&n) => Ok(Step::Pause(n)),
                _ => Err(format!("pause takes seconds, from 1 to {MAX_PAUSE_SECS}.")),
            },
            _ => Err("Start a step with expect, send, password, secret or pause.".into()),
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Expect(t) => write!(f, "expect {t}"),
            Step::Send(t) if t.is_empty() => f.write_str("send"),
            Step::Send(t) => write!(f, "send {t}"),
            Step::Password => f.write_str("password"),
            Step::Secret(n) => write!(f, "secret {n}"),
            Step::Pause(n) => write!(f, "pause {n}"),
        }
    }
}

/// Where a named secret comes from when it is not in the Windows
/// Credential Manager: a password manager command (see
/// [`crate::password`]; `%n` is the secret's name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSource {
    pub name: String,
    pub command: String,
}

/// A named list of command lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snippet {
    pub name: String,
    pub lines: Vec<String>,
}

impl Snippet {
    /// Whether `filter`'s words all appear in the name or the lines,
    /// ignoring case.
    pub fn matches(&self, filter: &str) -> bool {
        let hay = format!("{}\n{}", self.name, self.lines.join("\n")).to_lowercase();
        filter
            .split_whitespace()
            .all(|w| hay.contains(&w.to_lowercase()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn steps_parse_and_print() {
        for (text, step) in [
            ("expect Username:", Step::Expect("Username:".into())),
            ("EXPECT  #", Step::Expect("#".into())),
            (
                "send terminal length 0",
                Step::Send("terminal length 0".into()),
            ),
            ("send", Step::Send(String::new())),
            ("password", Step::Password),
            ("secret enable", Step::Secret("enable".into())),
            ("pause 2", Step::Pause(2)),
        ] {
            let got = Step::parse(text).unwrap();
            assert_eq!(got, step, "{text}");
            assert_eq!(Step::parse(&got.to_string()).unwrap(), step);
        }
    }

    #[test]
    fn bad_steps_say_why() {
        for text in [
            "",
            "expect",
            "wait 3",
            "password hunter2",
            "secret",
            "secret a b",
            "pause 0",
            "pause 61",
            "pause x",
            "send a\u{1b}b",
        ] {
            assert!(Step::parse(text).is_err(), "{text:?}");
        }
        assert!(Step::parse(&format!("send {}", "x".repeat(MAX_TEXT + 1))).is_err());
    }

    #[test]
    fn snippets_filter_by_words() {
        let s = Snippet {
            name: "Show interfaces".into(),
            lines: vec!["show ip interface brief".into()],
        };
        assert!(s.matches(""));
        assert!(s.matches("INTER"));
        assert!(s.matches("brief show"));
        assert!(!s.matches("vlan"));
    }
}
