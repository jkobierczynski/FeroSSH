//! A local echo screen: the renderer test pattern plus a prompt that echoes
//! what you type. The terminal emulator replaces it in milestone 2.

use ferossh_render::{Cell, Color, Flags, Grid, Rgb};
use unicode_width::UnicodeWidthChar;

pub const PROMPT: &str = "local echo> ";

#[derive(Clone, Copy, Debug, Default)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
}

/// A grid with a cursor that echoes input locally.
#[derive(Debug)]
pub struct EchoScreen {
    grid: Grid,
    x: usize,
    y: usize,
    pen: Pen,
    /// Where the current input line starts, as (row, column); backspace
    /// stops there.
    input_start: (usize, usize),
}

impl EchoScreen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut s = Self {
            grid: Grid::new(cols, rows),
            x: 0,
            y: 0,
            pen: Pen::default(),
            input_start: (0, 0),
        };
        s.reset();
        s
    }

    /// Just a prompt, no test pattern.
    #[cfg(test)]
    fn bare(cols: usize, rows: usize) -> Self {
        let mut s = Self {
            grid: Grid::new(cols, rows),
            x: 0,
            y: 0,
            pen: Pen::default(),
            input_start: (0, 0),
        };
        s.prompt();
        s
    }

    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.grid
    }

    /// The cell the cursor is drawn on.
    pub fn cursor(&self) -> (usize, usize) {
        (self.x.min(self.grid.cols() - 1), self.y)
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
        let (cols, rows) = (self.grid.cols(), self.grid.rows());
        self.x = self.x.min(cols);
        if self.y >= rows {
            self.y = rows - 1;
            self.input_start = (self.y, 0);
        }
        self.input_start.1 = self.input_start.1.min(cols);
    }

    /// Clears the screen and draws the test pattern and a fresh prompt.
    pub fn reset(&mut self) {
        self.grid.clear();
        self.x = 0;
        self.y = 0;
        self.test_pattern();
        self.prompt();
    }

    /// Handles one typed character.
    pub fn input(&mut self, ch: char) {
        match ch {
            '\r' | '\n' => {
                self.newline();
                self.prompt();
            }
            '\u{8}' | '\u{7f}' => self.backspace(),
            '\t' => {
                for _ in 0..8 - self.x % 8 {
                    self.put(' ');
                }
            }
            // Ctrl+L, as in a shell.
            '\u{c}' => self.reset(),
            c if u32::from(c) < 0x20 => {
                // Other control characters echo in caret notation (^C).
                if let Some(caret) = char::from_u32(u32::from(c) + 0x40) {
                    self.with_pen(Color::Indexed(8), Flags::empty(), |s| {
                        s.put('^');
                        s.put(caret);
                    });
                }
            }
            c if c.is_control() => {}
            c => self.put(c),
        }
    }

    fn put(&mut self, ch: char) {
        let width = ch.width().unwrap_or(0);
        let cols = self.grid.cols();
        // Combining marks need grapheme clusters, which arrive with the
        // terminal emulator in milestone 2.
        if width == 0 || width > cols {
            return;
        }
        if self.x + width > cols {
            self.newline();
        }
        let (x, y) = (self.x, self.y);
        for i in 0..width {
            self.unpair_wide(x + i, y);
        }
        let Pen { fg, bg, flags } = self.pen;
        if width == 2 {
            self.set(
                x,
                y,
                Cell {
                    ch,
                    fg,
                    bg,
                    flags: flags.union(Flags::WIDE),
                },
            );
            self.set(
                x + 1,
                y,
                Cell {
                    ch: ' ',
                    fg,
                    bg,
                    flags: flags.union(Flags::WIDE_SPACER),
                },
            );
        } else {
            self.set(x, y, Cell { ch, fg, bg, flags });
        }
        self.x += width;
    }

    fn set(&mut self, x: usize, y: usize, cell: Cell) {
        if let Some(c) = self.grid.cell_mut(x, y) {
            *c = cell;
        }
    }

    /// Blanks the other half of a wide character about to be overwritten.
    fn unpair_wide(&mut self, x: usize, y: usize) {
        let Some(flags) = self.grid.cell(x, y).map(|c| c.flags) else {
            return;
        };
        if flags.contains(Flags::WIDE) {
            self.set(x + 1, y, Cell::BLANK);
        } else if flags.contains(Flags::WIDE_SPACER) && x > 0 {
            self.set(x - 1, y, Cell::BLANK);
        }
    }

    fn newline(&mut self) {
        self.x = 0;
        if self.y + 1 < self.grid.rows() {
            self.y += 1;
        } else {
            self.grid.scroll_up();
            self.input_start = match self.input_start {
                (0, _) => (0, 0),
                (y, x) => (y - 1, x),
            };
        }
    }

    fn backspace(&mut self) {
        if (self.y, self.x) <= self.input_start {
            return;
        }
        if self.x == 0 {
            self.y -= 1;
            self.x = self.grid.cols();
        }
        self.x -= 1;
        let spacer = self
            .grid
            .cell(self.x, self.y)
            .is_some_and(|c| c.flags.contains(Flags::WIDE_SPACER));
        if spacer && self.x > 0 {
            self.x -= 1;
        }
        self.unpair_wide(self.x, self.y);
        self.set(self.x, self.y, Cell::BLANK);
    }

    fn prompt(&mut self) {
        self.with_pen(Color::Indexed(10), Flags::BOLD, |s| s.text(PROMPT));
        self.input_start = (self.y, self.x);
    }

    fn text(&mut self, s: &str) {
        s.chars().for_each(|c| self.put(c));
    }

    fn with_pen(&mut self, fg: Color, flags: Flags, f: impl FnOnce(&mut Self)) {
        self.with_colours(fg, Color::Default, flags, f);
    }

    fn with_colours(&mut self, fg: Color, bg: Color, flags: Flags, f: impl FnOnce(&mut Self)) {
        let saved = self.pen;
        self.pen = Pen { fg, bg, flags };
        f(self);
        self.pen = saved;
    }

    fn label(&mut self, s: &str) {
        self.with_pen(Color::Indexed(8), Flags::empty(), |me| {
            me.text(&format!("{s:<8}"))
        });
    }

    fn swatch(&mut self, bg: Color, width: usize) {
        self.with_colours(Color::Default, bg, Flags::empty(), |s| {
            (0..width).for_each(|_| s.put(' '));
        });
    }

    /// Eighteen lines, 80 columns wide, exercising what the renderer must
    /// get right: palettes, truecolour, styles, box drawing, wide glyphs.
    fn test_pattern(&mut self) {
        self.with_pen(Color::Default, Flags::BOLD, |s| {
            s.text("FeroSSH renderer test pattern")
        });
        self.with_pen(Color::Indexed(8), Flags::empty(), |s| {
            s.text(" \u{b7} milestone 1")
        });
        self.newline();
        self.newline();

        self.label("ANSI");
        for i in 0..16 {
            if i == 8 {
                self.put(' ');
            }
            self.swatch(Color::Indexed(i), 4);
        }
        self.newline();

        const NAMES: [&str; 8] = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        for (bright, label) in [(0, "Text"), (8, "")] {
            self.label(label);
            for (i, name) in (0u8..).zip(NAMES) {
                self.with_pen(Color::Indexed(i + bright), Flags::empty(), |s| s.text(name));
                self.put(' ');
            }
            self.newline();
        }

        for line in 0..3u8 {
            self.label(if line == 0 { "256" } else { "" });
            for i in 0..72 {
                self.swatch(Color::Indexed(16 + line * 72 + i), 1);
            }
            self.newline();
        }

        self.label("Greys");
        for i in 0..24 {
            self.swatch(Color::Indexed(232 + i), 3);
        }
        self.newline();

        self.label("RGB");
        for i in 0..72 {
            let hue = i as f32 / 72.0;
            let top = Color::Rgb(hsv(hue, 0.8, 1.0));
            let bottom = Color::Rgb(hsv(hue, 0.9, 0.45));
            self.with_colours(top, bottom, Flags::empty(), |s| s.put('\u{2580}'));
        }
        self.newline();

        self.label("Styles");
        for (text, flags) in [
            ("normal", Flags::empty()),
            ("bold", Flags::BOLD),
            ("italic", Flags::ITALIC),
            ("bold italic", Flags::BOLD.union(Flags::ITALIC)),
            ("underline", Flags::UNDERLINE),
            ("inverse", Flags::INVERSE),
        ] {
            self.with_pen(Color::Default, flags, |s| s.text(text));
            self.put(' ');
        }
        self.newline();

        for (label, line) in [
            ("Boxes", "┌─┬─┐ ╔═╦═╗ ╭───╮ ░▒▓█ ▁▂▃▄▅▆▇█"),
            ("", "│ │ │ ║ ║ ║ │   │ ←↑→↓ ✓✗•…"),
            ("", "└─┴─┘ ╚═╩═╝ ╰───╯"),
        ] {
            self.label(label);
            self.text(line);
            self.newline();
        }

        self.label("Unicode");
        self.text("café naïve Ελληνικά Кириллица 日本語 中文 한국어 🦀");
        self.newline();
        self.label("Code");
        self.text("0O 1lI| {}[]() <= >= != -> => fn main() {}");
        self.newline();
        self.newline();
        self.with_pen(Color::Indexed(8), Flags::empty(), |s| {
            s.text("Type to test local echo. Ctrl+L redraws this. Ctrl+= / Ctrl+- / Ctrl+0 zoom.");
        });
        self.newline();
    }
}

/// HSV to RGB; all inputs in 0..=1.
fn hsv(h: f32, s: f32, v: f32) -> Rgb {
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
    Rgb::new(byte(r), byte(g), byte(b))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn line(s: &EchoScreen, y: usize) -> String {
        s.grid()
            .row(y)
            .unwrap()
            .iter()
            .filter(|c| !c.flags.contains(Flags::WIDE_SPACER))
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn typed(s: &mut EchoScreen, text: &str) {
        text.chars().for_each(|c| s.input(c));
    }

    #[test]
    fn pattern_fits_the_default_size() {
        let s = EchoScreen::new(80, 24);
        let (x, y) = s.cursor();
        assert_eq!((x, y), (PROMPT.len(), 18));
        assert_eq!(
            line(&s, 0),
            "FeroSSH renderer test pattern \u{b7} milestone 1"
        );
        assert!((0..24).all(|y| s.grid().row(y).unwrap().len() == 80));
    }

    #[test]
    fn echo_and_backspace_stop_at_the_prompt() {
        let mut s = EchoScreen::new(80, 24);
        let y = s.cursor().1;
        typed(&mut s, "ls\u{8}\u{8}\u{8}\u{8}pwd");
        assert_eq!(line(&s, y), format!("{PROMPT}pwd"));
        typed(&mut s, "\r");
        assert_eq!(s.cursor(), (PROMPT.len(), y + 1));
    }

    #[test]
    fn wide_characters_take_two_cells_and_wrap_whole() {
        let mut s = EchoScreen::bare(14, 4);
        let y = 0;
        // The prompt is 12 cells; one wide char fits, the next wraps.
        typed(&mut s, "日本");
        assert_eq!(s.cursor(), (2, y + 1));
        let first = s.grid().cell(12, y).unwrap();
        assert!(first.flags.contains(Flags::WIDE));
        assert!(
            s.grid()
                .cell(13, y)
                .unwrap()
                .flags
                .contains(Flags::WIDE_SPACER)
        );
        // Backspace removes both halves and crosses the wrap.
        typed(&mut s, "\u{8}\u{8}");
        assert_eq!(s.cursor(), (12, y));
        assert_eq!(*s.grid().cell(12, y).unwrap(), Cell::BLANK);
        assert_eq!(*s.grid().cell(13, y).unwrap(), Cell::BLANK);
    }

    #[test]
    fn bottom_line_scrolls() {
        let mut s = EchoScreen::bare(20, 3);
        for _ in 0..5 {
            typed(&mut s, "x\r");
        }
        assert_eq!(s.cursor().1, 2);
        assert_eq!(line(&s, 1), format!("{PROMPT}x"));
        typed(&mut s, "\u{8}");
        assert_eq!(line(&s, 1), format!("{PROMPT}x"));
    }

    #[test]
    fn control_characters_echo_as_carets() {
        let mut s = EchoScreen::new(80, 24);
        let y = s.cursor().1;
        typed(&mut s, "\u{3}");
        assert_eq!(line(&s, y), format!("{PROMPT}^C"));
    }

    #[test]
    fn resize_keeps_the_cursor_inside() {
        let mut s = EchoScreen::new(80, 24);
        s.resize(10, 5);
        let (x, y) = s.cursor();
        assert!(x < 10 && y < 5);
        typed(&mut s, "abc\u{8}");
        s.resize(1, 1);
        assert_eq!(s.cursor(), (0, 0));
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv(0.0, 1.0, 1.0), Rgb::new(255, 0, 0));
        assert_eq!(hsv(1.0 / 3.0, 1.0, 1.0), Rgb::new(0, 255, 0));
        assert_eq!(hsv(2.0 / 3.0, 1.0, 1.0), Rgb::new(0, 0, 255));
    }
}
