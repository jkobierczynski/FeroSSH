//! Remote names and paths: making a server's file names safe to create on
//! Windows, showing them, and joining remote paths.
//!
//! A server chooses the names in its listings, so a hostile one may send
//! `..`, a name with a slash, `CON`, `aux.txt`, `x:stream` or `name.`, all
//! of which mean something else to Windows than a plain file. [`local_name`]
//! turns each into an ordinary name, or refuses it.

/// Windows' reserved device names, which name devices with any extension.
const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

/// Longest name Windows allows, in UTF-16 units.
const MAX_NAME_UNITS: usize = 255;

fn reserved(base: &str) -> bool {
    let upper = base.trim_end_matches(' ').to_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    // COM0-9 and LPT0-9, and the superscript digits Windows also accepts.
    let mut chars = upper.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (prefix == "COM" || prefix == "LPT")
        && matches!(rest.as_slice(), [d] if d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
}

/// The name a remote file gets on Windows, or `None` if it must not be
/// created at all (`.`, `..`, empty, or holding a path separator, which a
/// listing never should).
///
/// Characters Windows forbids (`<>:"/\|?*` and control characters) become
/// `_`, so do trailing dots and spaces, and a reserved device name gets a
/// `_` in front. Bytes that are not UTF-8 become `_` too. Long names are
/// cut to Windows' limit, keeping the extension.
pub fn local_name(remote: &[u8]) -> Option<String> {
    if remote.is_empty() || remote == b"." || remote == b".." || remote.contains(&b'/') {
        return None;
    }
    if remote.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(remote);
    let mut name: String = text
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\u{fffd}' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let kept = name.trim_end_matches(['.', ' ']).len();
    let trailing = name.len() - kept;
    name.truncate(kept);
    name.extend(std::iter::repeat_n('_', trailing));
    let base = name.split('.').next().unwrap_or_default();
    if reserved(base) {
        name.insert(0, '_');
    }
    if name.encode_utf16().count() > MAX_NAME_UNITS {
        let ext: String = match name.rfind('.') {
            Some(i) if name.len() - i <= 16 => name.get(i..).unwrap_or_default().to_owned(),
            _ => String::new(),
        };
        let mut stem: String = name
            .get(..name.len() - ext.len())
            .unwrap_or_default()
            .to_owned();
        while stem.encode_utf16().count() + ext.encode_utf16().count() > MAX_NAME_UNITS {
            stem.pop();
        }
        name = stem + &ext;
    }
    Some(name)
}

/// A remote name or path made safe to draw: not UTF-8 becomes U+FFFD, and
/// control characters and bidirectional overrides become `?`, so a name
/// cannot move the terminal's cursor or disguise its extension.
pub fn display(remote: &[u8]) -> String {
    String::from_utf8_lossy(remote)
        .chars()
        .map(|c| match c {
            c if c.is_control() => '?',
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' => '?',
            c => c,
        })
        .collect()
}

/// `dir/name`.
pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = dir.to_vec();
    if !p.ends_with(b"/") {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// The directory holding `path`; `/` for `/` itself.
pub fn parent(path: &[u8]) -> Vec<u8> {
    let trimmed = match path {
        [rest @ .., b'/'] if !rest.is_empty() => rest,
        p => p,
    };
    match trimmed.iter().rposition(|&b| b == b'/') {
        Some(0) => b"/".to_vec(),
        Some(i) => trimmed.get(..i).unwrap_or(b"/").to_vec(),
        None => b".".to_vec(),
    }
}

/// The last part of `path`.
pub fn file_name(path: &[u8]) -> &[u8] {
    let trimmed = match path {
        [rest @ .., b'/'] if !rest.is_empty() => rest,
        p => p,
    };
    match trimmed.iter().rposition(|&b| b == b'/') {
        Some(i) => trimmed.get(i + 1..).unwrap_or_default(),
        None => trimmed,
    }
}

/// Whether canonical path `path` is `root` or inside it. Both come from
/// the server's `realpath`, so `..` and links are already resolved.
pub fn inside(root: &[u8], path: &[u8]) -> bool {
    if path == root {
        return true;
    }
    if root == b"/" {
        return path.starts_with(b"/");
    }
    path.len() > root.len() && path.starts_with(root) && path.get(root.len()) == Some(&b'/')
}

/// `name` with ` (n)` before its extension, for keeping both files.
pub fn numbered(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!(
            "{} ({n}){}",
            name.get(..i).unwrap_or(name),
            name.get(i..).unwrap_or_default()
        ),
        _ => format!("{name} ({n})"),
    }
}

/// Bytes as `1.2 MB`, for the transfer list.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit + 1 < UNITS.len() {
        v /= 1000.0;
        unit += 1;
    }
    let u = UNITS.get(unit).copied().unwrap_or("B");
    if unit == 0 {
        format!("{bytes} B")
    } else if v < 10.0 {
        format!("{v:.1} {u}")
    } else {
        format!("{v:.0} {u}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_windows_names() {
        let cases: [(&[u8], Option<&str>); 16] = [
            (b"notes.txt", Some("notes.txt")),
            (b"..", None),
            (b".", None),
            (b"", None),
            (b"a/b", None),
            (b"a\0b", None),
            (b"a:b", Some("a_b")),
            (b"what?*.txt", Some("what__.txt")),
            (b"back\\slash", Some("back_slash")),
            (b"dots...", Some("dots___")),
            (b"space ", Some("space_")),
            (b"CON", Some("_CON")),
            (b"aux.tar.gz", Some("_aux.tar.gz")),
            (b"com1", Some("_com1")),
            (b"LPT\xc2\xb9", Some("_LPT\u{b9}")),
            (b"bad\xffutf8\x1b[2J", Some("bad_utf8_[2J")),
        ];
        for (remote, want) in cases {
            assert_eq!(local_name(remote).as_deref(), want, "{remote:?}");
        }
        // Not reserved: only the exact device names are.
        assert_eq!(local_name(b"console").as_deref(), Some("console"));
        assert_eq!(local_name(b"com10").as_deref(), Some("com10"));
        assert_eq!(local_name(b".bashrc").as_deref(), Some(".bashrc"));
    }

    #[test]
    fn cuts_long_names_keeping_the_extension() {
        let long = format!("{}.tar.gz", "x".repeat(300));
        let name = local_name(long.as_bytes()).unwrap_or_default();
        assert_eq!(name.encode_utf16().count(), 255);
        assert!(name.ends_with("x.gz"));
    }

    #[test]
    fn shows_names_safely() {
        assert_eq!(display(b"evil\x1b]0;x\x07"), "evil?]0;x?");
        assert_eq!(display("exe\u{202e}txt.exe".as_bytes()), "exe?txt.exe");
        assert_eq!(display(b"\xff"), "\u{fffd}");
    }

    #[test]
    fn joins_and_splits_paths() {
        assert_eq!(join(b"/home/j", b"a"), b"/home/j/a");
        assert_eq!(join(b"/", b"a"), b"/a");
        assert_eq!(parent(b"/home/j/a"), b"/home/j");
        assert_eq!(parent(b"/home/"), b"/");
        assert_eq!(parent(b"/a"), b"/");
        assert_eq!(parent(b"/"), b"/");
        assert_eq!(file_name(b"/home/j/a.txt"), b"a.txt");
        assert_eq!(file_name(b"/home/j/"), b"j");
    }

    #[test]
    fn containment_needs_a_separator() {
        assert!(inside(b"/data", b"/data"));
        assert!(inside(b"/data", b"/data/x"));
        assert!(!inside(b"/data", b"/database"));
        assert!(!inside(b"/data", b"/etc/passwd"));
        assert!(inside(b"/", b"/etc"));
    }

    #[test]
    fn numbers_and_sizes() {
        assert_eq!(numbered("a.txt", 2), "a (2).txt");
        assert_eq!(numbered(".bashrc", 2), ".bashrc (2)");
        assert_eq!(numbered("Makefile", 3), "Makefile (3)");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_234_567), "1.2 MB");
        assert_eq!(size(45_000_000_000), "45 GB");
    }
}
