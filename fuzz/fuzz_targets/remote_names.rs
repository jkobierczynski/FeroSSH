//! Feeds arbitrary names and paths, as a hostile server would put them in
//! a listing, a ZMODEM header or its shell's prompt and OSC 7 reports, to
//! what turns them into Windows file names, text on screen and the
//! directory the file pane follows. Nothing may panic, and:
//!
//! - a local name is an ordinary file name in the folder it is saved to:
//!   no path separator or other character Windows forbids, no device
//!   name, no trailing dot or space (which Windows drops, so `a.txt.`
//!   would land on `a.txt`), no bidi control, at most 255 UTF-16 units;
//! - a shown name has no control character or bidi override;
//! - `inside` never lets a path out of its root;
//! - the directory a shell reports is the same however its output is
//!   split, and never holds a NUL.

#![no_main]

use ferossh_sftp::names::{display, file_name, inside, join, local_name, parent};
use ferossh_term::{Action, Settings, Terminal, prompt_dir};
use libfuzzer_sys::fuzz_target;

const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

fn bidi(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
}

/// Whether Windows would read `base` (a name before its first dot) as a
/// device.
fn device(base: &str) -> bool {
    let upper = base.trim_end_matches(' ').to_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    let mut chars = upper.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (prefix == "COM" || prefix == "LPT")
        && matches!(rest.as_slice(), [d] if d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
}

fn check_local(remote: &[u8]) {
    let Some(name) = local_name(remote) else {
        return;
    };
    assert!(!name.is_empty(), "{remote:?}");
    assert!(name != "." && name != "..", "{remote:?}");
    assert!(
        !name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            || c.is_control()
            || bidi(c)),
        "{remote:?} -> {name:?}"
    );
    assert!(!name.ends_with(['.', ' ']), "{remote:?} -> {name:?}");
    assert!(name.encode_utf16().count() <= 255, "{remote:?}");
    assert!(!device(name.split('.').next().unwrap_or_default()), "{remote:?} -> {name:?}");
}

fn split(data: &[u8]) -> Vec<&[u8]> {
    data.split(|&b| b == 0xff).collect()
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let parts = split(rest);
    for p in &parts {
        check_local(p);
        // A ZMODEM sender may send a path: the last part names the file.
        check_local(file_name(p));
        let shown = display(p);
        assert!(!shown.chars().any(|c| c.is_control() || bidi(c)));
    }
    if let [root, path, ..] = parts.as_slice() {
        // Canonical paths, as `realpath` gives them: absolute, no `.`, `..`
        // or doubled slashes, no trailing slash but `/` itself.
        let canonical = |p: &[u8]| {
            p.starts_with(b"/")
                && (p == b"/" || !p.ends_with(b"/"))
                && p.split(|&b| b == b'/').skip(1).all(|s| !s.is_empty() && s != b"." && s != b"..")
        };
        if canonical(root) && canonical(path) && inside(root, path) {
            // Climbing out of `path` reaches `root` before leaving it.
            let mut p = path.to_vec();
            while p != *root {
                assert!(p.len() > root.len(), "{root:?} {path:?}");
                p = parent(&p);
            }
        }
        let joined = join(root, path);
        assert!(joined.starts_with(root));
        if !path.is_empty() {
            assert_eq!(file_name(&joined), file_name(&[b"/", *path].concat()));
        }
    }

    // The same bytes as the shell's output, whole and in pieces.
    let text = String::from_utf8_lossy(rest);
    for line in text.lines() {
        if let Some(d) = prompt_dir(line) {
            assert!(!d.host.chars().any(char::is_control));
            assert!(!d.path.iter().any(|&b| b < 0x20 || b == 0x7f));
        }
    }
    let step = usize::from(mode % 32).max(1);
    let dirs = |chunks: &mut dyn Iterator<Item = &[u8]>| {
        let mut t = Terminal::new(20, 4, &Settings::new(0));
        let mut out = Vec::new();
        for c in chunks {
            t.feed(c);
            for a in t.take_actions() {
                if let Action::WorkingDirectory(d) = a {
                    assert!(!d.path.contains(&0));
                    assert!(!d.host.chars().any(char::is_control));
                    out.push(d);
                }
            }
        }
        out
    };
    let whole = dirs(&mut std::iter::once(rest));
    let pieces = dirs(&mut rest.chunks(step));
    assert_eq!(whole, pieces);
});
