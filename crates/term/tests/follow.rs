//! Following the output: stopping at a stop pattern's match, holding the
//! view still while output goes on, and following again. Also keyword
//! highlighting as drawn.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::rc::Rc;

use ferossh_render::{Color, Flags, Grid};
use ferossh_term::highlight::{Highlighter, Rule, Style};
use ferossh_term::{Settings, Terminal};

fn rows(t: &Terminal) -> Vec<String> {
    let mut g = Grid::new(1, 1);
    t.draw_into(&mut g, true);
    (0..g.rows())
        .map(|y| {
            g.row(y)
                .unwrap()
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn lines(t: &mut Terminal, from: usize, to: usize) {
    for i in from..to {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
}

#[test]
fn stops_at_a_match_and_holds_the_view() {
    let mut t = Terminal::new(40, 5, &Settings::new(1000));
    t.set_stop_pattern(Some(r"(?i)\berror\b")).unwrap();
    lines(&mut t, 0, 10);
    assert!(!t.stopped());
    t.feed(b"ERROR: disk full\r\n");
    assert!(t.stopped());
    let held = rows(&t);
    assert!(
        held.iter().any(|r| r.starts_with("ERROR: disk full")),
        "{held:?}"
    );
    // More output, also in big bursts.
    lines(&mut t, 10, 50);
    let burst: String = (50..400).map(|i| format!("line {i}\r\n")).collect();
    t.feed(burst.as_bytes());
    // The badge covers the top right corner, and the bottom row was the
    // cursor's, which the next line filled.
    let now = rows(&t);
    // It followed until the line reached the middle row, then held.
    assert_eq!(now[1..4], ["line 9", "ERROR: disk full", "line 10"]);
    // Typing or Shift+End follows again.
    t.scroll_to_bottom();
    assert!(!t.stopped());
    assert_eq!(t.display_offset(), 0);
    assert!(rows(&t).iter().any(|r| r == "line 399"));
}

#[test]
fn holds_once_the_history_is_full() {
    let mut t = Terminal::new(40, 5, &Settings::new(20));
    t.set_stop_pattern(Some("fatal")).unwrap();
    lines(&mut t, 0, 100);
    assert_eq!(t.history_len(), 20);
    t.feed(b"fatal: no\r\nline a\r\n");
    assert!(t.stopped());
    let held = rows(&t);
    lines(&mut t, 100, 108);
    assert_eq!(rows(&t)[1..4], held[1..4]);
}

#[test]
fn scrolling_down_to_the_bottom_follows_again() {
    let mut t = Terminal::new(40, 5, &Settings::new(100));
    t.set_stop_pattern(Some("boom")).unwrap();
    lines(&mut t, 0, 10);
    t.feed(b"boom\r\n");
    lines(&mut t, 10, 30);
    assert!(t.stopped());
    assert!(t.display_offset() > 0);
    t.scroll_pages(-100);
    assert!(!t.stopped());
    // The same line is not stopped at again, the next match is.
    lines(&mut t, 30, 40);
    assert!(!t.stopped());
    t.feed(b"boom again\r\n");
    assert!(t.stopped());
}

#[test]
fn a_match_split_over_two_feeds_is_found() {
    let mut t = Terminal::new(40, 5, &Settings::new(100));
    t.set_stop_pattern(Some("segfault")).unwrap();
    t.feed(b"prog: seg");
    assert!(!t.stopped());
    t.feed(b"fault\r\n");
    assert!(t.stopped());
}

#[test]
fn no_stop_on_the_alternate_screen_or_once_cleared() {
    let mut t = Terminal::new(40, 5, &Settings::new(100));
    t.set_stop_pattern(Some("error")).unwrap();
    t.feed(b"\x1b[?1049herror in vim\r\n\x1b[?1049l");
    assert!(!t.stopped());
    t.set_stop_pattern(None).unwrap();
    assert!(!t.stop_armed());
    t.feed(b"error\r\n");
    assert!(!t.stopped());
}

#[test]
fn bad_stop_patterns_are_refused() {
    let mut t = Terminal::new(40, 5, &Settings::default());
    assert!(t.set_stop_pattern(Some("(")).is_err());
    assert!(!t.stop_armed());
}

#[test]
fn stop_bar_sets_the_pattern() {
    let mut t = Terminal::new(60, 5, &Settings::default());
    t.stop_bar_open();
    t.search_set("(");
    t.search_next(true);
    assert!(t.search_active(), "an invalid pattern keeps the bar open");
    assert!(rows(&t)[4].contains("not a valid pattern"));
    t.search_set("oops");
    t.search_next(true);
    assert!(!t.search_active());
    assert_eq!(t.stop_pattern(), Some("oops"));
    t.feed(b"oops\r\n");
    assert!(t.stopped());
    assert!(rows(&t)[0].ends_with("End follows"));
}

#[test]
fn keywords_are_highlighted_on_the_main_screen_only() {
    let mut t = Terminal::new(40, 3, &Settings::default());
    let (h, errors) = Highlighter::new(&[Rule {
        pattern: r"(?i)\bwarn(ing)?\b".into(),
        style: Style {
            fg: Some(Color::Indexed(3)),
            bold: true,
            ..Style::default()
        },
    }]);
    assert!(errors.is_empty());
    t.set_highlighter(h.map(Rc::new));
    t.feed(b"a Warning here \x1b[32mwarn\x1b[0m");
    let mut g = Grid::new(1, 1);
    t.draw_into(&mut g, true);
    let row = g.row(0).unwrap();
    assert_eq!(row[2].fg, Color::Indexed(3));
    assert!(row[2].flags.contains(Flags::BOLD));
    assert_eq!(row[1].fg, Color::Default);
    // The host's own colour wins.
    assert_eq!(row[15].fg, Color::Indexed(2));
    t.feed(b"\x1b[?1049h\x1b[Hwarning");
    t.draw_into(&mut g, true);
    assert_eq!(g.row(0).unwrap()[0].fg, Color::Default);
}

#[test]
fn the_line_stopped_at_is_not_stopped_at_again() {
    let mut t = Terminal::new(40, 5, &Settings::new(100));
    t.set_stop_pattern(Some("fault")).unwrap();
    t.feed(b"seg");
    t.feed(b"fault");
    assert!(t.stopped());
    t.scroll_to_bottom();
    t.feed(b" more on the same line\r\n");
    assert!(!t.stopped());
}

#[test]
fn random_output_with_a_stop_pattern_does_not_panic() {
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    const ALPHABET: &[u8] = b"ab \r\n\x1b[0123456789;?hlJKLMSTDEr\x0b\x0c\x1b[?1049h\x1b[3J";
    let mut t = Terminal::new(20, 6, &Settings::new(30));
    t.set_stop_pattern(Some("ab+a")).unwrap();
    for round in 0..3000 {
        let len = (next() % 200) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
            .collect();
        t.feed(&bytes);
        match next() % 13 {
            0 => t.scroll_to_bottom(),
            1 => t.scroll_pages(1),
            2 => t.resize(10 + (next() % 30) as usize, 3 + (next() % 8) as usize),
            3 => t.clear_scrollback(),
            4 => t.flush_sync(),
            _ => {}
        }
        let _ = rows(&t);
        assert!(t.display_offset() <= t.history_len(), "round {round}");
    }
}

#[test]
fn reading_the_scrollback_is_not_interrupted() {
    let mut t = Terminal::new(40, 5, &Settings::new(100));
    t.set_stop_pattern(Some("boom")).unwrap();
    lines(&mut t, 0, 20);
    t.scroll_lines(5);
    let reading = rows(&t);
    t.feed(b"boom\r\n");
    assert!(!t.stopped());
    assert_eq!(rows(&t), reading);
}

#[test]
fn scroll_up_sequences_are_counted_in_full() {
    let mut t = Terminal::new(40, 30, &Settings::new(2000));
    t.set_stop_pattern(Some("boom")).unwrap();
    // Fill the history first, so its size counts nothing by itself.
    lines(&mut t, 0, 2100);
    t.feed(b"boom\r\n");
    assert!(t.stopped());
    // 20 screens in one burst (more than the 512 lines a part may
    // scroll), with a sequence split across two feeds.
    let burst = "\x1b[30S".repeat(20);
    let (a, b) = burst.as_bytes().split_at(3);
    t.feed(a);
    t.feed(b);
    let now = rows(&t);
    assert_eq!(now[13..15], ["line 2099", "boom"]);
}

#[test]
fn matches_are_shown_in_the_middle_row() {
    let mut t = Terminal::new(40, 9, &Settings::new(100));
    lines(&mut t, 0, 50);
    t.search_open();
    t.search_set("line 30");
    assert_eq!(rows(&t)[4], "line 30");
    t.search_close();
    t.scroll_to_bottom();
    t.set_stop_pattern(Some("boom")).unwrap();
    t.feed(b"boom\r\n");
    lines(&mut t, 50, 60);
    assert_eq!(rows(&t)[4], "boom");
}

#[test]
fn clearing_the_pattern_drops_the_stop_and_its_selection() {
    let mut t = Terminal::new(40, 5, &Settings::new(100));
    t.set_stop_pattern(Some("boom")).unwrap();
    t.feed(b"boom\r\n");
    assert!(t.stopped());
    assert_eq!(t.selection_text().as_deref(), Some("boom"));
    t.set_stop_pattern(None).unwrap();
    assert!(!t.stopped());
    assert_eq!(t.selection_text(), None);
}

#[test]
fn a_selection_made_while_stopped_stays() {
    let mut t = Terminal::new(40, 5, &Settings::new(100));
    t.set_stop_pattern(Some("boom")).unwrap();
    t.feed(b"line a\r\nboom\r\n");
    assert!(t.stopped());
    t.select_start(0, 1, false, ferossh_term::SelectionKind::Line);
    let picked = t.selection_text();
    assert!(picked.is_some());
    t.set_stop_pattern(None).unwrap();
    assert_eq!(t.selection_text(), picked);
}
