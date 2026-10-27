//! Feeds arbitrary host output to the terminal in arbitrary chunks, with
//! resizes, scrolling, selection, search and the user's keys and mouse in
//! between. It must never panic, titles must come out sanitised, and
//! replies to the host must be plain ASCII escape sequences, never echoes
//! of host-chosen text.

#![no_main]

use ferossh_render::Grid;
use ferossh_term::{Action, Key, Mods, MouseButton, MouseEvent, SelectionKind, Settings, Terminal};
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
                t.select_start(usize::from(chunk[0]), 1, true, SelectionKind::Word);
                let _ = t.selection_text();
            }
            3 => {
                t.search_open();
                t.search_set(&String::from_utf8_lossy(&chunk[..chunk.len().min(4)]));
            }
            4 => t.flush_sync(),
            5 | 6 => user(&mut t, chunk),
            _ => {}
        }
        let _ = t.draw_into(&mut grid, i % 2 == 0);
    }
});

/// What the user does between two pieces of host output, picked and
/// placed by the chunk's bytes. Key and focus reports must stay plain
/// escape sequences, whatever modes the host set.
fn user(t: &mut Terminal, chunk: &[u8]) {
    let b = |n: usize| chunk.get(n).copied().unwrap_or(0);
    let (col, row) = (usize::from(b(1)), usize::from(b(2)));
    let mods = Mods {
        shift: b(3) & 1 != 0,
        alt: b(3) & 2 != 0,
        ctrl: b(3) & 4 != 0,
    };
    let button = match b(3) >> 3 & 7 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        3 => MouseButton::WheelUp,
        _ => MouseButton::WheelDown,
    };
    let reply = match b(0) % 16 {
        0 => {
            t.scroll_pages(i32::from(b(1) as i8 % 4));
            None
        }
        1 => {
            t.scroll_to_top();
            None
        }
        2 => {
            t.scroll_to_bottom();
            None
        }
        3 => {
            t.clear_scrollback();
            None
        }
        4 => {
            let kind = match b(3) % 4 {
                0 => SelectionKind::Simple,
                1 => SelectionKind::Block,
                2 => SelectionKind::Word,
                _ => SelectionKind::Line,
            };
            t.select_start(col, row, b(4) & 1 != 0, kind);
            t.select_update(usize::from(b(4)), usize::from(b(5)), b(4) & 2 != 0);
            let _ = t.selection_text();
            None
        }
        5 => {
            t.select_clear();
            None
        }
        6 => {
            t.search_open();
            t.search_next(b(1) & 1 != 0);
            None
        }
        7 => {
            t.search_close();
            None
        }
        8 => {
            let _ = t.link_at(col, row);
            None
        }
        // Mouse reports in the legacy encodings carry raw coordinate bytes.
        9 => {
            let _ = t.wheel(i32::from(b(1) as i8), col, row, mods);
            None
        }
        10 => {
            let _ = t.mouse(MouseEvent::Press(button), col, row, mods);
            None
        }
        11 => {
            let _ = t.mouse(MouseEvent::Release(button), col, row, mods);
            None
        }
        12 => {
            let held = Some(button).filter(|_| b(4) & 1 != 0);
            let _ = t.mouse(MouseEvent::Motion(held), col, row, mods);
            None
        }
        13 => t.focus(b(1) & 1 != 0),
        14 => {
            let key = match b(1) % 11 {
                0 => Key::Up,
                1 => Key::Down,
                2 => Key::Right,
                3 => Key::Left,
                4 => Key::Home,
                5 => Key::End,
                6 => Key::Insert,
                7 => Key::Delete,
                8 => Key::PageUp,
                9 => Key::PageDown,
                _ => Key::F(b(2) % 12 + 1),
            };
            t.key(key, mods)
        }
        _ => {
            t.set_cell_size(u32::from(b(1)), u32::from(b(2)));
            None
        }
    };
    if let Some(bytes) = reply {
        assert!(
            bytes.iter().all(|b| (0x20..0x7f).contains(b) || *b == 0x1b),
            "user reply {bytes:?}"
        );
    }
}
