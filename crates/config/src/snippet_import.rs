//! Command snippets brought over from other programs:
//!
//! - Windows Terminal: `sendInput` actions in its `settings.json` (named
//!   ones, and the ones in nested command menus, whose names become
//!   categories), and the `snippets` list of a `.wt.json` file.
//! - SecureCRT: the Send String buttons of its button bars
//!   (`ButtonBarV4.ini` or `ButtonBarV5.ini`); each bar is a category.
//! - MobaXterm: the macros in `MobaXterm.ini`'s `[Macros]` section.
//!   MobaXterm does not document how it stores them, so this reads what
//!   is known of its encoding and the import page shows the lines first.
//! - A plain text file: one snippet, a line per line.
//!
//! Keystrokes become snippet lines: text ended by Enter is a line,
//! text without an Enter is an `@type` step, other control keys are
//! `@key` steps and SecureCRT's pause is `@pause 1`. Text is typed as it
//! is: a `$` is doubled and a leading `@` becomes `@@`, so nothing turns
//! into an argument, variable or step by accident.

use crate::json::{self, Json};
use crate::login::Snippet;
use crate::record::MAX_LINES;

/// A program snippets are imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnippetClient {
    WindowsTerminal,
    SecureCrt,
    MobaXterm,
    /// A text file, or a FeroSSH snippet file from elsewhere.
    TextFile,
}

impl SnippetClient {
    /// The name the user knows it by.
    pub fn name(self) -> &'static str {
        match self {
            SnippetClient::WindowsTerminal => "Windows Terminal",
            SnippetClient::SecureCrt => "SecureCRT",
            SnippetClient::MobaXterm => "MobaXterm",
            SnippetClient::TextFile => "a file",
        }
    }

    /// The category its snippets get when they bring none.
    pub fn category(self) -> &'static str {
        match self {
            SnippetClient::WindowsTerminal => "Windows Terminal",
            SnippetClient::SecureCrt => "SecureCRT",
            SnippetClient::MobaXterm => "MobaXterm",
            SnippetClient::TextFile => "Imported",
        }
    }
}

/// The longest name kept.
const MAX_NAME: usize = 80;
/// Snippets read from one file at most.
const MAX_SNIPPETS: usize = 2000;

/// Plain text as a snippet line that types it as it is.
fn literal_line(text: &str) -> String {
    let t = text.replace('$', "$$");
    if t.trim_start().starts_with('@') {
        t.replacen('@', "@@", 1)
    } else {
        t
    }
}

/// Builds snippet lines from keystrokes.
#[derive(Default)]
struct Keys {
    lines: Vec<String>,
    text: String,
    /// The last thing was a `\r`, so a `\n` after it is the same Enter.
    after_cr: bool,
}

impl Keys {
    fn push(&mut self, line: String) {
        if self.lines.len() < MAX_LINES {
            self.lines.push(line);
        }
    }

    /// Text typed without an Enter: an `@type` step, with spaces at its
    /// ends as keys since step text is trimmed.
    fn flush(&mut self) {
        let t = std::mem::take(&mut self.text);
        if t.is_empty() {
            return;
        }
        let lead = t.len() - t.trim_start().len();
        let trail = t.trim_start().len() - t.trim().len();
        for _ in 0..lead {
            self.push("@key space".into());
        }
        if !t.trim().is_empty() {
            // A step holds at most MAX_TEXT characters, `$$` included;
            // a `$$` is never cut in half.
            let mut text: String = t
                .trim()
                .replace('$', "$$")
                .chars()
                .take(crate::login::MAX_TEXT)
                .collect();
            if (text.len() - text.trim_end_matches('$').len()) % 2 == 1 {
                text.pop();
            }
            self.push(format!("@type {text}"));
        }
        for _ in 0..trail {
            self.push("@key space".into());
        }
    }

    fn enter(&mut self) {
        let t = std::mem::take(&mut self.text);
        if t.trim().is_empty() {
            self.push("@key enter".into());
        } else {
            self.push(literal_line(&t));
        }
    }

    fn key(&mut self, name: &str) {
        self.flush();
        self.push(format!("@key {name}"));
    }

    fn pause(&mut self) {
        self.flush();
        self.push("@pause 1".into());
    }

    fn char(&mut self, c: char) {
        let cr = self.after_cr;
        self.after_cr = false;
        match c {
            '\r' => {
                self.enter();
                self.after_cr = true;
            }
            '\n' if cr => {}
            '\n' => self.enter(),
            '\t' => self.key("tab"),
            '\u{1b}' => self.key("escape"),
            '\u{7f}' | '\u{8}' => self.key("backspace"),
            c if (c as u32) < 0x20 => {
                let letter = char::from(b'@' + c as u8).to_ascii_lowercase();
                self.key(&format!("ctrl+{letter}"));
            }
            c if c.is_control() => {}
            c => {
                if self.text.chars().count() < crate::login::MAX_TEXT {
                    self.text.push(c);
                }
            }
        }
    }

    fn done(mut self) -> Vec<String> {
        self.flush();
        self.lines
    }
}

/// Keystrokes as snippet lines.
pub fn keystrokes(input: &str) -> Vec<String> {
    let mut k = Keys::default();
    for c in input.chars() {
        k.char(c);
    }
    k.done()
}

fn short_name(text: &str) -> String {
    let one: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let one = one.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > MAX_NAME {
        let mut s: String = one.chars().take(MAX_NAME - 1).collect();
        s.push('…');
        s
    } else {
        one
    }
}

fn snippet(name: &str, category: &str, lines: Vec<String>) -> Option<Snippet> {
    let name = short_name(name);
    if name.is_empty() || lines.is_empty() {
        return None;
    }
    Some(Snippet {
        name,
        category: crate::snippet_store::clean_category(category),
        lines,
        ..Snippet::default()
    })
}

/// Windows Terminal's `sendInput` actions in a `settings.json`, or the
/// `snippets` of a `.wt.json` file.
pub fn windows_terminal(text: &str) -> Result<Vec<Snippet>, String> {
    let root = json::parse(text).map_err(|e| format!("It is not JSON that can be read: {e}."))?;
    let mut out = Vec::new();
    // A .wt.json file: {"snippets": [{"name", "input", "description"}]}.
    for s in root.get("snippets").map(Json::list).unwrap_or_default() {
        let Some(input) = s.get("input").and_then(Json::text) else {
            continue;
        };
        let name = s.get("name").and_then(Json::text).unwrap_or(input);
        if let Some(s) = snippet(
            name,
            SnippetClient::WindowsTerminal.category(),
            keystrokes(input),
        ) {
            out.push(s);
        }
    }
    for key in ["actions", "keybindings"] {
        for a in root.get(key).map(Json::list).unwrap_or_default() {
            wt_action(a, &[], &mut out, 0);
        }
    }
    out.truncate(MAX_SNIPPETS);
    Ok(out)
}

fn wt_action(a: &Json, menus: &[String], out: &mut Vec<Snippet>, depth: usize) {
    if depth > 8 || out.len() >= MAX_SNIPPETS {
        return;
    }
    let name = a.get("name").and_then(|n| match n {
        Json::Text(t) => Some(t.clone()),
        // A name may be a {"key": resource} object; there is no text then.
        _ => None,
    });
    if let Some(nested) = a.get("commands") {
        let mut inner = menus.to_vec();
        if let Some(n) = &name {
            inner.push(n.clone());
        }
        for c in nested.list() {
            wt_action(c, &inner, out, depth + 1);
        }
        return;
    }
    let Some(command) = a.get("command") else {
        return;
    };
    let is_send = command
        .get("action")
        .and_then(Json::text)
        .is_some_and(|t| t.eq_ignore_ascii_case("sendInput"));
    let Some(input) = command
        .get("input")
        .and_then(Json::text)
        .filter(|_| is_send)
    else {
        return;
    };
    let category = if menus.is_empty() {
        SnippetClient::WindowsTerminal.category().to_owned()
    } else {
        menus.join("/")
    };
    let name = name.unwrap_or_else(|| input.to_owned());
    if let Some(s) = snippet(&name, &category, keystrokes(input)) {
        out.push(s);
    }
}

/// SecureCRT's escapes in a Send String: `\r` Enter, `\n` line feed,
/// `\t` Tab, `\b` Backspace, `\e` Escape, `\p` a one-second pause and
/// `\\` a backslash.
fn securecrt_keys(text: &str) -> Vec<String> {
    // Some files hold the escapes with doubled backslashes (`\\r`): when
    // every backslash is doubled, undo that first.
    let doubled = {
        let singles = text.replace("\\\\", "");
        !singles.contains('\\') && text.contains("\\\\")
    };
    let text = if doubled {
        text.replace("\\\\", "\\")
    } else {
        text.to_owned()
    };
    let mut k = Keys::default();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            k.char(c);
            continue;
        }
        match chars.next() {
            Some('r') => k.char('\r'),
            Some('n') => k.char('\n'),
            Some('t') => k.char('\t'),
            Some('b') => k.char('\u{7f}'),
            Some('e') => k.char('\u{1b}'),
            Some('p') => {
                k.after_cr = false;
                k.pause();
            }
            Some(other) => k.char(other),
            None => k.char('\\'),
        }
    }
    k.done()
}

/// The Send String buttons of SecureCRT's button bars
/// (`ButtonBarV5.ini`): a `Z:"bar"=count` line, then a line per button,
/// ` SEND,text,label,,,n,n,`.
pub fn securecrt_buttons(text: &str) -> Vec<Snippet> {
    let mut out = Vec::new();
    let mut bar = String::new();
    for line in text.lines() {
        if out.len() >= MAX_SNIPPETS {
            break;
        }
        if let Some(rest) = line.strip_prefix("Z:\"") {
            bar = rest.split('"').next().unwrap_or_default().to_owned();
            continue;
        }
        let fields: Vec<&str> = line.trim_start().split(',').collect();
        let Some((&kind, rest)) = fields.split_first() else {
            continue;
        };
        if !kind.eq_ignore_ascii_case("SEND") || rest.len() < 6 {
            continue;
        }
        // The text may hold commas; the five fields after it never do.
        let label_at = rest.len() - 6;
        let send = rest.get(..label_at).unwrap_or_default().join(",");
        let label = rest.get(label_at).copied().unwrap_or_default();
        let category = if bar.is_empty() || bar.eq_ignore_ascii_case("Default") {
            SnippetClient::SecureCrt.category().to_owned()
        } else {
            bar.clone()
        };
        let name = if label.trim().is_empty() {
            send.as_str()
        } else {
            label
        };
        if let Some(s) = snippet(name, &category, securecrt_keys(&send)) {
            out.push(s);
        }
    }
    out
}

/// MobaXterm's encodings of characters that would break its INI lines.
const MOBA_ESCAPES: [(&str, &str); 6] = [
    ("__PTVIRG__", ";"),
    ("__DBLQUO__", "\""),
    ("__DIEZE__", "#"),
    ("__PERCENT__", "%"),
    ("__EGAL__", "="),
    ("__PIPE__", "|"),
];

/// The macros in `MobaXterm.ini`'s `[Macros]` section, as `name=keys`.
/// Lines of a macro are split at `__PIPE__`, with a `n:n:n:` prefix
/// before each one's text; each is typed with Enter.
pub fn mobaxterm_macros(text: &str) -> Vec<Snippet> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('[') {
            inside = line.trim().eq_ignore_ascii_case("[Macros]");
            continue;
        }
        if !inside || out.len() >= MAX_SNIPPETS {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let mut lines = Vec::new();
        for part in value.split("__PIPE__") {
            let mut part = part;
            // A `12:2:0:` style prefix.
            let mut fields = part.splitn(4, ':');
            if let (Some(a), Some(b), Some(c), Some(rest)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
                && [a, b, c]
                    .iter()
                    .all(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_digit()))
            {
                part = rest;
            }
            let mut t = part.to_owned();
            for (code, c) in MOBA_ESCAPES {
                t = t.replace(code, c);
            }
            if t.trim().is_empty() {
                continue;
            }
            // Each part is a line of its own: typed with Enter.
            if !t.ends_with(['\r', '\n']) {
                t.push('\r');
            }
            for l in keystrokes(&t) {
                if lines.len() < MAX_LINES {
                    lines.push(l);
                }
            }
        }
        if let Some(s) = snippet(name.trim(), SnippetClient::MobaXterm.category(), lines) {
            out.push(s);
        }
    }
    out
}

/// A text file as one snippet named `name`: a line per non-empty line,
/// typed as it is (a `#!` first line is left out).
pub fn text_file(name: &str, text: &str) -> Option<Snippet> {
    let mut lines = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let l = l.trim_end_matches('\r');
        if (i == 0 && l.starts_with("#!")) || l.trim().is_empty() {
            continue;
        }
        let l: String = l
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        if lines.len() < MAX_LINES {
            lines.push(literal_line(&l.replace('\t', "    ")));
        }
    }
    snippet(name, SnippetClient::TextFile.category(), lines)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::login::line_step;

    #[test]
    fn keystrokes_become_lines_and_steps() {
        assert_eq!(keystrokes("ls -l\r"), ["ls -l"]);
        assert_eq!(keystrokes("a\r\nb\nc"), ["a", "b", "@type c"]);
        assert_eq!(
            keystrokes("echo $HOME\r@x\r\r\u{3}\t ok "),
            [
                "echo $$HOME",
                "@@x",
                "@key enter",
                "@key ctrl+c",
                "@key tab",
                "@key space",
                "@type ok",
                "@key space"
            ]
        );
        let long = keystrokes(&"$".repeat(400));
        assert_eq!(long, [format!("@type {}", "$".repeat(256))]);
        assert!(!matches!(line_step(&long[0]), Some(Err(_))));
        for l in keystrokes("\u{1b}\u{7f}\u{1a}x$1\r") {
            assert!(!matches!(line_step(&l), Some(Err(_))), "{l}");
        }
    }

    #[test]
    fn windows_terminal_actions_and_snippets() {
        let text = r#"{
            // comment
            "actions": [
                { "command": { "action": "sendInput", "input": "git status\r" }, "name": "Git status", "keys": "ctrl+g" },
                { "command": { "action": "copy" }, "keys": "ctrl+c" },
                { "command": { "action": "sendInput", "input": "cd ~\r" }, "id": "User.x" },
                { "name": "Docker", "commands": [
                    { "name": "ps", "command": { "action": "sendInput", "input": "docker ps\r" } },
                    { "name": { "key": "Res" }, "command": { "action": "sendInput", "input": "docker images\r" } },
                ] },
            ],
            "snippets": [ { "name": "Up", "input": "uptime\n" } ],
        }"#;
        let s = windows_terminal(text).unwrap();
        let names: Vec<_> = s
            .iter()
            .map(|s| (s.name.as_str(), s.category.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("Up", "Windows Terminal"),
                ("Git status", "Windows Terminal"),
                ("cd ~", "Windows Terminal"),
                ("ps", "Docker"),
                ("docker images", "Docker"),
            ]
        );
        assert_eq!(s[1].lines, ["git status"]);
        assert!(windows_terminal("{").is_err());
        assert!(windows_terminal("{}").unwrap().is_empty());
    }

    #[test]
    fn securecrt_button_bars() {
        let text = "Z:\"Keyword HL Video BBar\"=00000003\n \
            SEND,sh ip int br\\r,ip br,,,0,4,\n \
            SEND,ena\\r\\pSECRET\\r,enable,,,0,5,\n \
            MENU_TOGGLE_KEYWORD_HIGHLIGHTING,,Highlight on/off,,,0,0,\n\
            Z:\"Default\"=00000001\n \
            SEND,echo a\\,b\\r,,,,0,1,\n";
        let s = securecrt_buttons(text);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].name, "ip br");
        assert_eq!(s[0].category, "Keyword HL Video BBar");
        assert_eq!(s[0].lines, ["sh ip int br"]);
        assert_eq!(s[1].lines, ["ena", "@pause 1", "SECRET"]);
        assert_eq!(s[2].category, "SecureCRT");
        assert_eq!(s[2].lines, ["echo a,b"]);
        // The doubled form some pages show reads the same.
        let doubled = securecrt_buttons("Z:\"B\"=1\n SEND,sh run\\\\r,run,,,0,1,\n");
        assert_eq!(doubled[0].lines, ["sh run"]);
    }

    #[test]
    fn mobaxterm_macros_section() {
        let text = "[Bookmarks]\nx=y\n[Macros]\nDisk=12:2:0:df -h__PTVIRG__ echo ok__PIPE__12:2:0:uptime\nEmpty=\n[Other]\nz=1\n";
        let s = mobaxterm_macros(text);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Disk");
        assert_eq!(s[0].lines, ["df -h; echo ok", "uptime"]);
    }

    #[test]
    fn text_files() {
        let s = text_file("backup", "#!/bin/sh\ntar czf /tmp/x.tgz $HOME\n\n@me\n").unwrap();
        assert_eq!(s.lines, ["tar czf /tmp/x.tgz $$HOME", "@@me"]);
        assert_eq!(s.category, "Imported");
        assert!(text_file("x", "\n\n").is_none());
    }

    #[test]
    fn every_import_reads_back_as_valid_lines() {
        let long = "x$".repeat(300);
        let inputs = [
            format!(
                "{{\"snippets\":[{{\"name\":\"a\\\"b\\nSnippet c\",\"input\":\"{long}\\r @x\\t\\u0001$9\"}}]}}"
            ),
            format!("Z:\"x/../y\"=1\n SEND,{long}\\p\\e\\\\,@lbl,,,0,1,\n"),
            format!("[Macros]\n..=1:2:3:{long}__PIPE__@@__DIEZE__ $1\n"),
            format!("#!/bin/sh\n\t@key\u{7}\n{long}\n"),
        ];
        for text in &inputs {
            let mut all = windows_terminal(text).unwrap_or_default();
            all.extend(securecrt_buttons(text));
            all.extend(mobaxterm_macros(text));
            all.extend(text_file("..", text));
            assert!(!all.is_empty());
            for s in &all {
                for l in &s.lines {
                    assert!(!matches!(line_step(l), Some(Err(_))), "{l:?}");
                }
                let file = crate::snippet_store::file_for(s);
                assert!(
                    !file.split('/').any(|p| p.is_empty() || p == ".."),
                    "{file}"
                );
                let text = crate::snippet_store::file_text(s);
                let back = crate::snippet_store::parse_file(&text, &s.category).unwrap();
                let trimmed: Vec<&str> = s.lines.iter().map(|l| l.trim()).collect();
                assert_eq!(back.lines, trimmed);
            }
        }
    }
}
