//! Escape-sequence behaviour, checked the way vttest checks it: feed bytes,
//! then compare the screen and the replies.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use ferossh_render::{Cell, Color, CursorShape, Flags, Grid, Rgb, Theme};
use ferossh_term::{Action, Key, Mods, MouseButton, MouseEvent, SelectionKind, Settings, Terminal};

fn term(cols: usize, rows: usize) -> Terminal {
    Terminal::new(cols, rows, &Settings::default())
}

fn screen(t: &Terminal) -> (Grid, Option<ferossh_render::Cursor>) {
    let mut g = Grid::new(1, 1);
    let cursor = t.draw_into(&mut g, true);
    (g, cursor)
}

fn line(t: &Terminal, y: usize) -> String {
    let (g, _) = screen(t);
    g.row(y)
        .unwrap()
        .iter()
        .filter(|c| !c.flags.contains(Flags::WIDE_SPACER))
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn cell(t: &Terminal, x: usize, y: usize) -> Cell {
    *screen(t).0.cell(x, y).unwrap()
}

fn cursor(t: &Terminal) -> (usize, usize) {
    let c = screen(t).1.unwrap();
    (c.x, c.y)
}

fn replies(t: &mut Terminal) -> Vec<String> {
    t.take_actions()
        .into_iter()
        .filter_map(|a| match a {
            Action::Reply(b) => Some(String::from_utf8(b).unwrap()),
            _ => None,
        })
        .collect()
}

#[test]
fn text_wraps_and_line_feeds() {
    let mut t = term(10, 3);
    t.feed(b"hello\r\nabcdefghijKL");
    assert_eq!(line(&t, 0), "hello");
    assert_eq!(line(&t, 1), "abcdefghij");
    assert_eq!(line(&t, 2), "KL");
    assert_eq!(cursor(&t), (2, 2));
}

#[test]
fn cursor_movement_and_erase() {
    let mut t = term(20, 5);
    // Fill the screen with E (DECALN), as vttest's first screens do.
    t.feed(b"\x1b#8");
    assert_eq!(line(&t, 4), "E".repeat(20));
    // CUP, then erase to end of line, then erase below.
    t.feed(b"\x1b[2;5H\x1b[K\x1b[4;1H\x1b[J");
    assert_eq!(line(&t, 1), "EEEE");
    assert_eq!(line(&t, 3), "");
    assert_eq!(line(&t, 4), "");
    // Relative moves.
    t.feed(b"\x1b[1;1H\x1b[2B\x1b[3C*");
    assert_eq!(&line(&t, 2)[..5], "EEE*E");
}

#[test]
fn scroll_region() {
    let mut t = term(5, 5);
    t.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    // Region rows 2-4; a line feed at its bottom scrolls only the region.
    t.feed(b"\x1b[2;4r\x1b[4;1H\nX");
    let rows: Vec<_> = (0..5).map(|y| line(&t, y)).collect();
    assert_eq!(rows, ["1", "3", "4", "X", "5"]);
}

#[test]
fn colours_and_attributes() {
    let mut t = term(20, 2);
    t.feed(
        b"\x1b[1;31mA\x1b[0;38;5;196mB\x1b[38;2;1;2;3;48;5;4mC\x1b[0;3;4;7;9;2mD\x1b[0;91mE\x1b[0m",
    );
    let a = cell(&t, 0, 0);
    assert_eq!((a.fg, a.flags), (Color::Indexed(1), Flags::BOLD));
    assert_eq!(cell(&t, 1, 0).fg, Color::Indexed(196));
    let c = cell(&t, 2, 0);
    assert_eq!(c.fg, Color::Rgb(Rgb::new(1, 2, 3)));
    assert_eq!(c.bg, Color::Indexed(4));
    let d = cell(&t, 3, 0).flags;
    for f in [
        Flags::ITALIC,
        Flags::UNDERLINE,
        Flags::INVERSE,
        Flags::STRIKEOUT,
        Flags::DIM,
    ] {
        assert!(d.contains(f), "{f:?}");
    }
    assert_eq!(cell(&t, 4, 0).fg, Color::Indexed(9));
    assert_eq!(cell(&t, 5, 0).fg, Color::Default);
}

#[test]
fn wide_and_combining_characters() {
    let mut t = term(10, 2);
    t.feed("日e\u{301}x".as_bytes());
    let (g, _) = screen(&t);
    assert!(g.cell(0, 0).unwrap().flags.contains(Flags::WIDE));
    assert!(g.cell(1, 0).unwrap().flags.contains(Flags::WIDE_SPACER));
    let e = g.cell(2, 0).unwrap();
    assert_eq!(e.ch, 'e');
    assert_eq!(g.cluster(e), "\u{301}");
    assert_eq!(g.cell(3, 0).unwrap().ch, 'x');
}

#[test]
fn alternate_screen_restores_the_main_one() {
    let mut t = term(10, 3);
    t.feed(b"main");
    t.feed(b"\x1b[?1049h\x1b[2J\x1b[Halt");
    assert_eq!(line(&t, 0), "alt");
    t.feed(b"\x1b[?1049l");
    assert_eq!(line(&t, 0), "main");
    assert_eq!(cursor(&t), (4, 0));
}

#[test]
fn scrollback_keeps_the_configured_lines() {
    let mut t = Terminal::new(10, 5, &Settings::new(20));
    for i in 0..100 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    assert_eq!(t.history_len(), 20);
    assert_eq!(line(&t, 3), "line 99");
    t.scroll_to_top();
    assert_eq!(t.display_offset(), 20);
    assert_eq!(line(&t, 0), "line 76");
    // No cursor while it is scrolled out of view.
    assert!(screen(&t).1.is_none());
    t.scroll_to_bottom();
    assert_eq!(t.display_offset(), 0);
    t.clear_scrollback();
    assert_eq!(t.history_len(), 0);
}

#[test]
fn scrollback_default_and_limit() {
    assert_eq!(Settings::default().scrollback, 10_000);
    assert_eq!(
        Settings::new(usize::MAX).scrollback,
        Settings::MAX_SCROLLBACK
    );
}

#[test]
fn wheel_scrolls_history_or_sends_keys() {
    let mut t = term(10, 3);
    t.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    assert_eq!(t.wheel(1, 0, 0, Mods::NONE), None);
    assert_eq!(t.display_offset(), 1);
    t.scroll_to_bottom();
    // Full-screen programs get arrow keys for the wheel.
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.wheel(-2, 0, 0, Mods::NONE).unwrap(), b"\x1b[B\x1b[B");
    // With mouse reporting the wheel is a mouse button.
    t.feed(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(t.wheel(1, 2, 1, Mods::NONE).unwrap(), b"\x1b[<64;3;2M");
}

#[test]
fn mouse_modes_are_set_by_the_host() {
    let mut t = term(10, 3);
    assert!(!t.mouse_reporting());
    let press = MouseEvent::Press(MouseButton::Left);
    assert!(t.mouse(press, 0, 0, Mods::NONE).is_none());
    t.feed(b"\x1b[?1002h\x1b[?1006h");
    assert!(t.mouse_reporting());
    assert_eq!(t.mouse(press, 1, 2, Mods::NONE).unwrap(), b"\x1b[<0;2;3M");
    t.feed(b"\x1b[?1002l");
    assert!(!t.mouse_reporting());
}

#[test]
fn keys_follow_cursor_key_mode() {
    let mut t = term(10, 3);
    assert_eq!(t.key(Key::Up, Mods::NONE).unwrap(), b"\x1b[A");
    t.feed(b"\x1b[?1h");
    assert_eq!(t.key(Key::Up, Mods::NONE).unwrap(), b"\x1bOA");
    t.feed(b"\x1b[20h");
    assert_eq!(t.key(Key::Enter, Mods::NONE).unwrap(), b"\r\n");
}

#[test]
fn device_queries_get_fixed_replies() {
    let mut t = term(80, 24);
    t.feed(b"\x1b[3;7H\x1b[6n\x1b[c\x1b[5n");
    assert_eq!(replies(&mut t), ["\x1b[3;7R", "\x1b[?6c", "\x1b[0n"]);
    t.set_cell_size(9, 18);
    t.feed(b"\x1b[18t\x1b[14t");
    assert_eq!(replies(&mut t), ["\x1b[8;24;80t", "\x1b[4;432;720t"]);
}

#[test]
fn colour_queries_answer_from_the_theme() {
    let mut t = term(10, 2);
    let theme = Theme::by_name("One Half Light").unwrap_or_default();
    t.set_theme(&theme);
    t.feed(b"\x1b]10;?\x07\x1b]4;1;?\x1b\\");
    let fg = theme.foreground;
    let red = theme.ansi[1];
    let hex = |c: Rgb| {
        format!(
            "rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}",
            c.r, c.g, c.b
        )
    };
    assert_eq!(
        replies(&mut t),
        [
            format!("\x1b]10;{}\x07", hex(fg)),
            format!("\x1b]4;1;{}\x1b\\", hex(red))
        ]
    );
    // A colour the host redefined is reported and drawn as redefined.
    t.feed(b"\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07\x1b[31mR");
    assert_eq!(
        replies(&mut t),
        ["\x1b]4;1;rgb:1212/3434/5656\x07".to_owned()]
    );
    assert_eq!(cell(&t, 0, 0).fg, Color::Rgb(Rgb::new(0x12, 0x34, 0x56)));
}

#[test]
fn injection_prone_queries_are_not_answered() {
    let mut t = term(20, 2);
    t.feed(b"\x1b]2;rm -rf ~\x07");
    // Title report (CSI 21 t), icon label report (CSI 20 t), DECRQSS,
    // clipboard read (OSC 52 ?), and xterm's font query.
    t.feed(b"\x1b[21t\x1b[20t\x1bP$qm\x1b\\\x1b]52;c;?\x07\x1b]50;?\x07");
    let actions = t.take_actions();
    assert_eq!(actions, [Action::Title(Some("rm -rf ~".to_owned()))]);
}

#[test]
fn titles_are_sanitised() {
    let mut t = term(10, 2);
    t.feed("\x1b]0;a\u{202E}b\u{9b}c\x07\x1b]2;\x07".as_bytes());
    assert_eq!(
        t.take_actions(),
        [
            Action::Title(Some("abc".to_owned())),
            Action::Title(Some(String::new()))
        ]
    );
}

fn titles(t: &mut Terminal) -> Vec<Option<String>> {
    t.take_actions()
        .into_iter()
        .filter_map(|a| match a {
            Action::Title(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[test]
fn full_screen_apps_give_the_title_back() {
    let mc = Some("mc [user@Debian]:~".to_owned());
    // mc where terminfo has no title stack (smcup is only 1049h): the
    // title from before comes back when it leaves.
    let mut t = term(20, 4);
    t.feed(b"\x1b[?1049h\x1b]0;mc [user@Debian]:~\x07\x1b[?1049l");
    assert_eq!(titles(&mut t), [mc.clone(), None]);
    t.feed(b"\x1b]2;bash\x07\x1b[?1049h");
    t.feed(b"\x1b]0;mc [user@Debian]:~\x07");
    t.feed(b"\x1b[?1049l");
    assert_eq!(
        titles(&mut t),
        [Some("bash".to_owned()), mc.clone(), Some("bash".to_owned())]
    );
    // Byte by byte, and with other modes in the same sequence.
    for (enter, leave) in [("?1049h", "?1049l"), ("?1;1049h", "?1049;1l")] {
        let mut t = term(20, 4);
        let bytes = format!("\x1b[{enter}\x1b]0;mc\x07x\x1b[{leave}");
        for b in bytes.as_bytes() {
            t.feed(&[*b]);
        }
        assert_eq!(titles(&mut t), [Some("mc".to_owned()), None], "{enter}");
    }
    // With a title stack (xterm-256color smcup and rmcup), the app's own
    // restore and ours agree.
    let mut t = term(20, 4);
    t.feed(b"\x1b]2;bash\x07\x1b[?1049h\x1b[22;0;0t\x1b]0;mc\x07");
    t.feed(b"\x1b[?1049l\x1b[23;0;0t");
    assert_eq!(titles(&mut t).last(), Some(&Some("bash".to_owned())));
}

#[test]
fn titles_outside_full_screen_apps_stay() {
    // An app that leaves its title on purpose by setting it after the
    // alternate screen, and one that never changed it.
    let mut t = term(20, 4);
    t.feed(b"\x1b[?1049h\x1b[?1049l\x1b]0;job done\x07");
    assert_eq!(titles(&mut t), [Some("job done".to_owned())]);
    t.feed(b"\x1b[?1049hless\x1b[?1049l");
    assert_eq!(titles(&mut t), Vec::<Option<String>>::new());
    // An app that set the old title back itself.
    t.feed(b"\x1b[?1049h\x1b]0;vim\x07\x1b]0;job done\x07\x1b[?1049l");
    assert_eq!(
        titles(&mut t),
        [Some("vim".to_owned()), Some("job done".to_owned())]
    );
}

#[test]
fn clipboard_writes_need_confirmation() {
    let mut t = term(10, 2);
    // "hello" in base64, to the clipboard and to the primary selection.
    t.feed(b"\x1b]52;c;aGVsbG8=\x07\x1b]52;p;aGVsbG8=\x07");
    assert_eq!(
        t.take_actions(),
        [Action::ClipboardRequest("hello".to_owned())]
    );
}

#[test]
fn bell_is_reported() {
    let mut t = term(10, 2);
    t.feed(b"\x07");
    assert_eq!(t.take_actions(), [Action::Bell]);
}

#[test]
fn hyperlinks_are_kept_per_cell() {
    let mut t = term(20, 2);
    t.feed(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ plain");
    assert_eq!(t.link_at(0, 0).as_deref(), Some("https://example.com"));
    assert_eq!(t.link_at(3, 0).as_deref(), Some("https://example.com"));
    assert_eq!(t.link_at(5, 0), None);
}

#[test]
fn bracketed_paste_and_focus_reports() {
    let mut t = term(10, 2);
    assert_eq!(t.paste("a\nb").bytes, b"a\rb");
    t.feed(b"\x1b[?2004h");
    assert_eq!(t.paste("a\nb").bytes, b"\x1b[200~a\rb\x1b[201~");
    assert_eq!(t.focus(false), None);
    t.feed(b"\x1b[?1004h");
    assert_eq!(t.focus(false).unwrap(), b"\x1b[O");
    assert_eq!(t.focus(true).unwrap(), b"\x1b[I");
}

#[test]
fn mouse_selection_copies_text() {
    let mut t = term(20, 3);
    t.feed(b"one two three\r\nfour");
    t.select_start(4, 0, false, SelectionKind::Simple);
    t.select_update(6, 0, true);
    assert_eq!(t.selection_text().as_deref(), Some("two"));
    // Selected cells are drawn inverted.
    assert!(cell(&t, 5, 0).flags.contains(Flags::INVERSE));
    t.select_start(9, 0, false, SelectionKind::Word);
    assert_eq!(t.selection_text().as_deref(), Some("three"));
    t.select_start(0, 1, false, SelectionKind::Line);
    assert_eq!(t.selection_text().as_deref(), Some("four\n"));
    t.select_clear();
    assert!(!t.has_selection());
}

#[test]
fn search_finds_text_in_scrollback() {
    let mut t = term(40, 3);
    t.feed(b"needle one\r\n");
    for i in 0..50 {
        t.feed(format!("filler {i}\r\n").as_bytes());
    }
    t.feed(b"Needle two (x)\r\n");
    t.search_open();
    t.search_set("needle");
    // Case-insensitive, nearest first.
    assert!(t.search_found());
    assert_eq!(t.selection_text().as_deref(), Some("Needle"));
    t.search_next(true);
    assert_eq!(t.selection_text().as_deref(), Some("needle"));
    assert!(t.display_offset() > 40);
    // Capitals make it case-sensitive; regex syntax is literal.
    t.search_set("Needle two (x)");
    assert_eq!(t.selection_text().as_deref(), Some("Needle two (x)"));
    t.search_set("absent");
    assert!(!t.search_found());
    // The search bar takes the bottom row.
    let (g, cur) = screen(&t);
    let bar: String = g.row(2).unwrap().iter().map(|c| c.ch).collect();
    assert!(bar.starts_with(" Find: absent"), "{bar}");
    assert!(bar.contains("no match"));
    assert_eq!(cur.unwrap().shape, CursorShape::Beam);
    t.search_close();
    assert!(!t.search_active());
}

#[test]
fn synchronized_updates_wait_for_the_end() {
    let mut t = term(10, 2);
    t.feed(b"\x1b[?2026hhidden");
    assert_eq!(line(&t, 0), "");
    assert!(t.sync_deadline().is_some());
    t.flush_sync();
    assert_eq!(line(&t, 0), "hidden");
    t.feed(b"\x1b[?2026h!\x1b[?2026l");
    assert_eq!(line(&t, 0), "hidden!");
}

#[test]
fn resize_reflows_and_keeps_history() {
    let mut t = term(10, 3);
    t.feed(b"abcdefghijkl");
    t.resize(6, 3);
    assert_eq!(line(&t, 0), "abcdef");
    assert_eq!(line(&t, 1), "ghijkl");
    t.resize(20, 3);
    assert_eq!(line(&t, 0), "abcdefghijkl");
    t.resize(0, 0);
    assert_eq!((t.cols(), t.rows()), (2, 1));
}

#[test]
fn cursor_shape_follows_decscusr_and_focus() {
    let mut t = term(10, 2);
    assert_eq!(screen(&t).1.unwrap().shape, CursorShape::Block);
    t.feed(b"\x1b[6 q");
    assert_eq!(screen(&t).1.unwrap().shape, CursorShape::Beam);
    t.feed(b"\x1b[4 q");
    assert_eq!(screen(&t).1.unwrap().shape, CursorShape::Underline);
    let mut g = Grid::new(1, 1);
    assert_eq!(
        t.draw_into(&mut g, false).unwrap().shape,
        CursorShape::Hollow
    );
    t.feed(b"\x1b[?25l");
    assert!(screen(&t).1.is_none());
}

#[test]
fn unchanged_rows_stay_clean() {
    let mut t = term(10, 3);
    let mut g = Grid::new(1, 1);
    t.feed(b"a\r\nb");
    t.draw_into(&mut g, true);
    g.clear_dirty();
    t.feed(b"c");
    t.draw_into(&mut g, true);
    assert_eq!(g.dirty_span(), Some((1, 1)));
}

#[test]
fn garbage_does_not_panic() {
    let mut t = term(7, 3);
    // A cheap deterministic byte soup heavy in escape introducers.
    let mut x: u32 = 0x1234_5678;
    let mut bytes = Vec::new();
    for _ in 0..200_000 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let b = match x % 7 {
            0 => 0x1b,
            1 => b'[',
            2 => b';',
            3 => b'0' + (x >> 8) as u8 % 10,
            _ => (x >> 16) as u8,
        };
        bytes.push(b);
    }
    for chunk in bytes.chunks(97) {
        t.feed(chunk);
        let _ = t.take_actions();
    }
    t.resize(3, 2);
    t.feed(&bytes[..1000]);
    let _ = screen(&t);
}

#[test]
fn deleting_lines_while_scrolled_back_keeps_the_view_in_history() {
    // Three lines of history, viewed one line back.
    let mut t = Terminal::new(10, 5, &Settings::new(100));
    for i in 0..8 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    t.scroll_lines(1);
    // Delete lines from the middle of the screen, again and again. None
    // of them goes into history, but alacritty_terminal moves the view
    // back for each one.
    for _ in 0..10 {
        t.feed(b"\x1b[3H\x1b[2M");
        let _ = screen(&t);
    }
    // The view stops at the oldest line instead of reaching past it.
    assert_eq!(line(&t, 0), "line 0");
    // A scroll region under a status line does the same.
    t.feed(b"\x1b[2;5r\x1b[5H");
    for _ in 0..10 {
        t.feed(b"\n");
        let _ = screen(&t);
    }
    assert_eq!(line(&t, 0), "line 0");
}

/// Inputs the `term_feed` fuzz target once crashed or hung on, run the way
/// it runs them (without its user actions). They are the files in
/// `fuzz/regressions/term_feed`.
#[test]
fn fuzz_regressions_do_not_panic() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/regressions/term_feed"
    );
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let data = std::fs::read(entry.unwrap().path()).unwrap();
        let Some((&split, stream)) = data.split_first() else {
            continue;
        };
        count += 1;
        let mut t = Terminal::new(20, 6, &Settings::new(50));
        let mut g = Grid::new(1, 1);
        for (i, chunk) in stream.chunks(usize::from(split % 64).max(1)).enumerate() {
            t.feed(chunk);
            let _ = t.take_actions();
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
                _ => {}
            }
            let _ = t.draw_into(&mut g, i % 2 == 0);
        }
    }
    assert!(count >= 10, "only {count} regression inputs found");
}

#[test]
fn block_selection_from_the_right_half_of_the_last_column_is_empty() {
    let mut t = term(20, 6);
    t.feed(b"abcdefghijklmnopqrst\r\nabcdefghijklmnopqrst");
    // Alt+drag from the right half of the last cell, down and to its
    // left half: no whole cell is covered.
    t.select_start(19, 0, true, SelectionKind::Block);
    t.select_update(19, 1, false);
    assert_eq!(t.selection_text(), None);
    t.select_update(19, 1, true);
    assert_eq!(t.selection_text(), None);
    let _ = screen(&t);
    // From the left half it covers the last column.
    t.select_start(19, 0, false, SelectionKind::Block);
    t.select_update(19, 1, true);
    assert_eq!(t.selection_text().as_deref(), Some("t\nt"));
}

#[test]
fn copying_a_line_that_wraps_a_wide_character_takes_it_from_the_next_line() {
    // "中" does not fit in the last column, so it wraps and leaves a
    // spacer there.
    let mut t = Terminal::new(10, 3, &Settings::new(100));
    t.feed("123456789中x\r\nb\r\nc\r\nd\r\ne".as_bytes());
    // The wrapped line is now the oldest line of history.
    t.scroll_to_top();
    assert_eq!(line(&t, 0), "123456789");
    t.select_start(0, 0, false, SelectionKind::Simple);
    t.select_update(9, 0, true);
    assert_eq!(t.selection_text().as_deref(), Some("123456789中"));
    t.select_start(0, 0, false, SelectionKind::Block);
    t.select_update(9, 0, true);
    assert_eq!(t.selection_text().as_deref(), Some("123456789中"));
    t.select_start(3, 0, false, SelectionKind::Line);
    assert_eq!(t.selection_text().as_deref(), Some("123456789中x\n"));

    // With a line above it, upstream would have added that line's first
    // character.
    let mut t = Terminal::new(10, 3, &Settings::new(100));
    t.feed("a\r\n123456789中x".as_bytes());
    t.select_start(0, 1, false, SelectionKind::Simple);
    t.select_update(9, 1, true);
    assert_eq!(t.selection_text().as_deref(), Some("123456789中"));
}

#[test]
fn deleting_a_wide_character_leaves_no_spacer_under_the_cursor() {
    // CSI P on "中" at the start of the line moves its spacer to column 0,
    // where the cursor is.
    let mut t = term(10, 2);
    t.feed("中x\r\x1b[P".as_bytes());
    let (_, cursor) = screen(&t);
    assert_eq!(cursor.map(|c| (c.x, c.y)), Some((0, 0)));
    // The left-over spacer shows as a blank cell.
    assert_eq!(line(&t, 0), " x");
}

#[test]
fn a_synchronized_update_keeps_the_view_in_history() {
    let mut t = Terminal::new(10, 5, &Settings::new(100));
    for i in 0..8 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    t.scroll_lines(1);
    // The same deletes as above, held back by a synchronized update and
    // applied when it ends.
    t.feed(b"\x1b[?2026h");
    for _ in 0..10 {
        t.feed(b"\x1b[3H\x1b[2M");
    }
    t.flush_sync();
    let _ = screen(&t);
    assert_eq!(line(&t, 0), "line 0");
}

#[test]
fn searching_past_a_wide_character_that_was_overwritten_ends() {
    // From a fuzzer find: narrowing the terminal leaves a line ending in
    // a spacer that says a wide character goes on at the start of the
    // next line, and "M" then overwrites that character. Upstream's
    // search loops forever on such a line.
    let mut t = Terminal::new(20, 6, &Settings::new(50));
    t.feed(b"M\xa5\xe4");
    t.resize(18, 1);
    t.feed(b"\xa2\x95\x00\xff\xff\xe4\xa2\x95\x00\x08\xcd");
    t.resize(25, 1);
    t.feed(b"\xa5\xe4\xa2\x95\x00\x00\x1b\x00\x08\xa2\x08");
    t.resize(8, 1);
    t.feed(b"M");
    t.scroll_lines(1);
    t.search_open();
    t.search_set(" ");
    let _ = screen(&t);
}

#[test]
fn prompt_line_is_the_text_before_the_cursor() {
    let mut t = term(40, 5);
    t.feed(b"total 0\r\nuser@Debian:~/Downloads$ ");
    assert_eq!(
        t.prompt_line().as_deref(),
        Some("user@Debian:~/Downloads$ ")
    );
    assert_eq!(
        t.prompt_line()
            .as_deref()
            .and_then(ferossh_term::prompt_dir)
            .map(|d| d.path),
        Some(b"~/Downloads".to_vec())
    );
    t.feed(b"ls");
    assert_eq!(
        t.prompt_line().as_deref(),
        Some("user@Debian:~/Downloads$ ls")
    );
    // Full-screen programs draw no prompt.
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.prompt_line(), None);
}
