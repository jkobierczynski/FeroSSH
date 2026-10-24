//! Cleaning what the host sends before the UI shows or acts on it, and what
//! the user pastes before it reaches the host.

/// Longest window title kept, in characters.
pub const MAX_TITLE: usize = 256;
/// Longest link target accepted, in bytes.
pub const MAX_LINK: usize = 2048;

/// Characters that change how text around them is displayed without being
/// visible themselves: bidi overrides and isolates, zero-width characters
/// and the BOM. A host can use them to make a title or link read as
/// something it is not.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// A window title from the host, safe to show: control and invisible
/// formatting characters removed, at most [`MAX_TITLE`] characters.
pub fn sanitize_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_control() && !is_invisible_format(*c))
        .take(MAX_TITLE)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// An OSC 8 link target that may be opened: `http`, `https` or `mailto`,
/// no longer than [`MAX_LINK`], with no whitespace, control or invisible
/// formatting characters. Anything else returns `None`; the UI shows the
/// returned target verbatim before opening it.
pub fn check_link(uri: &str) -> Option<&str> {
    if uri.len() > MAX_LINK
        || uri
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || is_invisible_format(c))
    {
        return None;
    }
    let (scheme, rest) = uri.split_once(':')?;
    let ok = match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest.len() > 2 && rest.starts_with("//"),
        "mailto" => !rest.is_empty(),
        _ => false,
    };
    ok.then_some(uri)
}

/// Text from the host shown in a confirmation prompt: control characters
/// made visible, invisible formatting removed, cut at `max_chars`.
pub fn clipboard_preview(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, c) in text
        .chars()
        .filter(|c| !is_invisible_format(*c))
        .enumerate()
    {
        if i == max_chars {
            out.push('…');
            break;
        }
        match c {
            '\n' => out.push('↵'),
            '\t' => out.push('→'),
            c if c.is_control() => out.push('�'),
            c => out.push(c),
        }
    }
    out
}

/// Text ready to send to the host as a paste.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paste {
    pub bytes: Vec<u8>,
    /// Line breaks in the pasted text. Without bracketed paste each one
    /// acts as Enter, so a shell runs the lines as they arrive.
    pub line_breaks: usize,
    /// Wrapped in bracketed-paste markers, so the application sees it as
    /// one paste rather than typed keys.
    pub bracketed: bool,
}

impl Paste {
    /// Whether the user should confirm before it is sent: it would run
    /// commands at once (a line break, without bracketed paste).
    pub fn needs_confirmation(&self) -> bool {
        self.line_breaks > 0 && !self.bracketed
    }
}

/// Prepares a paste: line breaks become CR (what Enter sends), every other
/// control character except tab is removed (so the text cannot contain
/// escape sequences, including a fake end-of-paste marker), and the result
/// is bracketed when the host enabled bracketed paste.
pub fn prepare_paste(text: &str, bracketed: bool) -> Paste {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut line_breaks = 0;
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                clean.push('\r');
                line_breaks += 1;
            }
            '\n' => {
                clean.push('\r');
                line_breaks += 1;
            }
            '\t' => clean.push('\t'),
            c if c.is_control() => {}
            c => clean.push(c),
        }
    }
    let mut bytes = Vec::with_capacity(clean.len() + 12);
    if bracketed {
        bytes.extend_from_slice(b"\x1b[200~");
    }
    bytes.extend_from_slice(clean.as_bytes());
    if bracketed {
        bytes.extend_from_slice(b"\x1b[201~");
    }
    Paste {
        bytes,
        line_breaks,
        bracketed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_controls_and_bidi_tricks() {
        assert_eq!(
            sanitize_title("  vim\x07 main.rs \x1b[31m"),
            "vim main.rs [31m"
        );
        assert_eq!(sanitize_title("evil\u{202E}txt.exe"), "eviltxt.exe");
        assert_eq!(sanitize_title("a\u{9b}b\u{200B}c"), "abc");
        assert_eq!(sanitize_title(&"x".repeat(1000)).chars().count(), MAX_TITLE);
        assert_eq!(sanitize_title("日本語"), "日本語");
    }

    #[test]
    fn links_must_be_plain_web_or_mail() {
        assert_eq!(
            check_link("https://example.com/a?b=c"),
            Some("https://example.com/a?b=c")
        );
        assert!(check_link("HTTP://example.com").is_some());
        assert!(check_link("mailto:a@example.com").is_some());
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:",
            "\\\\server\\share\\x.exe",
            "https://exa mple.com",
            "https://example.com/\u{202E}gpj.exe",
            "https://example.com/\x07",
            "https:",
            "no scheme",
        ] {
            assert!(check_link(bad).is_none(), "{bad:?} accepted");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_LINK));
        assert!(check_link(&long).is_none());
    }

    #[test]
    fn preview_shows_controls() {
        assert_eq!(clipboard_preview("a\nb\tc\x1b", 10), "a↵b→c�");
        assert_eq!(clipboard_preview("abcdef", 3), "abc…");
        assert_eq!(clipboard_preview("a\u{202E}b", 10), "ab");
    }

    #[test]
    fn paste_strips_controls_and_normalises_newlines() {
        let p = prepare_paste("echo hi\r\nrm -rf /\x1b[201~\n\x03x\ty", false);
        assert_eq!(p.bytes, b"echo hi\rrm -rf /[201~\rx\ty");
        assert_eq!(p.line_breaks, 2);
        assert!(p.needs_confirmation());
        // C1 controls (8-bit CSI) go too.
        assert_eq!(prepare_paste("a\u{9b}31mb", false).bytes, b"a31mb");
    }

    #[test]
    fn single_line_paste_needs_no_confirmation() {
        let p = prepare_paste("ls -la", false);
        assert_eq!(p.bytes, b"ls -la");
        assert!(!p.needs_confirmation());
        // A trailing newline would run the command at once.
        assert!(prepare_paste("ls -la\n", false).needs_confirmation());
    }

    #[test]
    fn bracketed_paste_cannot_be_closed_early() {
        let p = prepare_paste("a\n\x1b[201~b", true);
        assert_eq!(p.bytes, b"\x1b[200~a\r[201~b\x1b[201~");
        assert!(!p.needs_confirmation());
    }
}
