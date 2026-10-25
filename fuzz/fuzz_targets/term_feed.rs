//! Feeds arbitrary host output to the terminal in arbitrary chunks, with
//! resizes, scrolling, selection and search in between. It must never
//! panic, titles must come out sanitised, and replies to the host must be
//! plain ASCII escape sequences, never echoes of host-chosen text.

#![no_main]

use ferossh_render::Grid;
use ferossh_term::{Action, Settings, Terminal};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let mut t = Terminal::new(20, 6, &Settings::new(50));
    let mut grid = Grid::new(1, 1);
    for (i, chunk) in stream.chunks(usize::from(split % 64).max(1)).enumerate() {
        t.feed(chunk);
        for action in t.take_actions() {
            match action {
                Action::Reply(bytes) => assert!(
                    bytes
                        .iter()
                        .all(|b| (0x20..0x7f).contains(b) || *b == 0x1b || *b == 0x07),
                    "reply {bytes:?}"
                ),
                Action::Title(Some(title)) => {
                    assert!(!title.chars().any(char::is_control));
                    assert!(title.chars().count() <= 256);
                }
                Action::Title(None) | Action::ClipboardRequest(_) | Action::Bell => {}
                Action::WorkingDirectory(dir) => {
                    assert!(!dir.path.contains(&0));
                    assert!(dir.path.len() <= 4096);
                }
            }
        }
        // Vary the size and view now and then, driven by the input.
        match (split as usize + i) % 11 {
            0 => t.resize(usize::from(chunk[0] % 30), usize::from(split % 9)),
            1 => t.scroll_lines(i32::from(chunk[0] as i8)),
            2 => {
                t.select_start(usize::from(chunk[0]), 1, true, ferossh_term::SelectionKind::Word);
                let _ = t.selection_text();
            }
            3 => {
                t.search_open();
                t.search_set(&String::from_utf8_lossy(&chunk[..chunk.len().min(4)]));
            }
            4 => t.flush_sync(),
            _ => {}
        }
        let _ = t.draw_into(&mut grid, i % 2 == 0);
    }
});
