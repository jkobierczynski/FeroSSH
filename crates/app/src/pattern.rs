//! The renderer test pattern as terminal output: palettes, truecolour,
//! styles, box drawing, wide and combining characters, all through escape
//! sequences, so it exercises the emulator and the renderer together.
//! Settings > Test pattern prints it.

use std::fmt::Write;

/// The pattern, 80 columns wide and 17 lines tall, ending with CRLF.
pub fn test_pattern() -> String {
    let mut s = String::new();
    let label = |s: &mut String, text: &str| {
        let _ = write!(s, "\x1b[90m{text:<8}\x1b[0m");
    };
    s.push_str("\x1b[0m\x1b[1mFeroSSH test pattern\x1b[0;90m \u{b7} milestone 2\x1b[0m\r\n\r\n");

    label(&mut s, "ANSI");
    for i in 0..16 {
        if i == 8 {
            s.push(' ');
        }
        let _ = write!(s, "\x1b[48;5;{i}m    ");
    }
    s.push_str("\x1b[0m\r\n");

    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    for (base, text) in [(30, "Text"), (90, "")] {
        label(&mut s, text);
        for (i, name) in NAMES.iter().enumerate() {
            let _ = write!(s, "\x1b[{}m{name}\x1b[0m ", base + i);
        }
        s.push_str("\r\n");
    }

    for line in 0..3 {
        label(&mut s, if line == 0 { "256" } else { "" });
        for i in 0..72 {
            let _ = write!(s, "\x1b[48;5;{}m ", 16 + line * 72 + i);
        }
        s.push_str("\x1b[0m\r\n");
    }

    label(&mut s, "Greys");
    for i in 0..24 {
        let _ = write!(s, "\x1b[48;5;{}m   ", 232 + i);
    }
    s.push_str("\x1b[0m\r\n");

    label(&mut s, "RGB");
    for i in 0..72 {
        let hue = i as f32 / 72.0;
        let (r, g, b) = hsv(hue, 0.8, 1.0);
        let (br, bg, bb) = hsv(hue, 0.9, 0.45);
        let _ = write!(s, "\x1b[38;2;{r};{g};{b};48;2;{br};{bg};{bb}m\u{2580}");
    }
    s.push_str("\x1b[0m\r\n");

    label(&mut s, "Styles");
    for (sgr, text) in [
        ("0", "normal"),
        ("1", "bold"),
        ("3", "italic"),
        ("1;3", "bold-italic"),
        ("4", "underline"),
        ("7", "inverse"),
        ("2", "dim"),
        ("9", "strike"),
    ] {
        let _ = write!(s, "\x1b[{sgr}m{text}\x1b[0m ");
    }
    s.push_str("\r\n");

    for (text, line) in [
        ("Boxes", "┌─┬─┐ ╔═╦═╗ ╭───╮ ░▒▓█ ▁▂▃▄▅▆▇█"),
        ("", "│ │ │ ║ ║ ║ │   │ ←↑→↓ ✓✗•…"),
        ("", "└─┴─┘ ╚═╩═╝ ╰───╯"),
    ] {
        label(&mut s, text);
        s.push_str(line);
        s.push_str("\r\n");
    }

    label(&mut s, "Unicode");
    // The second é is e plus a combining acute accent.
    s.push_str("café cafe\u{301} naïve Ελληνικά Кириллица 日本語 中文 한국어 🦀\r\n");
    label(&mut s, "Code");
    s.push_str("0O 1lI| {}[]() <= >= != -> => fn main() {}\r\n");
    label(&mut s, "Link");
    s.push_str("\x1b]8;;https://github.com/jkobierczynski/FeroSSH\x1b\\FeroSSH on GitHub\x1b]8;;\x1b\\ \x1b[90m(Ctrl+click)\x1b[0m\r\n");
    s
}

/// HSV to RGB bytes; all inputs in 0..=1.
fn hsv(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h6 = (h.fract() * 6.0).max(0.0);
    let f = h6.fract();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match h6 as u32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let byte = |x: f32| (x * 255.0).round().clamp(0.0, 255.0) as u8;
    (byte(r), byte(g), byte(b))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ferossh_render::{Color, Flags, Grid};
    use ferossh_term::{Settings, Terminal};

    #[test]
    fn pattern_fits_80_columns() {
        let mut t = Terminal::new(80, 24, &Settings::default());
        t.feed(test_pattern().as_bytes());
        let mut g = Grid::new(1, 1);
        let cursor = t.draw_into(&mut g, true).unwrap();
        assert_eq!((cursor.x, cursor.y), (0, 17));
        // Nothing wrapped: line 1 is the blank line after the heading.
        let text = |y: usize| -> String { g.row(y).unwrap().iter().map(|c| c.ch).collect() };
        assert!(text(0).starts_with("FeroSSH test pattern"));
        assert_eq!(text(1).trim(), "");
        assert!(text(2).starts_with("ANSI"));
        assert_eq!(g.cell(8, 2).unwrap().bg, Color::Indexed(0));
        assert!(text(10).contains("strike"));
        assert!(g.cell(15, 10).unwrap().flags.contains(Flags::BOLD));
        assert_eq!(
            t.link_at(8, 16).as_deref(),
            Some("https://github.com/jkobierczynski/FeroSSH")
        );
        assert_eq!(t.history_len(), 0);
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(hsv(1.0 / 3.0, 1.0, 1.0), (0, 255, 0));
        assert_eq!(hsv(2.0 / 3.0, 1.0, 1.0), (0, 0, 255));
    }
}
