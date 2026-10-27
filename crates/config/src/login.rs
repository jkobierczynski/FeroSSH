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
//! - `type TEXT` types `TEXT` without Enter
//! - `capture PATTERN` waits, like `expect`, for text that fits
//!   `PATTERN`, whose one `*` stands for the value to keep (to the end of
//!   the line when nothing follows it): `capture Version *,` keeps what
//!   follows `Version ` up to the comma. Snippets run on several panes
//!   show the values in a live report.
//! - `key NAME` presses a key: `enter`, `space`, `tab`, `escape`,
//!   `backspace`, or `ctrl+` a letter or one of `@ [ \ ] ^ _`
//!
//! A snippet is a named list of command lines, typed into a pane from the
//! snippets view, by a hotkey, or by itself when the host prints a text
//! (an auto-response). A line that starts with `@` and a step's verb is
//! that step (`@expect (config)#`, `@pause 2`); `@@` types a plain `@`.
//! Lines may hold arguments (`$1`) and variables (`${host}`, see
//! [`VARIABLES`]).

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
    /// Types the text without Enter.
    Type(String),
    /// Presses a key, by its name (see [`key_bytes`]).
    Key(String),
    /// Waits for text that fits the pattern and keeps the part its `*`
    /// stands for (see [`capture`]).
    Capture(String),
}

/// The longest value a `capture` keeps, in characters.
pub const MAX_VALUE: usize = 80;

/// Finds what `pattern` (text around one `*`) captures in `seen`: the end
/// of the match, in bytes, and the value, trimmed, one line of at most
/// [`MAX_VALUE`] characters. With nothing after the `*`, the value runs
/// to the end of the line, so it waits for the line to end.
pub fn capture(seen: &str, pattern: &str) -> Option<(usize, String)> {
    let (before, after) = pattern.split_once('*')?;
    if before.is_empty() {
        return None;
    }
    let mut from = 0;
    loop {
        let at = from + seen.get(from..)?.find(before)?;
        let start = at + before.len();
        let rest = seen.get(start..)?;
        let end = if after.is_empty() {
            rest.find(['\r', '\n'])
        } else {
            // Within the line only.
            let eol = rest.find(['\r', '\n']);
            rest.find(after).filter(|&e| eol.is_none_or(|l| e <= l))
        };
        match end {
            Some(e) => {
                let value: String = rest
                    .get(..e)?
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>()
                    .trim()
                    .chars()
                    .take(MAX_VALUE)
                    .collect();
                return Some((start + e + after.len(), value));
            }
            // The line ended without the text after `*`: try a later
            // match.
            None if !after.is_empty() && rest.contains(['\r', '\n']) => {
                from = start;
            }
            None => return None,
        }
    }
}

/// What key `name` types, if FeroSSH knows it.
pub fn key_bytes(name: &str) -> Option<Vec<u8>> {
    let name = name.to_ascii_lowercase();
    let byte = match name.as_str() {
        "enter" | "return" => b'\r',
        "space" => b' ',
        "tab" => b'\t',
        "escape" | "esc" => 0x1b,
        "backspace" => 0x7f,
        _ => {
            let rest = name.strip_prefix("ctrl+")?;
            let mut chars = rest.chars();
            let c = chars.next()?;
            if chars.next().is_some()
                || !(c.is_ascii_lowercase() || matches!(c, '@' | '[' | '\\' | ']' | '^' | '_'))
            {
                return None;
            }
            c.to_ascii_uppercase() as u8 & 0x1f
        }
    };
    Some(vec![byte])
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
    /// Whether the step waits (for the host, a moment or a secret), so it
    /// needs a script runner rather than one write.
    pub fn waits(&self) -> bool {
        matches!(
            self,
            Step::Expect(_) | Step::Capture(_) | Step::Pause(_) | Step::Password | Step::Secret(_)
        )
    }

    /// What a step that does not wait types.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        match self {
            Step::Send(t) => {
                let mut b = t.as_bytes().to_vec();
                b.push(b'\r');
                Some(b)
            }
            Step::Type(t) => Some(t.as_bytes().to_vec()),
            Step::Key(k) => key_bytes(k),
            _ => None,
        }
    }

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
            "type" if rest.is_empty() => Err("type needs the text to type.".into()),
            "type" => Ok(Step::Type(rest.to_owned())),
            "capture" if rest.matches('*').count() == 1 && !rest.starts_with('*') => {
                Ok(Step::Capture(rest.to_owned()))
            }
            "capture" => Err(
                "capture needs text, then one * for the value, as in: capture Version *,".into(),
            ),
            "key" if key_bytes(rest).is_some() => Ok(Step::Key(rest.to_ascii_lowercase())),
            "key" => {
                Err("key takes enter, space, tab, escape, backspace or ctrl+ a letter.".into())
            }
            _ => Err(
                "Start a step with expect, capture, send, type, key, password, secret or pause."
                    .into(),
            ),
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
            Step::Type(t) => write!(f, "type {t}"),
            Step::Key(k) => write!(f, "key {k}"),
            Step::Capture(p) => write!(f, "capture {p}"),
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

/// The most hotkeys: Ctrl+Shift+F1 to F12.
pub const MAX_HOTKEY: u8 = 12;
/// The shortest auto-response text, so a stray character never sets one
/// off.
pub const MIN_WHEN: usize = 2;

/// A named list of command lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snippet {
    pub name: String,
    pub lines: Vec<String>,
    /// Only for these hosts: `Host`-style patterns, matched against the
    /// session's name and its host name. Empty, with `folders`, is every
    /// host.
    pub hosts: Vec<String>,
    /// Only for the sessions in these folders of the session manager.
    pub folders: Vec<String>,
    /// Ctrl+Shift+F`n` types it.
    pub hotkey: Option<u8>,
    /// Typed by itself when the host prints this text (an
    /// auto-response); empty for none.
    pub when: String,
}

/// Where a pane is connected, for the snippets that apply there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    /// The saved session's name, or what was typed.
    pub session: String,
    /// The host name it really goes to.
    pub host: String,
    /// Its folder in the session manager.
    pub folder: Option<String>,
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

    /// Whether it is for every host.
    pub fn everywhere(&self) -> bool {
        self.hosts.is_empty() && self.folders.is_empty()
    }

    /// Whether it applies in a pane at `place` (`None`: no host, such as
    /// a local shell, where only snippets for every host apply).
    pub fn applies(&self, place: Option<&Place>) -> bool {
        if self.everywhere() {
            return true;
        }
        let Some(place) = place else {
            return false;
        };
        let by_host = [place.session.as_str(), place.host.as_str()]
            .iter()
            .filter(|n| !n.is_empty())
            .any(|n| crate::match_host(n, self.hosts.iter().map(String::as_str)));
        let by_folder = place.folder.as_deref().is_some_and(|f| {
            self.folders
                .iter()
                .any(|p| crate::match_pattern(&f.to_lowercase(), &p.to_lowercase()))
        });
        by_host || by_folder
    }

    /// Why it cannot be an auto-response, if it cannot: one types only
    /// what the user wrote, at once, and nothing the host could fish for.
    pub fn auto_problem(&self) -> Option<String> {
        if self.when.is_empty() {
            return None;
        }
        if self.when.trim().chars().count() < MIN_WHEN {
            return Some(format!(
                "An auto-response waits for at least {MIN_WHEN} characters."
            ));
        }
        if self.when.chars().count() > MAX_TEXT || self.when.chars().any(char::is_control) {
            return Some(format!(
                "An auto-response waits for one line of at most {MAX_TEXT} characters."
            ));
        }
        if !args(&self.lines).is_empty() {
            return Some("An auto-response cannot ask for arguments ($1).".into());
        }
        if uses_var(&self.lines, "clipboard") {
            return Some("An auto-response cannot type the clipboard.".into());
        }
        for l in &self.lines {
            if let Some(Ok(step)) = line_step(l)
                && step.waits()
            {
                return Some(
                    "An auto-response types lines and keys only: no expect, pause, password \
                     or secret."
                        .into(),
                );
            }
        }
        None
    }
}

/// `n` as a hotkey's name.
pub fn hotkey_name(n: u8) -> String {
    format!("Ctrl+Shift+F{n}")
}

/// A hotkey as written: `F3` or `3`; `Ok(None)` when empty.
pub fn parse_hotkey(text: &str) -> Result<Option<u8>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let digits = t.strip_prefix(['F', 'f']).unwrap_or(t);
    match digits.parse::<u8>() {
        Ok(n) if (1..=MAX_HOTKEY).contains(&n) => Ok(Some(n)),
        _ => Err(format!(
            "A hotkey is F1 to F{MAX_HOTKEY} (with Ctrl+Shift), or empty."
        )),
    }
}

/// The step a snippet line stands for: `None` for a plain line, else the
/// step after its `@` (or why it is not one).
pub fn line_step(line: &str) -> Option<Result<Step, String>> {
    let rest = line.trim_start().strip_prefix('@')?;
    if rest.starts_with('@') {
        return None;
    }
    Some(Step::parse(rest).map_err(|e| format!("{e} A line that starts with @@ types a plain @.")))
}

/// A plain line's text: `@@` is a plain `@`.
fn line_text(line: &str) -> String {
    let trimmed = line.trim_start();
    match trimmed.strip_prefix("@@") {
        Some(rest) => {
            let indent = line.get(..line.len() - trimmed.len()).unwrap_or_default();
            format!("{indent}@{rest}")
        }
        None => line.to_owned(),
    }
}

/// The variables a snippet line may hold, as `${name}`: the session's
/// name, host name, user and port, today's date, the time, a date and
/// time for file names, and the text on the clipboard. Other `${...}`
/// text stays as it is, so a shell's `${HOME}` still works.
pub const VARIABLES: [&str; 8] = [
    "session",
    "host",
    "user",
    "port",
    "date",
    "time",
    "stamp",
    "clipboard",
];

/// A snippet argument: `$1` to `$9` in a line, or `${1:default}` with
/// the text it stands for when nothing else is typed. `$$` is a plain `$`,
/// so `awk '{print $$1}'` types `awk '{print $1}'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arg {
    pub number: u8,
    /// The first default the lines give it, or empty.
    pub default: String,
}

/// One piece of a snippet line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Arg(u8, Option<String>),
    Var(&'static str),
}

fn pieces(line: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = line;
    while let Some(at) = rest.find('$') {
        text.push_str(rest.get(..at).unwrap_or_default());
        let after = rest.get(at + 1..).unwrap_or_default();
        let mut chars = after.chars();
        match chars.next() {
            Some('$') => {
                text.push('$');
                rest = chars.as_str();
            }
            Some(d @ '1'..='9') => {
                out.push(Piece::Text(std::mem::take(&mut text)));
                out.push(Piece::Arg(d as u8 - b'0', None));
                rest = chars.as_str();
            }
            Some('{') => {
                let inner = chars.as_str();
                let var = inner.find('}').and_then(|end| {
                    let body = inner.get(..end)?;
                    let name = VARIABLES.iter().find(|v| **v == body)?;
                    Some((*name, inner.get(end + 1..)?))
                });
                if let Some((name, after)) = var {
                    out.push(Piece::Text(std::mem::take(&mut text)));
                    out.push(Piece::Var(name));
                    rest = after;
                    continue;
                }
                let parsed = inner.find('}').and_then(|end| {
                    let body = inner.get(..end)?;
                    let (num, default) = match body.split_once(':') {
                        Some((n, d)) => (n, Some(d.to_owned())),
                        None => (body, None),
                    };
                    let n = num.parse::<u8>().ok().filter(|n| (1..=9).contains(n))?;
                    Some((n, default, inner.get(end + 1..)?))
                });
                match parsed {
                    Some((n, default, after)) => {
                        out.push(Piece::Text(std::mem::take(&mut text)));
                        out.push(Piece::Arg(n, default));
                        rest = after;
                    }
                    None => {
                        text.push('$');
                        rest = after;
                    }
                }
            }
            _ => {
                text.push('$');
                rest = after;
            }
        }
    }
    text.push_str(rest);
    out.push(Piece::Text(text));
    out.retain(|p| *p != Piece::Text(String::new()));
    out
}

/// The arguments `lines` use, by number.
pub fn args(lines: &[String]) -> Vec<Arg> {
    let mut out: Vec<Arg> = Vec::new();
    for piece in lines.iter().flat_map(|l| pieces(l)) {
        if let Piece::Arg(number, default) = piece {
            match out.iter_mut().find(|a| a.number == number) {
                Some(a) if a.default.is_empty() => a.default = default.unwrap_or_default(),
                Some(_) => {}
                None => out.push(Arg {
                    number,
                    default: default.unwrap_or_default(),
                }),
            }
        }
    }
    out.sort_by_key(|a| a.number);
    out
}

/// Whether `lines` use variable `name`.
pub fn uses_var(lines: &[String], name: &str) -> bool {
    lines
        .iter()
        .flat_map(|l| pieces(l))
        .any(|p| matches!(p, Piece::Var(v) if v == name))
}

/// One line of text: control characters in a value become spaces, so a
/// value never adds a line or a key of its own.
fn one_line(value: String) -> String {
    if value.chars().any(char::is_control) {
        value
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect()
    } else {
        value
    }
}

fn fill_text(text: &str, value: &impl Fn(u8) -> String, var: &impl Fn(&str) -> String) -> String {
    pieces(text)
        .into_iter()
        .map(|p| match p {
            Piece::Text(t) => t,
            Piece::Arg(n, _) => one_line(value(n)),
            Piece::Var(name) => one_line(var(name)),
        })
        .collect()
}

/// `lines` with each argument replaced by `value(number)`, each variable
/// by `var(name)`, and `$$` by `$`. What a value holds is typed as it is:
/// a `$1` in it stays `$1`.
pub fn fill(
    lines: &[String],
    value: impl Fn(u8) -> String,
    var: impl Fn(&str) -> String,
) -> Vec<String> {
    lines.iter().map(|l| fill_text(l, &value, &var)).collect()
}

/// What typing a snippet's `lines` does, step by step, with arguments and
/// variables filled in: a plain line is typed with Enter (the last one
/// without, unless `enter`), an `@` line is its step. A line is read as a
/// step before anything is filled in, so no value turns into one.
pub fn expand(
    lines: &[String],
    enter: bool,
    value: impl Fn(u8) -> String,
    var: impl Fn(&str) -> String,
) -> Vec<Step> {
    let mut out: Vec<Step> = lines
        .iter()
        .map(|l| match line_step(l) {
            Some(Ok(step)) => match step {
                Step::Expect(t) => Step::Expect(fill_text(&t, &value, &var)),
                Step::Capture(t) => Step::Capture(fill_text(&t, &value, &var)),
                Step::Send(t) => Step::Send(fill_text(&t, &value, &var)),
                Step::Type(t) => Step::Type(fill_text(&t, &value, &var)),
                other => other,
            },
            // A line that is not a step after all is typed as written.
            Some(Err(_)) => Step::Send(fill_text(l, &value, &var)),
            None => Step::Send(fill_text(&line_text(l), &value, &var)),
        })
        .collect();
    if !enter
        && let Some(last) = out.last_mut()
        && let Step::Send(t) = last
    {
        *last = Step::Type(std::mem::take(t));
    }
    out
}

/// The bytes `steps` type when none of them waits; `None` when one does.
pub fn steps_bytes(steps: &[Step]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for s in steps {
        out.extend(s.bytes()?);
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn owned(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| (*l).to_owned()).collect()
    }

    #[test]
    fn arguments_are_found_with_their_defaults() {
        let lines = owned(&[
            "interface $1",
            "description ${2:uplink to core}",
            "show int ${1:Gi0/1} | include $2",
            "echo $HOME $$1 ${x} ${10} $",
        ]);
        assert_eq!(
            args(&lines),
            [
                Arg {
                    number: 1,
                    default: "Gi0/1".into()
                },
                Arg {
                    number: 2,
                    default: "uplink to core".into()
                },
            ]
        );
        assert!(args(&owned(&["ls -l $HOME", "echo $$5"])).is_empty());
    }

    #[test]
    fn filling_replaces_arguments_once() {
        let lines = owned(&[
            "interface $1",
            "description ${2:uplink}",
            "awk '{print $$1}' ${3:x}$",
        ]);
        let got = fill(
            &lines,
            |n| match n {
                1 => "Gi0/2".into(),
                2 => "$1 stays".into(),
                _ => String::new(),
            },
            |_| String::new(),
        );
        assert_eq!(
            got,
            owned(&[
                "interface Gi0/2",
                "description $1 stays",
                "awk '{print $1}' $"
            ])
        );
        assert_eq!(
            fill(&owned(&["${1:unclosed"]), |_| "v".into(), |_| String::new()),
            ["${1:unclosed"]
        );
    }

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
            ..Snippet::default()
        };
        assert!(s.matches(""));
        assert!(s.matches("INTER"));
        assert!(s.matches("brief show"));
        assert!(!s.matches("vlan"));
    }

    fn vars(name: &str) -> String {
        match name {
            "host" => "sw1.lab".into(),
            "stamp" => "20261027-2049".into(),
            "clipboard" => "two\nlines".into(),
            _ => String::new(),
        }
    }

    #[test]
    fn variables_fill_in_and_others_stay() {
        let lines = owned(&[
            "copy run tftp://10.0.0.5/${host}-${stamp}.cfg",
            "echo ${HOME} $${host} ${clipboard}",
        ]);
        assert!(uses_var(&lines, "clipboard"));
        assert!(!uses_var(&lines, "user"));
        assert!(args(&lines).is_empty());
        assert_eq!(
            fill(&lines, |_| String::new(), vars),
            owned(&[
                "copy run tftp://10.0.0.5/sw1.lab-20261027-2049.cfg",
                "echo ${HOME} ${host} two lines",
            ]),
            "a value never adds a line"
        );
    }

    #[test]
    fn snippets_expand_to_steps() {
        let lines = owned(&[
            "conf t",
            "@expect (config)#",
            "interface $1",
            "@pause 1",
            "@@home",
            "@key ctrl+z",
            "@nonsense",
            "  @@x",
            "end",
        ]);
        let steps = expand(&lines, false, |_| "Gi0/1".into(), vars);
        assert_eq!(
            steps,
            [
                Step::Send("conf t".into()),
                Step::Expect("(config)#".into()),
                Step::Send("interface Gi0/1".into()),
                Step::Pause(1),
                Step::Send("@home".into()),
                Step::Key("ctrl+z".into()),
                Step::Send("@nonsense".into()),
                Step::Send("  @x".into()),
                Step::Type("end".into()),
            ]
        );
        assert_eq!(steps_bytes(&steps), None, "it waits");
        // A value that looks like a step is still typed as text.
        let steps = expand(&owned(&["$1"]), true, |_| "@password".into(), vars);
        assert_eq!(steps, [Step::Send("@password".into())]);
        let steps = expand(
            &owned(&["a", "@key space", "@type b c"]),
            true,
            |_| String::new(),
            vars,
        );
        assert_eq!(steps_bytes(&steps).unwrap(), b"a\r b c");
        assert!(line_step("@expct x").unwrap().is_err());
    }

    #[test]
    fn keys_have_bytes() {
        for (name, byte) in [
            ("enter", b'\r'),
            ("Space", b' '),
            ("tab", b'\t'),
            ("esc", 0x1b),
            ("backspace", 0x7f),
            ("ctrl+c", 0x03),
            ("CTRL+Z", 0x1a),
            ("ctrl+^", 0x1e),
            ("ctrl+@", 0x00),
        ] {
            assert_eq!(key_bytes(name), Some(vec![byte]), "{name}");
        }
        for name in ["", "ctrl+", "ctrl+ab", "ctrl+1", "f1", "up"] {
            assert_eq!(key_bytes(name), None, "{name}");
        }
        assert_eq!(Step::parse("key Enter").unwrap(), Step::Key("enter".into()));
        assert_eq!(Step::parse("type  y ").unwrap(), Step::Type("y".into()));
        assert!(Step::parse("key f1").is_err());
        assert!(Step::parse("type").is_err());
    }

    #[test]
    fn snippets_apply_by_host_or_folder() {
        let place = Place {
            session: "core-sw".into(),
            host: "10.0.0.1".into(),
            folder: Some("Network".into()),
        };
        let mut s = Snippet::default();
        assert!(s.applies(None) && s.applies(Some(&place)));
        s.hosts = vec!["*-sw".into()];
        assert!(s.applies(Some(&place)));
        assert!(!s.applies(None), "a local shell has no host");
        s.hosts = vec!["10.0.0.*".into()];
        assert!(s.applies(Some(&place)), "the host name counts too");
        s.hosts = vec!["web*".into()];
        assert!(!s.applies(Some(&place)));
        s.folders = vec!["network".into()];
        assert!(s.applies(Some(&place)));
    }

    #[test]
    fn auto_responses_type_only_what_was_written() {
        let auto = |when: &str, lines: &[&str]| Snippet {
            name: "a".into(),
            lines: owned(lines),
            when: when.into(),
            ..Snippet::default()
        };
        assert_eq!(auto("--More--", &["@key space"]).auto_problem(), None);
        assert_eq!(auto("", &["@password"]).auto_problem(), None, "not one");
        for bad in [
            auto("x", &["y"]),
            auto("[confirm]", &["$1"]),
            auto("[confirm]", &["${clipboard}"]),
            auto("Password:", &["@password"]),
            auto("Password:", &["@secret enable"]),
            auto("a#", &["@expect b"]),
            auto("a#", &["@pause 1"]),
        ] {
            assert!(bad.auto_problem().is_some(), "{bad:?}");
        }
    }

    #[test]
    fn hotkeys_parse() {
        assert_eq!(parse_hotkey(""), Ok(None));
        assert_eq!(parse_hotkey(" f3 "), Ok(Some(3)));
        assert_eq!(parse_hotkey("12"), Ok(Some(12)));
        assert!(parse_hotkey("F0").is_err());
        assert!(parse_hotkey("F13").is_err());
        assert!(parse_hotkey("x").is_err());
        assert_eq!(hotkey_name(3), "Ctrl+Shift+F3");
    }

    #[test]
    fn captures_keep_the_value() {
        let out = "show version\r\nCisco IOS Software, Version 15.2(4)E10, RELEASE\r\nsw1#";
        let (end, v) = capture(out, "Version *,").unwrap();
        assert_eq!(v, "15.2(4)E10");
        assert!(out[end..].starts_with(" RELEASE"));
        assert_eq!(
            capture("uptime is 3 weeks\r\n", "uptime is *").unwrap().1,
            "3 weeks"
        );
        assert_eq!(
            capture("uptime is 3 we", "uptime is *"),
            None,
            "waits for the line"
        );
        // The first line has no comma: a later one does.
        assert_eq!(
            capture("Version x\r\nVersion y, z", "Version *,")
                .unwrap()
                .1,
            "y"
        );
        assert_eq!(capture("a: \x07b\tc\n", "a: *").unwrap().1, "b c");
        let long = format!("v={}\n", "x".repeat(500));
        assert_eq!(capture(&long, "v=*").unwrap().1.len(), MAX_VALUE);
        assert_eq!(
            Step::parse("capture Version *,").unwrap(),
            Step::Capture("Version *,".into())
        );
        for bad in ["capture", "capture x", "capture *x", "capture a*b*"] {
            assert!(Step::parse(bad).is_err(), "{bad}");
        }
        assert!(Step::Capture("a*".into()).waits());
    }
}
