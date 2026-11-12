//! Drives command suggestions with arbitrary host output and typing: the
//! host draws whatever it likes (its prompt, its echo, a redrawn line,
//! questions for a password), the user types, presses Enter or a control
//! key, and the pane is resized, in any order. The tracker and history
//! are the app's own (`crates/app/src/suggest.rs`, compiled in here: the
//! app is a binary, not a library). Nothing may panic, and:
//!
//! - whatever is kept is one line of text without control characters,
//!   not too long, and never a command carrying a secret;
//! - suggestions only ever finish what is typed, and are few;
//! - a history file, written and read back, is the same history, and an
//!   arbitrary one reads as a history within its limits.

#![no_main]

#[allow(dead_code)]
#[path = "../../crates/app/src/suggest.rs"]
mod suggest;

use ferossh_term::{Settings, Terminal};
use libfuzzer_sys::fuzz_target;
use suggest::{History, MAX_CHOICES, MAX_ENTRIES, MAX_LEN, Tracker, carries_secret, choices};

const KEY: &str = "ssh:jo@h:22";

fn check_kept(line: &str) {
    assert!(!line.trim().is_empty() && line.chars().count() >= 2, "{line:?}");
    assert!(line.len() <= MAX_LEN, "{line:?}");
    assert!(!line.chars().any(char::is_control), "{line:?}");
    assert!(!line.starts_with(' '), "{line:?}");
    assert!(!carries_secret(line), "{line:?}");
}

fn check_choices(history: &History, typed: &str) {
    let got = choices(Some(history), typed);
    assert!(got.len() <= MAX_CHOICES);
    for c in &got {
        assert!(c.len() > typed.len() && c.starts_with(typed), "{typed:?} {c:?}");
        let word = suggest::next_word(&c[typed.len()..]);
        assert!(c[typed.len()..].starts_with(word));
    }
}

fuzz_target!(|data: &[u8]| {
    // An arbitrary history file first: the part before the first 0xff.
    let (file, ops) = match data.iter().position(|&b| b == 0xff) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &[][..]),
    };
    let text = String::from_utf8_lossy(file);
    let mut history = History::parse(&format!("FeroSSH command history 1\n{KEY}\n{text}"), KEY)
        .expect("a history of this host");
    let _ = History::parse(&text, KEY);
    assert!(history.matching("").count() <= MAX_ENTRIES);
    for kept in history.matching("") {
        check_kept_file(kept);
    }

    let mut t = Terminal::new(30, 5, &Settings::new(20));
    let mut k = Tracker::default();
    let mut i = 0;
    while let Some(&op) = ops.get(i) {
        i += 1;
        let len = usize::from(ops.get(i).copied().unwrap_or(0) % 24);
        let arg = ops.get(i + 1..(i + 1 + len).min(ops.len())).unwrap_or_default();
        let kept = match op % 8 {
            // The host's output.
            0 | 1 => {
                i += 1 + arg.len();
                t.feed(arg);
                k.poll(&t)
            }
            // The user types, and the host echoes it.
            2 | 3 => {
                i += 1 + arg.len();
                let kept = k.typed(&t, arg);
                if op & 8 == 0 {
                    t.feed(arg);
                }
                kept
            }
            // Enter.
            4 => k.typed(&t, b"\r"),
            // Ctrl+C, Backspace.
            5 => k.typed(&t, if op & 8 == 0 { b"\x03" } else { b"\x7f" }),
            6 => {
                i += 1;
                t.resize(usize::from(len % 40) + 1, usize::from(op >> 4) + 1);
                k.poll(&t)
            }
            _ => {
                if let Some(line) = k.line(&t) {
                    check_choices(&history, &line.before);
                }
                None
            }
        };
        if let Some(line) = kept {
            check_kept(&line);
            history.record(&line);
        }
    }

    let back = History::parse(&history.to_text(), KEY).expect("reads back");
    assert_eq!(back, history);
    let typed = String::from_utf8_lossy(ops.get(..ops.len().min(8)).unwrap_or_default());
    check_choices(&history, &typed);
});

/// What a history file may hold: [`check_kept`] without the checks for
/// secrets, which are made when a line is recorded.
fn check_kept_file(line: &str) {
    assert!(!line.trim().is_empty());
    assert!(line.len() <= MAX_LEN);
    assert!(!line.chars().any(char::is_control));
}
