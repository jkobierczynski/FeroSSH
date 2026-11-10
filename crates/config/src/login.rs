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
/// The shortest and longest `delay`, in milliseconds.
pub const MIN_DELAY_MS: u32 = 10;
pub const MAX_DELAY_MS: u32 = MAX_PAUSE_SECS * 1000;

/// A `delay`'s length: milliseconds (`300`, `300ms`) or seconds (`1.5s`).
fn delay_ms(text: &str) -> Option<u32> {
    let t = text.trim().to_ascii_lowercase().replace(' ', "");
    let ms = if let Some(n) = t.strip_suffix("ms") {
        n.parse::<f64>().ok()?
    } else if let Some(n) = t.strip_suffix('s') {
        n.parse::<f64>().ok()? * 1000.0
    } else {
        t.parse::<f64>().ok()?
    };
    if !ms.is_finite() || !(f64::from(MIN_DELAY_MS)..=f64::from(MAX_DELAY_MS)).contains(&ms) {
        return None;
    }
    Some(ms.round() as u32)
}
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
    /// Waits this many milliseconds: a `delay`, for timing between keys
    /// and lines finer than a `pause`.
    Delay(u32),
    /// In a snippet: moves FeroSSH's focus to the pane `left`, `right`,
    /// `up` or `down` of this one; the steps after it go to that pane.
    Pane(String),
    /// In a snippet: moves to the `next` or `prev` tab; the steps after
    /// it go to that tab's focused pane.
    Tab(String),
    /// Types the text without Enter.
    Type(String),
    /// Presses a key, by its name (see [`key_bytes`]).
    Key(String),
    /// Waits for text that fits the pattern and keeps the part its `*`
    /// stands for (see [`capture`]).
    Capture(String),
    /// A place a `goto` jumps to; does nothing itself.
    Label(String),
    /// Goes on at the label.
    Goto(String),
    /// One of several texts to wait for: the `on` steps in a row wait
    /// together, and the first text that shows up goes to its label (or
    /// on after the last of them, without one).
    On(String, Option<String>),
    /// How many seconds the `expect`, `capture` and `on` steps after it
    /// wait, and the label to go to when one times out (else the script
    /// fails).
    Timeout(u32, Option<String>),
    /// Sets a variable, used as `${name}`, to the text.
    Set(String, String),
    /// Adds a whole number to a variable (an unset one is 0).
    Add(String, i64),
    /// Goes to the label when the condition holds.
    If(Cond),
    /// Shows a line in the pane, not typed at the host.
    Show(String),
    /// Ends the script, as finished.
    Stop,
    /// Ends the script as failed, with this reason.
    Fail(String),
}

/// How an `if` compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
}

impl Op {
    /// The operators, as written, longest first so `<=` is not read as
    /// `<`.
    const ALL: [(&'static str, Op); 7] = [
        ("contains", Op::Contains),
        ("==", Op::Eq),
        ("!=", Op::Ne),
        ("<=", Op::Le),
        (">=", Op::Ge),
        ("<", Op::Lt),
        (">", Op::Gt),
    ];

    /// How it is written.
    pub fn text(self) -> &'static str {
        Op::ALL
            .iter()
            .find(|(_, o)| *o == self)
            .map_or("==", |(t, _)| t)
    }
}

/// An `if` step: `if LEFT OP RIGHT goto LABEL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cond {
    pub left: String,
    pub op: Op,
    pub right: String,
    pub label: String,
}

impl Cond {
    /// Whether `op` holds for these (filled in) sides: `<`, `<=`, `>`
    /// and `>=` compare whole numbers, `==` and `!=` numbers when both
    /// sides are, else text; `contains` looks for the right side in the
    /// left. `Err` when a number comparison has a side that is not one.
    pub fn holds(op: Op, left: &str, right: &str) -> Result<bool, String> {
        let nums = (left.trim().parse::<i64>(), right.trim().parse::<i64>());
        Ok(match (op, nums) {
            (Op::Contains, _) => left.contains(right),
            (Op::Eq, (Ok(a), Ok(b))) => a == b,
            (Op::Ne, (Ok(a), Ok(b))) => a != b,
            (Op::Eq, _) => left == right,
            (Op::Ne, _) => left != right,
            (Op::Lt, (Ok(a), Ok(b))) => a < b,
            (Op::Le, (Ok(a), Ok(b))) => a <= b,
            (Op::Gt, (Ok(a), Ok(b))) => a > b,
            (Op::Ge, (Ok(a), Ok(b))) => a >= b,
            (op, _) => {
                return Err(format!(
                    "{} compares whole numbers, not \"{left}\" and \"{right}\".",
                    op.text()
                ));
            }
        })
    }
}

/// The longest label or variable name.
pub const MAX_NAME: usize = 32;
/// The longest `timeout`, in seconds.
pub const MAX_TIMEOUT_SECS: u32 = 3600;

/// Whether `name` can name a label or a variable: letters, digits, `_`
/// and `-`, starting with a letter or `_`, at most [`MAX_NAME`] long.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.len() <= MAX_NAME
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn name_error(what: &str) -> String {
    format!("{what} needs a label of letters, digits, _ and - (at most {MAX_NAME}).")
}

/// A text with `goto LABEL` at its end split off: `("text", Some("x"))`.
fn split_goto(text: &str) -> Result<(&str, Option<String>), String> {
    let lower = text.to_ascii_lowercase();
    let at = if lower.starts_with("goto ") {
        Some(0)
    } else {
        lower.rfind(" goto ").map(|i| i + 1)
    };
    let Some(at) = at else {
        return Ok((text, None));
    };
    let label = text.get(at + 5..).unwrap_or_default().trim();
    if !valid_name(label) {
        return Err(name_error("goto"));
    }
    Ok((
        text.get(..at).unwrap_or_default().trim_end(),
        Some(label.to_owned()),
    ))
}

/// A side of an `if` or a `set` value: `"..."` around it keeps spaces
/// and allows an empty one.
fn operand(text: &str) -> String {
    let t = text.trim();
    t.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(t)
        .to_owned()
}

/// Puts `"` around a side that needs them to read back the same.
fn quote(text: &str) -> String {
    if text.is_empty() || text != text.trim() || text.starts_with('"') {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    }
}

fn parse_if(rest: &str) -> Result<Cond, String> {
    const HOW: &str = "if needs a comparison and a label: if ${count} < 5 goto again, \
                       if ${model} contains 9300 goto new.";
    let (cond, label) = split_goto(rest)?;
    let label = label.ok_or(HOW)?;
    let lower = cond.to_ascii_lowercase();
    for (text, op) in Op::ALL {
        let spaced = format!(" {text} ");
        if let Some(at) = lower.find(&spaced) {
            return Ok(Cond {
                left: operand(cond.get(..at).unwrap_or_default()),
                op,
                right: operand(cond.get(at + spaced.len()..).unwrap_or_default()),
                label,
            });
        }
    }
    Err(HOW.into())
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

/// The most times one key in a `key` step is pressed.
pub const MAX_KEY_REPEAT: usize = 50;

/// A key that sends an escape sequence, which carries Shift, Alt and
/// Ctrl as xterm's modifier number.
enum Special {
    /// `ESC [ final`, or `ESC [ 1 ; m final` with modifiers.
    Csi(char),
    /// `ESC O final` alone (F1 to F4), `ESC [ 1 ; m final` with modifiers.
    Ss3(char),
    /// `ESC [ n ~`, or `ESC [ n ; m ~` with modifiers.
    Tilde(u8),
}

fn special(name: &str) -> Option<Special> {
    Some(match name {
        "up" => Special::Csi('A'),
        "down" => Special::Csi('B'),
        "right" => Special::Csi('C'),
        "left" => Special::Csi('D'),
        "home" => Special::Csi('H'),
        "end" => Special::Csi('F'),
        "insert" | "ins" => Special::Tilde(2),
        "delete" | "del" => Special::Tilde(3),
        "pageup" | "pgup" => Special::Tilde(5),
        "pagedown" | "pgdn" => Special::Tilde(6),
        "f1" => Special::Ss3('P'),
        "f2" => Special::Ss3('Q'),
        "f3" => Special::Ss3('R'),
        "f4" => Special::Ss3('S'),
        "f5" => Special::Tilde(15),
        "f6" => Special::Tilde(17),
        "f7" => Special::Tilde(18),
        "f8" => Special::Tilde(19),
        "f9" => Special::Tilde(20),
        "f10" => Special::Tilde(21),
        "f11" => Special::Tilde(23),
        "f12" => Special::Tilde(24),
        _ => return None,
    })
}

/// The byte a plain key sends.
fn plain_byte(name: &str) -> Option<u8> {
    Some(match name {
        "enter" | "return" => b'\r',
        "space" => b' ',
        "tab" => b'\t',
        "escape" | "esc" => 0x1b,
        "backspace" => 0x7f,
        _ => return None,
    })
}

const SHIFT: u8 = 1;
const ALT: u8 = 2;
const CTRL: u8 = 4;

/// What one key `name` types, if FeroSSH knows it: Enter, Space, Tab,
/// Escape, Backspace, the arrows, Home, End, Insert, Delete, Page Up and
/// Down and F1 to F12, each with any of `ctrl+`, `alt+` and `shift+` in
/// front (`alt+right`, `ctrl+shift+f5`), and Ctrl+ a letter (or
/// `@[\]^_`) or Alt+ a character. Keys that send escape sequences send
/// what xterm does in its normal modes.
fn one_key(name: &str) -> Option<Vec<u8>> {
    let mut mods = 0;
    let mut rest = name;
    loop {
        if let Some(r) = rest.strip_prefix("ctrl+").filter(|r| !r.is_empty()) {
            mods |= CTRL;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("alt+").filter(|r| !r.is_empty()) {
            mods |= ALT;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("shift+").filter(|r| !r.is_empty()) {
            mods |= SHIFT;
            rest = r;
        } else {
            break;
        }
    }
    if let Some(key) = special(rest) {
        let m = 1 + mods;
        return Some(
            match (key, mods) {
                (Special::Csi(f), 0) => format!("\x1b[{f}"),
                (Special::Ss3(f), 0) => format!("\x1bO{f}"),
                (Special::Csi(f) | Special::Ss3(f), _) => format!("\x1b[1;{m}{f}"),
                (Special::Tilde(n), 0) => format!("\x1b[{n}~"),
                (Special::Tilde(n), _) => format!("\x1b[{n};{m}~"),
            }
            .into_bytes(),
        );
    }
    if mods == SHIFT && rest == "tab" {
        return Some(b"\x1b[Z".to_vec());
    }
    // Ctrl+ a letter is one control byte.
    let ctrl_byte = |text: &str| {
        let mut chars = text.chars();
        let c = chars.next()?;
        if chars.next().is_some()
            || !(c.is_ascii_lowercase() || matches!(c, '@' | '[' | '\\' | ']' | '^' | '_'))
        {
            return None;
        }
        Some(c.to_ascii_uppercase() as u8 & 0x1f)
    };
    let one_char = |text: &str| {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if !c.is_control() => {
                let mut buf = [0; 4];
                Some(c.encode_utf8(&mut buf).as_bytes().to_vec())
            }
            _ => None,
        }
    };
    let base = match mods & !ALT {
        0 => match plain_byte(rest) {
            Some(b) => vec![b],
            // A lone character is not a key name, but Alt+ one is.
            None if mods == ALT => one_char(rest)?,
            None => return None,
        },
        CTRL => vec![ctrl_byte(rest)?],
        _ => return None,
    };
    // Alt sends Escape first.
    let mut out = Vec::new();
    if mods & ALT != 0 {
        out.push(0x1b);
    }
    out.extend(base);
    Some(out)
}

/// What a `key` step's text types, if FeroSSH knows every key in it:
/// key names (see [`one_key`]) with spaces between, each one optionally
/// followed by how many times to press it, or single characters, which
/// type themselves: `down 3 enter` presses Down three times and then
/// Enter, and `ctrl+b d` is tmux's detach.
pub fn key_bytes(name: &str) -> Option<Vec<u8>> {
    let name = name.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut last: Option<Vec<u8>> = None;
    for word in name.split_whitespace() {
        if let Ok(n) = word.parse::<usize>() {
            let key = last.take()?;
            if !(1..=MAX_KEY_REPEAT).contains(&n) {
                return None;
            }
            for _ in 1..n {
                out.extend_from_slice(&key);
            }
            continue;
        }
        let mut chars = word.chars();
        let key = match (chars.next(), chars.next()) {
            // One character types itself.
            (Some(c), None) => {
                let mut buf = [0; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
            _ => one_key(word)?,
        };
        out.extend_from_slice(&key);
        last = Some(key);
    }
    (!out.is_empty()).then_some(out)
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
            Step::Expect(_)
                | Step::Capture(_)
                | Step::Pause(_)
                | Step::Delay(_)
                | Step::Password
                | Step::Secret(_)
                | Step::Pane(_)
                | Step::Tab(_)
        ) || self.is_flow()
    }

    /// Whether it is script logic (labels, jumps, waits for one of
    /// several texts, timeouts, variables, messages, stop and fail),
    /// which only a script runner carries out.
    pub fn is_flow(&self) -> bool {
        matches!(
            self,
            Step::Label(_)
                | Step::Goto(_)
                | Step::On(..)
                | Step::Timeout(..)
                | Step::Set(..)
                | Step::Add(..)
                | Step::If(_)
                | Step::Show(_)
                | Step::Stop
                | Step::Fail(_)
        )
    }

    /// The label it may go to.
    pub fn target(&self) -> Option<&str> {
        match self {
            Step::Goto(l) | Step::On(_, Some(l)) | Step::Timeout(_, Some(l)) => Some(l),
            Step::If(c) => Some(&c.label),
            _ => None,
        }
    }

    /// The step with `f` applied to each text it types, waits for,
    /// compares, sets or shows (not to names or labels).
    pub fn map_text(self, f: impl Fn(&str) -> String) -> Step {
        match self {
            Step::Expect(t) => Step::Expect(f(&t)),
            Step::Capture(t) => Step::Capture(f(&t)),
            Step::Send(t) => Step::Send(f(&t)),
            Step::Type(t) => Step::Type(f(&t)),
            Step::On(t, l) => Step::On(f(&t), l),
            Step::Set(n, v) => Step::Set(n, f(&v)),
            Step::If(c) => Step::If(Cond {
                left: f(&c.left),
                right: f(&c.right),
                ..c
            }),
            Step::Show(t) => Step::Show(f(&t)),
            Step::Fail(t) => Step::Fail(f(&t)),
            other => other,
        }
    }

    /// Whether it moves to another pane or tab.
    pub fn moves(&self) -> bool {
        matches!(self, Step::Pane(_) | Step::Tab(_))
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
                // With a unit (500ms, 1.5s) it is a delay.
                _ if rest.trim().ends_with(['s', 'S']) => {
                    delay_ms(rest).map(Step::Delay).ok_or_else(|| {
                        format!(
                            "pause takes seconds (1 to {MAX_PAUSE_SECS}) or a time such as \
                             500ms or 1.5s, from {MIN_DELAY_MS}ms."
                        )
                    })
                }
                _ => Err(format!(
                    "pause takes seconds (1 to {MAX_PAUSE_SECS}) or a time such as 500ms or 1.5s."
                )),
            },
            "pane" => match rest.to_ascii_lowercase().as_str() {
                d @ ("left" | "right" | "up" | "down") => Ok(Step::Pane(d.to_owned())),
                _ => Err("pane takes left, right, up or down.".into()),
            },
            "tab" => match rest.to_ascii_lowercase().as_str() {
                "next" => Ok(Step::Tab("next".into())),
                "prev" | "previous" => Ok(Step::Tab("prev".into())),
                _ => Err("tab takes next or prev.".into()),
            },
            "delay" => delay_ms(rest).map(Step::Delay).ok_or_else(|| {
                format!(
                    "delay takes milliseconds or seconds, from {MIN_DELAY_MS}ms to \
                     {MAX_PAUSE_SECS}s: delay 300ms, delay 1.5s."
                )
            }),
            "type" if rest.is_empty() => Err("type needs the text to type.".into()),
            "type" => Ok(Step::Type(rest.to_owned())),
            "capture" if rest.matches('*').count() == 1 && !rest.starts_with('*') => {
                Ok(Step::Capture(rest.to_owned()))
            }
            "capture" => Err(
                "capture needs text, then one * for the value, as in: capture Version *,".into(),
            ),
            "key" if key_bytes(rest).is_some() => Ok(Step::Key(rest.to_ascii_lowercase())),
            "label" if valid_name(rest) => Ok(Step::Label(rest.to_owned())),
            "label" => Err(name_error("label")),
            "goto" => match split_goto(line) {
                Ok((_, Some(l))) => Ok(Step::Goto(l)),
                _ => Err(name_error("goto")),
            },
            "on" => {
                let (text, label) = split_goto(rest)?;
                if text.is_empty() {
                    Err("on needs the text to wait for: on (yes/no)? goto hostkey.".into())
                } else {
                    Ok(Step::On(text.to_owned(), label))
                }
            }
            "timeout" => {
                let (secs, label) = split_goto(rest)?;
                match secs.trim().parse::<u32>() {
                    Ok(n) if (1..=MAX_TIMEOUT_SECS).contains(&n) => Ok(Step::Timeout(n, label)),
                    _ => Err(format!(
                        "timeout takes seconds (1 to {MAX_TIMEOUT_SECS}), optionally followed by \
                         goto and a label: timeout 60 goto slow."
                    )),
                }
            }
            v @ ("set" | "add") => {
                let (name, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let name = name
                    .strip_prefix("${")
                    .and_then(|n| n.strip_suffix('}'))
                    .unwrap_or(name);
                if !valid_name(name) {
                    return Err(format!(
                        "{v} needs a variable name of letters, digits, _ and - first."
                    ));
                }
                if VARIABLES.contains(&name) || name == "captured" && v == "add" {
                    return Err(format!(
                        "${{{name}}} is filled in by FeroSSH; pick another name."
                    ));
                }
                let value = value.trim();
                if v == "set" {
                    Ok(Step::Set(name.to_owned(), operand(value)))
                } else {
                    match value.parse::<i64>() {
                        Ok(n) if n.unsigned_abs() <= 1_000_000 => Ok(Step::Add(name.to_owned(), n)),
                        _ => Err("add takes a variable and a whole number: add tries 1.".into()),
                    }
                }
            }
            "if" => parse_if(rest).map(Step::If),
            "show" => Ok(Step::Show(rest.to_owned())),
            "stop" if rest.is_empty() => Ok(Step::Stop),
            "stop" => Err("stop takes nothing after it; fail takes a reason.".into()),
            "fail" if rest.is_empty() => Ok(Step::Fail("the script said so.".into())),
            "fail" => Ok(Step::Fail(rest.to_owned())),
            "key" => Err(
                "key takes key names such as enter, tab, up, f5, ctrl+c, alt+right or shift+tab, \
                     each one optionally followed by a count: down 3."
                    .into(),
            ),
            _ => Err(
                "Start a step with expect, on, capture, send, type, key, password, secret, pause, \
                 delay, timeout, label, goto, if, set, add, show, stop, fail, pane or tab."
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
            Step::Delay(ms) => write!(f, "delay {ms}ms"),
            Step::Pane(d) => write!(f, "pane {d}"),
            Step::Tab(d) => write!(f, "tab {d}"),
            Step::Type(t) => write!(f, "type {t}"),
            Step::Key(k) => write!(f, "key {k}"),
            Step::Capture(p) => write!(f, "capture {p}"),
            Step::Label(l) => write!(f, "label {l}"),
            Step::Goto(l) => write!(f, "goto {l}"),
            Step::On(t, None) => write!(f, "on {t}"),
            Step::On(t, Some(l)) => write!(f, "on {t} goto {l}"),
            Step::Timeout(n, None) => write!(f, "timeout {n}"),
            Step::Timeout(n, Some(l)) => write!(f, "timeout {n} goto {l}"),
            Step::Set(n, v) => write!(f, "set {n} {}", quote(v)),
            Step::Add(n, v) => write!(f, "add {n} {v}"),
            Step::If(c) => write!(
                f,
                "if {} {} {} goto {}",
                quote(&c.left),
                c.op.text(),
                quote(&c.right),
                c.label
            ),
            Step::Show(t) => write!(f, "show {t}"),
            Step::Stop => f.write_str("stop"),
            Step::Fail(t) => write!(f, "fail {t}"),
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
    /// The category it is listed under, `/` between levels
    /// (`Network/Cisco`); empty for none. It is the folder its file is
    /// in, under the snippets folder (see [`crate::snippet_store`]).
    pub category: String,
    /// The file it was read from or last written to, relative to the
    /// snippets folder with `/` between parts; `None` for one never
    /// saved.
    pub file: Option<String>,
    pub lines: Vec<String>,
    /// Only for these hosts: `Host`-style patterns, matched against the
    /// session's name and its host name. Empty, with `folders`, is every
    /// host.
    pub hosts: Vec<String>,
    /// Only for the sessions in these folders of the connection manager.
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
    /// Its folder in the connection manager.
    pub folder: Option<String>,
}

impl Snippet {
    /// Whether `filter`'s words all appear in the name, the category or
    /// the lines, ignoring case.
    pub fn matches(&self, filter: &str) -> bool {
        let hay = format!(
            "{}\n{}\n{}",
            self.name,
            self.category,
            self.lines.join("\n")
        )
        .to_lowercase();
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
                    "An auto-response types lines and keys only: no expect, pause, password, \
                     secret or script logic (on, goto, if, set...)."
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
            Some(Ok(step)) => step.map_text(|t| fill_text(t, &value, &var)),
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

/// Whether `steps` hang together as a script: every label is named
/// once, every `goto`, `on`, `timeout` and `if` goes to one of them, and
/// no `pane` or `tab` move mixes with jumps. `Err` holds the index of the
/// step at fault and why.
pub fn check_flow(steps: &[Step]) -> Result<(), (usize, String)> {
    let mut labels: Vec<&str> = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        if let Step::Label(l) = s {
            if labels.iter().any(|x| x.eq_ignore_ascii_case(l)) {
                return Err((i, format!("The label {l} is used twice.")));
            }
            labels.push(l);
        }
    }
    let jumps = steps.iter().any(|s| s.target().is_some());
    for (i, s) in steps.iter().enumerate() {
        if let Some(t) = s.target()
            && !labels.iter().any(|x| x.eq_ignore_ascii_case(t))
        {
            return Err((i, format!("There is no label {t} to go to.")));
        }
        if jumps && s.moves() {
            return Err((
                i,
                "A script that jumps (goto, on, if, timeout) cannot also move to another pane \
                 or tab."
                    .into(),
            ));
        }
    }
    Ok(())
}

/// [`check_flow`] for a script's lines: `Err` holds the line's index.
pub fn check_lines(lines: &[String]) -> Result<(), (usize, String)> {
    let mut at = Vec::new();
    let mut steps = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(Ok(step)) = line_step(l) {
            at.push(i);
            steps.push(step);
        }
    }
    check_flow(&steps).map_err(|(i, why)| (at.get(i).copied().unwrap_or(0), why))
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
            ("delay 300ms", Step::Delay(300)),
            ("delay 250", Step::Delay(250)),
            ("delay 1.5s", Step::Delay(1500)),
            ("pane Right", Step::Pane("right".into())),
            ("tab previous", Step::Tab("prev".into())),
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
        for name in [
            "",
            "ctrl+",
            "ctrl+ab",
            "ctrl+1",
            "f13",
            "upp",
            "3",
            "up 0",
            "up 51",
            "alt+",
            "shift+a",
            "ctrl+enter",
            "shift+x",
        ] {
            assert_eq!(key_bytes(name), None, "{name}");
        }
        assert_eq!(key_bytes("Up").unwrap(), b"\x1b[A");
        assert_eq!(key_bytes("down 3 enter").unwrap(), b"\x1b[B\x1b[B\x1b[B\r");
        assert_eq!(key_bytes("ctrl+b d").unwrap(), b"\x02d");
        assert_eq!(key_bytes("a 2 tab").unwrap(), b"aa\t");
        assert_eq!(key_bytes("f5").unwrap(), b"\x1b[15~");
        assert_eq!(key_bytes("alt+x").unwrap(), b"\x1bx");
        assert_eq!(key_bytes("alt+enter").unwrap(), b"\x1b\r");
        assert_eq!(key_bytes("alt+right").unwrap(), b"\x1b[1;3C");
        assert_eq!(key_bytes("ctrl+left").unwrap(), b"\x1b[1;5D");
        assert_eq!(key_bytes("shift+up").unwrap(), b"\x1b[1;2A");
        assert_eq!(key_bytes("ctrl+shift+f5").unwrap(), b"\x1b[15;6~");
        assert_eq!(key_bytes("alt+f1").unwrap(), b"\x1b[1;3P");
        assert_eq!(key_bytes("Shift+Tab").unwrap(), b"\x1b[Z");
        assert_eq!(key_bytes("ctrl+alt+d").unwrap(), b"\x1b\x04");
        assert_eq!(key_bytes("alt+ctrl+x").unwrap(), b"\x1b\x18");
        assert_eq!(key_bytes("f1").unwrap(), b"\x1bOP");
        assert_eq!(Step::parse("pause 500ms").unwrap(), Step::Delay(500));
        assert_eq!(Step::parse("pause 1.5s").unwrap(), Step::Delay(1500));
        assert!(Step::parse("pause 5ms").is_err());
        assert!(Step::parse("pause 0").is_err());
        assert_eq!(Step::parse("key f1").unwrap(), Step::Key("f1".into()));
        assert_eq!(Step::parse("key Enter").unwrap(), Step::Key("enter".into()));
        assert_eq!(Step::parse("type  y ").unwrap(), Step::Type("y".into()));
        assert!(Step::parse("key f13").is_err());
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

    #[test]
    fn script_steps_parse_and_print_back() {
        for (text, step) in [
            ("label again", Step::Label("again".into())),
            ("goto again", Step::Goto("again".into())),
            (
                "on (yes/no)? goto hostkey",
                Step::On("(yes/no)?".into(), Some("hostkey".into())),
            ),
            ("on #", Step::On("#".into(), None)),
            ("timeout 60", Step::Timeout(60, None)),
            ("timeout 5 goto slow", Step::Timeout(5, Some("slow".into()))),
            (
                "set model ${captured}",
                Step::Set("model".into(), "${captured}".into()),
            ),
            ("set empty \"\"", Step::Set("empty".into(), String::new())),
            ("add tries -1", Step::Add("tries".into(), -1)),
            ("show Done: ${n}", Step::Show("Done: ${n}".into())),
            ("stop", Step::Stop),
            ("fail no route", Step::Fail("no route".into())),
            (
                "if ${model} contains 9300 goto new",
                Step::If(Cond {
                    left: "${model}".into(),
                    op: Op::Contains,
                    right: "9300".into(),
                    label: "new".into(),
                }),
            ),
            (
                "if \"\" != ${x} goto set",
                Step::If(Cond {
                    left: String::new(),
                    op: Op::Ne,
                    right: "${x}".into(),
                    label: "set".into(),
                }),
            ),
            (
                "if ${n} <= 5 goto again",
                Step::If(Cond {
                    left: "${n}".into(),
                    op: Op::Le,
                    right: "5".into(),
                    label: "again".into(),
                }),
            ),
        ] {
            let got = Step::parse(text).unwrap();
            assert_eq!(got, step, "{text}");
            assert_eq!(Step::parse(&got.to_string()).unwrap(), step, "{text}");
            assert!(got.waits() && got.is_flow(), "{text}");
        }
        for bad in [
            "label 9x",
            "label",
            "goto",
            "goto a b",
            "on goto x",
            "timeout 0",
            "timeout 99999",
            "timeout 5 goto",
            "set host x",
            "set 1 x",
            "add n x",
            "if ${n} goto x",
            "if a == b",
            "stop now",
        ] {
            assert!(Step::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn conditions_compare_numbers_and_text() {
        assert!(Cond::holds(Op::Lt, "9", "10").unwrap());
        assert!(Cond::holds(Op::Eq, "07", "7").unwrap());
        assert!(Cond::holds(Op::Eq, "up", "up").unwrap());
        assert!(Cond::holds(Op::Ne, "up", "down").unwrap());
        assert!(Cond::holds(Op::Contains, "C9300-48P", "9300").unwrap());
        assert!(Cond::holds(Op::Gt, "x", "1").is_err());
    }

    #[test]
    fn flow_checks_labels_and_moves() {
        let check = |lines: &[&str]| check_lines(&owned(lines));
        assert!(check(&["@label a", "x", "@goto a"]).is_ok());
        assert!(
            check(&["@on # goto A", "@label a"]).is_ok(),
            "labels ignore case"
        );
        assert_eq!(check(&["x", "@goto b"]).unwrap_err().0, 1);
        assert!(
            check(&["@label a", "@label A"])
                .unwrap_err()
                .1
                .contains("twice")
        );
        assert!(check(&["@label a", "@goto a", "@pane left"]).is_err());
        assert!(check(&["@label a", "@pane left"]).is_ok());
        // A snippet with script steps needs a runner, and is no
        // auto-response.
        let steps = expand(
            &owned(&["@set a 1", "x"]),
            true,
            |_| String::new(),
            |_| String::new(),
        );
        assert!(steps_bytes(&steps).is_none());
    }

    #[test]
    fn arguments_fill_script_steps_too() {
        let steps = expand(
            &owned(&[
                "@if $1 == ${host} goto a",
                "@label a",
                "@show ${1:x} on ${host}",
            ]),
            true,
            |_| "sw1".into(),
            |_| "sw1".into(),
        );
        assert_eq!(steps.first().unwrap().to_string(), "if sw1 == sw1 goto a");
        assert_eq!(steps.get(2), Some(&Step::Show("sw1 on sw1".into())));
    }
}
