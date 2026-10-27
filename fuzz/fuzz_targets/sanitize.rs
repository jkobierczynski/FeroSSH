//! Feeds arbitrary text to the checks that stand between the host (or the
//! clipboard, or someone who edited a log) and the user: window titles,
//! OSC 8 link targets, clipboard previews, pastes, and the log chain and
//! index readers. None may panic, and each must keep its promise.

#![no_main]

use ferossh_term::{chain, check_link, clipboard_preview, prepare_paste, sanitize_title};
use libfuzzer_sys::fuzz_target;

// The same set as the sanitiser's own, kept here so a change to one shows.
fn invisible(c: char) -> bool {
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

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let text = String::from_utf8_lossy(rest);
    match mode % 5 {
        0 => {
            let title = sanitize_title(&text);
            assert!(!title.chars().any(|c| c.is_control() || invisible(c)));
            assert!(title.chars().count() <= 256); // MAX_TITLE
        }
        1 => {
            if let Some(link) = check_link(&text) {
                let lower = link.to_ascii_lowercase();
                assert!(
                    lower.starts_with("http://")
                        || lower.starts_with("https://")
                        || lower.starts_with("mailto:"),
                    "{link:?}"
                );
                assert!(link.len() <= 2048); // MAX_LINK
                assert!(!link.chars().any(|c| c.is_control() || c.is_whitespace() || invisible(c)));
            }
        }
        2 => {
            let max = usize::from(mode >> 3);
            let shown = clipboard_preview(&text, max);
            assert!(!shown.chars().any(|c| c.is_control() || invisible(c)));
            assert!(shown.chars().count() <= max + 1);
        }
        3 => {
            let bracketed = mode & 0x80 != 0;
            let paste = prepare_paste(&text, bracketed);
            let body = if bracketed {
                let inner = paste
                    .bytes
                    .strip_prefix(b"\x1b[200~")
                    .and_then(|b| b.strip_suffix(b"\x1b[201~"));
                assert!(inner.is_some(), "markers missing");
                inner.unwrap_or_default()
            } else {
                &paste.bytes[..]
            };
            // Nothing in the pasted text can end the paste early or start
            // an escape sequence: only tab and CR are left of the controls.
            assert!(
                body.iter().all(|&b| b == b'\t' || b == b'\r' || b >= 0x20 && b != 0x7f),
                "{body:?}"
            );
            assert!(!String::from_utf8_lossy(body).chars().any(|c| c.is_control() && c != '\t' && c != '\r'));
            assert_eq!(paste.line_breaks, body.iter().filter(|&&b| b == b'\r').count());
            assert_eq!(paste.needs_confirmation(), paste.line_breaks > 0 && !bracketed);
        }
        _ => {
            // The log's bytes, its chain file and the index, split at NULs.
            let mut parts = rest.splitn(3, |&b| b == 0);
            let log = parts.next().unwrap_or_default();
            let chain_file = String::from_utf8_lossy(parts.next().unwrap_or_default());
            let index = String::from_utf8_lossy(parts.next().unwrap_or_default());
            let _ = chain::verify("host_tab1.log", log, &chain_file);
            let _ = chain::read_index(&index);
        }
    }
});
