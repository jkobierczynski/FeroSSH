//! The terminal state Mosh synchronises: a screen of cells and the modes
//! that show.
//!
//! The server sends each new state as terminal output that turns an older
//! state's screen into the new one. That older state need not be the one
//! on show here (the server may not have heard our acknowledgement yet),
//! so the worker keeps the screens of the states the server may still
//! refer to and applies each diff to the right one: [`Screen::apply`]
//! reads the output with the `vte` parser into a small VT100-style
//! screen. [`render`] then writes the output that turns the pane's
//! terminal from what it shows into a new screen.
//!
//! The screen handles everything the server writes (text, wide and
//! combining characters, cursor moves, erasing, scrolling regions,
//! renditions, modes, titles, hyperlinks, the clipboard) and the usual
//! VT100 controls besides; tests against a real `mosh-server` check that
//! it reads output as the server does. Input is hostile: sizes are
//! capped, and nothing panics or grows unbounded.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use unicode_width::UnicodeWidthChar;

/// The biggest screen accepted, either way.
pub const MAX_SIDE: usize = 1000;
/// Bytes of text one cell may hold (base and combining characters).
const CELL_TEXT: usize = 32;
/// Characters kept of a title, as Mosh does.
const TITLE: usize = 256;
/// Bytes kept of an OSC string (a clipboard copy).
const OSC_MAX: usize = 16 * 1024;

/// A colour: the terminal's default, one of 256, or 24-bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 1 << 2;
pub const UNDERLINE: u8 = 1 << 3;
pub const BLINK: u8 = 1 << 4;
pub const INVERSE: u8 = 1 << 5;
pub const INVISIBLE: u8 = 1 << 6;
const FAINT: u8 = 1 << 1;

/// How text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rendition {
    pub attrs: u8,
    pub fg: Color,
    pub bg: Color,
}

impl Rendition {
    /// A blank cell's look: only the background colour.
    fn blank(bg: Color) -> Self {
        Self {
            bg,
            ..Self::default()
        }
    }

    /// The SGR sequence that sets exactly this look.
    pub fn sgr(&self) -> String {
        let mut s = String::from("\x1b[0");
        for (bit, code) in [
            (BOLD, ";1"),
            (FAINT, ";2"),
            (ITALIC, ";3"),
            (UNDERLINE, ";4"),
            (BLINK, ";5"),
            (INVERSE, ";7"),
            (INVISIBLE, ";8"),
        ] {
            if self.attrs & bit != 0 {
                s.push_str(code);
            }
        }
        for (color, base, bright, ext) in [(self.fg, 30, 90, 38), (self.bg, 40, 100, 48)] {
            match color {
                Color::Default => {}
                Color::Indexed(n) if n < 8 => s.push_str(&format!(";{}", base + u16::from(n))),
                Color::Indexed(n) if n < 16 => {
                    s.push_str(&format!(";{}", bright + u16::from(n) - 8));
                }
                Color::Indexed(n) => s.push_str(&format!(";{ext};5;{n}")),
                Color::Rgb(r, g, b) => s.push_str(&format!(";{ext};2;{r};{g};{b}")),
            }
        }
        s.push('m');
        s
    }

    fn set(&mut self, n: u32) {
        match n {
            0 => *self = Self::default(),
            39 => self.fg = Color::Default,
            49 => self.bg = Color::Default,
            30..=37 => self.fg = Color::Indexed((n - 30) as u8),
            40..=47 => self.bg = Color::Indexed((n - 40) as u8),
            90..=97 => self.fg = Color::Indexed((n - 90 + 8) as u8),
            100..=107 => self.bg = Color::Indexed((n - 100 + 8) as u8),
            1 | 22 => self.flag(BOLD, n < 9),
            2 => self.flag(FAINT, true),
            3 | 23 => self.flag(ITALIC, n < 9),
            4 | 24 => self.flag(UNDERLINE, n < 9),
            5 | 25 => self.flag(BLINK, n < 9),
            7 | 27 => self.flag(INVERSE, n < 9),
            8 | 28 => self.flag(INVISIBLE, n < 9),
            _ => {}
        }
        if n == 22 {
            self.flag(FAINT, false);
        }
    }

    fn flag(&mut self, bit: u8, on: bool) {
        if on {
            self.attrs |= bit;
        } else {
            self.attrs &= !bit;
        }
    }
}

/// An OSC 8 hyperlink: its parameters and address.
pub type Link = Option<Arc<(String, String)>>;

fn osc8(link: &Link) -> String {
    match link {
        Some(l) => format!("\x1b]8;{};{}\x1b\\", l.0, l.1),
        None => "\x1b]8;;\x1b\\".to_owned(),
    }
}

/// One character cell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// The base character and any combining ones; empty when blank.
    pub text: String,
    pub rend: Rendition,
    pub link: Link,
    pub wide: bool,
    /// The text starts with a combining character.
    pub fallback: bool,
    /// The row wraps into the next (kept on the last cell).
    pub wrap: bool,
}

impl Cell {
    fn blank(bg: Color) -> Self {
        Self {
            rend: Rendition::blank(bg),
            ..Self::default()
        }
    }

    pub fn width(&self) -> usize {
        usize::from(self.wide) + 1
    }

    fn push(&mut self, c: char) {
        if self.text.len() + c.len_utf8() <= CELL_TEXT {
            self.text.push(c);
        }
    }

    fn grapheme(&self, out: &mut String) {
        if self.text.is_empty() {
            out.push(' ');
            return;
        }
        if self.fallback {
            out.push('\u{a0}');
        }
        out.push_str(&self.text);
    }
}

fn next_gen() -> u64 {
    static GEN: AtomicU64 = AtomicU64::new(0);
    GEN.fetch_add(1, Ordering::Relaxed)
}

/// A row of cells. Rows made at different times differ even when blank,
/// so a cleared row is never mistaken for a scrolled one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    generation: u64,
}

impl Row {
    fn new(width: usize, bg: Color) -> Self {
        Self {
            cells: vec![Cell::blank(bg); width],
            generation: next_gen(),
        }
    }

    pub fn wraps(&self) -> bool {
        self.cells.last().is_some_and(|c| c.wrap)
    }

    fn set_wrap(&mut self, on: bool) {
        if let Some(c) = self.cells.last_mut() {
            c.wrap = on;
        }
    }

    fn reset(&mut self, bg: Color) {
        self.generation = next_gen();
        for c in &mut self.cells {
            *c = Cell::blank(bg);
        }
    }
}

/// Mouse reporting and encoding modes, by their DEC mode numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mouse {
    /// 0, 9 or 1000 to 1003.
    pub reporting: u16,
    /// 0, 1005, 1006 or 1015.
    pub encoding: u16,
    pub focus: bool,
    pub alternate_scroll: bool,
}

/// What ESC 7 keeps for ESC 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Saved {
    row: usize,
    col: usize,
    rend: Rendition,
    autowrap: bool,
    origin: bool,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            row: 0,
            col: 0,
            rend: Rendition::default(),
            autowrap: true,
            origin: false,
        }
    }
}

/// The cursor and the modes that go with the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawState {
    pub width: usize,
    pub height: usize,
    pub cursor_col: usize,
    pub cursor_row: usize,
    /// The cell a combining character joins: the one printed last, or
    /// the one under the cursor once the cursor has been moved.
    glyph: Option<(usize, usize)>,
    /// The scrolling region's first and last rows.
    top: usize,
    bottom: usize,
    pub rend: Rendition,
    pub link: Link,
    saved: Saved,
    /// The last column was just written: the next character goes to the
    /// start of the next line (with autowrap on).
    wrap_pending: bool,
    origin: bool,
    autowrap: bool,
    insert: bool,
    pub cursor_visible: bool,
    pub reverse_video: bool,
    pub bracketed_paste: bool,
    pub mouse: Mouse,
    pub app_cursor_keys: bool,
}

impl DrawState {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cursor_col: 0,
            cursor_row: 0,
            glyph: Some((0, 0)),
            top: 0,
            bottom: height - 1,
            rend: Rendition::default(),
            link: None,
            saved: Saved::default(),
            wrap_pending: false,
            origin: false,
            autowrap: true,
            insert: false,
            cursor_visible: true,
            reverse_video: false,
            bracketed_paste: false,
            mouse: Mouse::default(),
            app_cursor_keys: false,
        }
    }

    /// The rows the cursor may go to: the region in origin mode, else
    /// the whole screen.
    fn reach(&self) -> (usize, usize) {
        if self.origin {
            (self.top, self.bottom)
        } else {
            (0, self.height - 1)
        }
    }

    fn in_region(&self) -> bool {
        (self.top..=self.bottom).contains(&self.cursor_row)
    }

    /// Puts the cursor at `row`, `col` (screen coordinates), kept within
    /// reach. A moved cursor no longer waits to wrap, and a combining
    /// character now joins the cell under it.
    fn place(&mut self, row: i64, col: i64) {
        let (lo, hi) = self.reach();
        self.cursor_row = row.clamp(lo as i64, hi as i64) as usize;
        self.cursor_col = col.clamp(0, self.width as i64 - 1) as usize;
        self.glyph = Some((self.cursor_row, self.cursor_col));
        self.wrap_pending = false;
    }

    fn step(&mut self, rows: i64, cols: i64) {
        self.place(self.cursor_row as i64 + rows, self.cursor_col as i64 + cols);
    }

    /// Row `n` counted from the top of reach (the region in origin mode).
    fn row_at(&mut self, n: i64) {
        let top = self.reach().0 as i64;
        self.place(top + n, self.cursor_col as i64);
    }

    fn col_at(&mut self, n: i64) {
        self.place(self.cursor_row as i64, n);
    }

    fn home(&mut self) {
        self.row_at(0);
        self.col_at(0);
    }

    /// Moves past a character of `cells` columns just written at the
    /// cursor; past the last column it waits there to wrap.
    fn advance(&mut self, cells: usize) {
        self.glyph = Some((self.cursor_row, self.cursor_col));
        let col = self.cursor_col + cells;
        self.wrap_pending = col >= self.width;
        self.cursor_col = col.min(self.width - 1);
    }
}

/// A whole screen state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub rows: Vec<Arc<Row>>,
    pub ds: DrawState,
    pub icon_name: String,
    pub title: String,
    pub title_set: bool,
    pub clipboard: String,
    pub bells: u32,
}

impl Screen {
    /// A blank screen of `width` x `height` (each at least 1).
    pub fn new(width: usize, height: usize) -> Self {
        let width = width.clamp(1, MAX_SIDE);
        let height = height.clamp(1, MAX_SIDE);
        Self {
            rows: (0..height)
                .map(|_| Arc::new(Row::new(width, Color::Default)))
                .collect(),
            ds: DrawState::new(width, height),
            icon_name: String::new(),
            title: String::new(),
            title_set: false,
            clipboard: String::new(),
            bells: 0,
        }
    }

    pub fn width(&self) -> usize {
        self.ds.width
    }

    pub fn height(&self) -> usize {
        self.ds.height
    }

    /// Whether two states look the same.
    pub fn same_as(&self, other: &Screen) -> bool {
        self == other
    }

    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        self.rows.get(row)?.cells.get(col)
    }

    fn row_mut(&mut self, row: usize) -> Option<&mut Row> {
        self.rows.get_mut(row).map(Arc::make_mut)
    }

    fn cell_mut(&mut self, row: usize, col: usize) -> Option<&mut Cell> {
        self.row_mut(row)?.cells.get_mut(col)
    }

    fn bg(&self) -> Color {
        self.ds.rend.bg
    }

    fn blank_row(&self) -> Arc<Row> {
        Arc::new(Row::new(self.ds.width, self.bg()))
    }

    /// Blanks columns `cols` of `row` in the current background.
    fn erase_cells(&mut self, row: usize, cols: std::ops::Range<usize>) {
        let bg = self.bg();
        if let Some(r) = self.row_mut(row) {
            let end = cols.end.min(r.cells.len());
            for c in r
                .cells
                .get_mut(cols.start.min(end)..end)
                .unwrap_or_default()
            {
                *c = Cell::blank(bg);
            }
        }
    }

    /// Replaces rows `rows` with new blank ones.
    fn erase_rows(&mut self, rows: std::ops::Range<usize>) {
        let bg = self.bg();
        let end = rows.end.min(self.rows.len());
        for r in self
            .rows
            .get_mut(rows.start.min(end)..end)
            .unwrap_or_default()
        {
            Arc::make_mut(r).reset(bg);
        }
    }

    /// Resizes the screen, as the server's resize instruction does.
    pub fn resize(&mut self, width: usize, height: usize) {
        let width = width.clamp(1, MAX_SIDE);
        let height = height.clamp(1, MAX_SIDE);
        let (old_w, old_h) = (self.ds.width, self.ds.height);
        if (old_w, old_h) == (width, height) {
            return;
        }
        let ds = &mut self.ds;
        ds.width = width;
        ds.height = height;
        ds.top = 0;
        ds.bottom = height - 1;
        let (lo, hi) = ds.reach();
        ds.cursor_row = ds.cursor_row.clamp(lo, hi);
        ds.cursor_col = ds.cursor_col.min(width - 1);
        ds.glyph = ds.glyph.filter(|&(r, c)| r < height && c < width);
        let bg = self.bg();
        if old_w != width {
            // Rows cut or widened no longer wrap.
            for row in self.rows.iter_mut().take(height) {
                let r = Arc::make_mut(row);
                r.set_wrap(false);
                r.cells.resize(width, Cell::blank(bg));
            }
        }
        let fresh = self.blank_row();
        self.rows.resize(height, fresh);
    }

    /// Takes `count` rows out at `at` and puts as many blank ones in at
    /// `fill`, within the scrolling region (both indexes after removal).
    fn shift_rows(&mut self, at: usize, count: usize, fill: usize) {
        let fresh = self.blank_row();
        self.rows.drain(at..at + count);
        self.rows
            .splice(fill..fill, std::iter::repeat_n(fresh, count));
    }

    /// Scrolls the region up by `n` rows (down when negative).
    fn scroll(&mut self, n: i64) {
        let (top, bottom) = (self.ds.top, self.ds.bottom);
        let count = (n.unsigned_abs() as usize).min(bottom + 1 - top);
        if count == 0 {
            return;
        }
        if n > 0 {
            self.shift_rows(top, count, bottom + 1 - count);
        } else {
            self.shift_rows(bottom + 1 - count, count, top);
        }
    }

    /// Inserts (`n` > 0) or deletes blank rows at the cursor's row, when
    /// it is in the scrolling region; the rows below move within it.
    fn insert_delete_rows(&mut self, n: i64) {
        if !self.ds.in_region() {
            return;
        }
        let (row, bottom) = (self.ds.cursor_row, self.ds.bottom);
        let count = (n.unsigned_abs() as usize).min(bottom + 1 - row);
        if n > 0 {
            self.shift_rows(bottom + 1 - count, count, row);
        } else {
            self.shift_rows(row, count, bottom + 1 - count);
        }
    }

    /// Moves the cursor a line down (or up): at the region's edge the
    /// region scrolls instead.
    fn line_feed(&mut self, down: bool) {
        let ds = &self.ds;
        let edge = if down { ds.bottom } else { ds.top };
        if ds.in_region() && ds.cursor_row == edge {
            self.scroll(if down { 1 } else { -1 });
            self.ds.step(0, 0);
        } else {
            self.ds.step(if down { 1 } else { -1 }, 0);
        }
    }

    /// Inserts (`n` > 0) or deletes blank cells at the cursor; the rest
    /// of the row moves right (and off the edge) or left.
    fn insert_delete_cells(&mut self, n: i64) {
        let (row, col, bg) = (self.ds.cursor_row, self.ds.cursor_col, self.bg());
        let Some(r) = self.row_mut(row) else {
            return;
        };
        let Some(tail) = r.cells.get_mut(col..) else {
            return;
        };
        let count = (n.unsigned_abs() as usize).min(tail.len());
        if n > 0 {
            tail.rotate_right(count);
            tail.iter_mut()
                .take(count)
                .for_each(|c| *c = Cell::blank(bg));
        } else {
            tail.rotate_left(count);
            tail.iter_mut()
                .rev()
                .take(count)
                .for_each(|c| *c = Cell::blank(bg));
        }
    }

    /// Interprets one instruction's terminal output: the server's diff
    /// from an older state to a new one, always whole sequences.
    pub fn apply(&mut self, bytes: &[u8]) {
        let mut parser = vte::Parser::new();
        parser.advance(&mut Feed(self), bytes);
    }

    fn print(&mut self, c: char) {
        let cells = if (' '..='~').contains(&c) || ('\u{a0}'..='\u{ff}').contains(&c) {
            1
        } else {
            match c.width() {
                Some(w) => w,
                None => {
                    // C1 controls, decoded from UTF-8.
                    self.control(u32::from(c));
                    return;
                }
            }
        };
        if cells == 0 {
            let target = self
                .ds
                .glyph
                .filter(|&(r, col)| self.cell(r, col).is_some_and(|cell| !cell.text.is_empty()));
            if let Some((r, col)) = target {
                if let Some(cell) = self.cell_mut(r, col) {
                    cell.push(c);
                }
                return;
            }
        }
        let w = self.ds.width;
        if self.ds.autowrap {
            if self.ds.wrap_pending {
                if let Some(r) = self.row_mut(self.ds.cursor_row) {
                    r.set_wrap(true);
                }
                self.ds.col_at(0);
                self.line_feed(true);
            } else if cells == 2 && self.ds.cursor_col == w - 1 {
                // No room for both halves: the last column stays blank.
                let row = self.ds.cursor_row;
                self.erase_cells(row, w - 1..w);
                if let Some(r) = self.row_mut(row) {
                    r.set_wrap(false);
                }
                self.ds.col_at(0);
                self.line_feed(true);
            }
        }
        let used = cells.max(1);
        if self.ds.insert {
            self.insert_delete_cells(used as i64);
        }
        let (row, col) = (self.ds.cursor_row, self.ds.cursor_col);
        let (rend, link) = (self.ds.rend, self.ds.link.clone());
        self.erase_cells(row, col..col + used);
        if let Some(cell) = self.cell_mut(row, col) {
            cell.push(c);
            cell.wide = used == 2;
            // A combining character with nothing to join stands alone.
            cell.fallback = cells == 0;
            cell.rend = rend;
            cell.link = link;
        }
        self.ds.advance(used);
    }

    /// C0 controls, and the C1 ones that move the cursor.
    fn control(&mut self, code: u32) {
        let pending = self.ds.wrap_pending;
        self.ds.wrap_pending = false;
        match code {
            0x07 => self.bells = self.bells.wrapping_add(1),
            0x08 => self.ds.step(0, -1),
            0x09 => {
                let stop = (self.ds.cursor_col / 8 + 1) * 8;
                self.ds.col_at(stop as i64);
                // A tab keeps a wrap that is due.
                self.ds.wrap_pending = pending;
            }
            0x0A..=0x0C | 0x84 => self.line_feed(true),
            0x0D => self.ds.col_at(0),
            0x85 => {
                self.ds.col_at(0);
                self.line_feed(true);
            }
            0x8D => self.line_feed(false),
            _ => {}
        }
    }

    fn escape(&mut self, intermediates: &[u8], byte: u8) {
        self.ds.wrap_pending = false;
        match (intermediates, byte) {
            (b"", b'D') => self.control(0x84),
            (b"", b'E') => self.control(0x85),
            (b"", b'M') => self.control(0x8D),
            (b"", b'7') => {
                let ds = &mut self.ds;
                ds.saved = Saved {
                    row: ds.cursor_row,
                    col: ds.cursor_col,
                    rend: ds.rend,
                    autowrap: ds.autowrap,
                    origin: ds.origin,
                };
            }
            (b"", b'8') => {
                let ds = &mut self.ds;
                let s = ds.saved;
                ds.rend = s.rend;
                ds.autowrap = s.autowrap;
                ds.origin = s.origin;
                ds.place(s.row as i64, s.col as i64);
            }
            (b"", b'c') => {
                let mut fresh = Screen::new(self.ds.width, self.ds.height);
                fresh.title = std::mem::take(&mut self.title);
                fresh.icon_name = std::mem::take(&mut self.icon_name);
                fresh.title_set = self.title_set;
                fresh.bells = self.bells;
                *self = fresh;
            }
            (b"#", b'8') => {
                // The alignment test: every cell an E.
                self.erase_rows(0..self.ds.height);
                for row in &mut self.rows {
                    for cell in &mut Arc::make_mut(row).cells {
                        cell.text.push('E');
                    }
                }
            }
            _ => {}
        }
    }

    fn csi(&mut self, params: &vte::Params, intermediates: &[u8], action: char) {
        let dec = intermediates == b"?";
        let plain = intermediates.is_empty();
        // Mode and rendition changes leave a due wrap alone.
        let leaves_wrap =
            dec && matches!(action, 'h' | 'l') || plain && matches!(action, 'm' | 'g' | 'I' | 'Z');
        if !leaves_wrap {
            self.ds.wrap_pending = false;
        }
        let args: Vec<u16> = params
            .iter()
            .map(|g| g.first().copied().unwrap_or(0))
            .collect();
        // Argument `i`; missing or 0 means `default`.
        let arg = |i: usize, default: u16| -> i64 {
            i64::from(args.get(i).copied().filter(|&v| v > 0).unwrap_or(default))
        };
        if dec {
            if matches!(action, 'h' | 'l') {
                for &mode in &args {
                    self.dec_mode(mode, action == 'h');
                }
            }
            return;
        }
        if !plain {
            return;
        }
        let n = arg(0, 1);
        let (row, col, w, h) = (
            self.ds.cursor_row,
            self.ds.cursor_col,
            self.ds.width,
            self.ds.height,
        );
        match action {
            'A' => self.ds.step(-n, 0),
            'B' => self.ds.step(n, 0),
            'C' => self.ds.step(0, n),
            'D' => self.ds.step(0, -n),
            'H' | 'f' => {
                self.ds.row_at(arg(0, 1) - 1);
                self.ds.col_at(arg(1, 1) - 1);
            }
            'G' | '`' => self.ds.col_at(n - 1),
            'd' => self.ds.row_at(n - 1),
            'J' => match arg(0, 0) {
                0 => {
                    self.erase_cells(row, col..w);
                    self.erase_rows(row + 1..h);
                }
                1 => {
                    self.erase_rows(0..row);
                    self.erase_cells(row, 0..col + 1);
                }
                2 => self.erase_rows(0..h),
                _ => {}
            },
            'K' => match arg(0, 0) {
                0 => self.erase_cells(row, col..w),
                1 => self.erase_cells(row, 0..col + 1),
                2 => self.erase_rows(row..row + 1),
                _ => {}
            },
            'X' => self.erase_cells(row, col..col.saturating_add(n as usize)),
            '@' => self.insert_delete_cells(n),
            'P' => self.insert_delete_cells(-n),
            'L' | 'M' => {
                self.insert_delete_rows(if action == 'L' { n } else { -n });
                self.ds.col_at(0);
            }
            'S' => self.scroll(n),
            'T' => self.scroll(-n),
            'r' => {
                let (top, bottom) = (arg(0, 1) as usize, arg(1, h as u16) as usize);
                if top < bottom && top <= h {
                    self.ds.top = top - 1;
                    self.ds.bottom = (bottom - 1).min(h - 1);
                    self.ds.home();
                }
            }
            'm' => self.sgr(params),
            'h' | 'l' if args.contains(&4) => self.ds.insert = action == 'h',
            _ => {}
        }
    }

    fn dec_mode(&mut self, mode: u16, on: bool) {
        let ds = &mut self.ds;
        match mode {
            1 => ds.app_cursor_keys = on,
            3 => {
                // Column mode: only its clearing and homing.
                ds.home();
                self.erase_rows(0..self.ds.height);
            }
            5 => ds.reverse_video = on,
            6 => {
                ds.origin = on;
                ds.home();
            }
            7 => ds.autowrap = on,
            25 => ds.cursor_visible = on,
            9 | 1000..=1003 => ds.mouse.reporting = if on { mode } else { 0 },
            1004 => ds.mouse.focus = on,
            1005 | 1006 | 1015 => ds.mouse.encoding = if on { mode } else { 0 },
            1007 => ds.mouse.alternate_scroll = on,
            2004 => ds.bracketed_paste = on,
            _ => {}
        }
    }

    fn sgr(&mut self, params: &vte::Params) {
        let groups: Vec<&[u16]> = params.iter().collect();
        if groups.is_empty() {
            self.ds.rend = Rendition::default();
            return;
        }
        let mut i = 0;
        while let Some(group) = groups.get(i) {
            let code = group.first().copied().unwrap_or(0);
            i += 1;
            if !matches!(code, 38 | 48) {
                self.ds.rend.set(u32::from(code));
                continue;
            }
            // An extended colour: 38:5:n or 38:2:[space:]r:g:b in one
            // group, or the same split by semicolons.
            let rest: Vec<u16> = if group.len() > 1 {
                match group {
                    [_, 2, _, r, g, b] => vec![2, *r, *g, *b],
                    _ => group.get(1..).unwrap_or_default().to_vec(),
                }
            } else {
                let take = match groups.get(i).and_then(|g| g.first()) {
                    Some(5) => 2,
                    Some(2) => 4,
                    _ => 0,
                };
                if groups.len() - i < take {
                    continue;
                }
                let rest = groups
                    .get(i..i + take)
                    .unwrap_or_default()
                    .iter()
                    .map(|g| g.first().copied().unwrap_or(0))
                    .collect();
                i += take;
                rest
            };
            let color = match rest.as_slice() {
                [5, n] if *n <= 255 => Color::Indexed(*n as u8),
                [2, r, g, b] => {
                    let c = |v: u16| v.min(255) as u8;
                    Color::Rgb(c(*r), c(*g), c(*b))
                }
                _ => continue,
            };
            if code == 38 {
                self.ds.rend.fg = color;
            } else {
                self.ds.rend.bg = color;
            }
        }
    }

    fn osc(&mut self, params: &[&[u8]]) {
        let text = |from: usize| -> String {
            let parts: Vec<String> = params
                .get(from..)
                .unwrap_or_default()
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            parts.join(";")
        };
        match params.first().copied().unwrap_or_default() {
            b"52" if params.get(1).copied() == Some(b"c".as_slice()) => {
                let mut data = text(2);
                let mut end = data.len().min(OSC_MAX);
                while !data.is_char_boundary(end) {
                    end -= 1;
                }
                data.truncate(end);
                self.clipboard = data;
            }
            b"8" if params.len() >= 3 => {
                let url = text(2);
                self.ds.link = (!url.is_empty()).then(|| {
                    Arc::new((
                        String::from_utf8_lossy(params.get(1).copied().unwrap_or_default())
                            .into_owned(),
                        url,
                    ))
                });
            }
            which @ (b"" | b"0" | b"1" | b"2") if params.len() >= 2 => {
                self.title_set = true;
                // At most TITLE characters of the whole string count,
                // its number and semicolon included.
                let title: String = text(1)
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(TITLE.saturating_sub(which.len() + 1))
                    .collect();
                if which != b"2" {
                    self.icon_name = title.clone();
                }
                if which != b"1" {
                    self.title = title;
                }
            }
            _ => {}
        }
    }
}

/// Feeds the parser's events to a screen.
struct Feed<'a>(&'a mut Screen);

impl vte::Perform for Feed<'_> {
    fn print(&mut self, c: char) {
        self.0.print(c);
    }

    fn execute(&mut self, byte: u8) {
        self.0.control(u32::from(byte));
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, c: char) {
        if ignore {
            self.0.ds.wrap_pending = false;
        } else {
            self.0.csi(params, intermediates, c);
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if !ignore {
            self.0.escape(intermediates, byte);
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.0.osc(params);
    }
}

/// The pane's terminal while a frame is written to it: where its cursor
/// is (`None` after writing the last column, where terminals disagree)
/// and what it draws with.
struct Pen {
    out: String,
    at: Option<(usize, usize)>,
    rend: Rendition,
    link: Link,
    cursor_shown: bool,
}

impl Pen {
    /// Puts the cursor at row `y`, column `x`, unless it is there.
    fn go(&mut self, y: usize, x: usize) {
        if self.at != Some((y, x)) {
            self.out.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
            self.at = Some((y, x));
        }
    }

    fn style(&mut self, rend: Rendition, link: &Link) {
        if self.rend != rend {
            self.out.push_str(&rend.sgr());
            self.rend = rend;
        }
        if self.link != *link {
            self.out.push_str(&osc8(link));
            self.link = link.clone();
        }
    }

    /// Hides the cursor while cells change under it.
    fn hide_cursor(&mut self) {
        if self.cursor_shown {
            self.out.push_str("\x1b[?25l");
            self.cursor_shown = false;
        }
    }

    /// Switches DEC private mode `mode` on or off.
    fn mode(&mut self, mode: u16, on: bool) {
        self.out
            .push_str(&format!("\x1b[?{mode}{}", if on { 'h' } else { 'l' }));
    }
}

/// Whether `c` is a blank that erasing leaves: no text, no link, and only
/// a background colour.
fn erased(c: &Cell) -> bool {
    c.text.is_empty() && c.link.is_none() && c.rend == Rendition::blank(c.rend.bg)
}

/// Shifted rows that `next` shares with `last`: how far the screen moved
/// up (0 when it did not) so that most of the rows need no redraw.
fn scrolled_by(last: &[Arc<Row>], next: &[Arc<Row>]) -> usize {
    let h = next.len().min(last.len());
    let same = |k: usize| -> usize {
        (0..h - k)
            .filter(|&i| match (next.get(i), last.get(i + k)) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
                _ => false,
            })
            .count()
    };
    let unmoved = same(0);
    let mut best = (0, unmoved);
    for k in 1..h {
        let kept = same(k);
        // Worth it only when the moved rows outnumber the unmoved ones
        // and make up half of what stays on screen.
        if kept > best.1 && kept * 2 >= h - k {
            best = (k, kept);
        }
    }
    best.0
}

/// Writes the output that turns a terminal showing `last` into one
/// showing `next`. Without `initialized`, the terminal's contents are
/// unknown and everything is drawn. When the screen scrolled, the
/// terminal is scrolled too (so the lines go to its scrollback) and only
/// what then differs is drawn.
pub fn render(initialized: bool, last: &Screen, next: &Screen) -> String {
    let (w, h) = (next.width(), next.height());
    let full = !initialized || w != last.width() || h != last.height();
    let mut pen = Pen {
        out: String::new(),
        at: None,
        rend: Rendition::default(),
        link: None,
        cursor_shown: last.ds.cursor_visible,
    };

    // Things outside the cells.
    if next.bells != last.bells {
        pen.out.push('\x07');
    }
    let printable = |s: &str| -> String { s.chars().filter(|c| !c.is_control()).collect() };
    if next.title_set && (full || next.title != last.title || next.icon_name != last.icon_name) {
        if next.title == next.icon_name {
            pen.out
                .push_str(&format!("\x1b]0;{}\x07", printable(&next.title)));
        } else {
            pen.out
                .push_str(&format!("\x1b]1;{}\x07", printable(&next.icon_name)));
            pen.out
                .push_str(&format!("\x1b]2;{}\x07", printable(&next.title)));
        }
    }
    if next.clipboard != last.clipboard {
        let base64: String = next
            .clipboard
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            .collect();
        pen.out.push_str(&format!("\x1b]52;c;{base64}\x07"));
    }
    if full || next.ds.reverse_video != last.ds.reverse_video {
        pen.mode(5, next.ds.reverse_video);
    }

    // What the terminal shows now, row by row.
    let mut shown: Vec<Arc<Row>>;
    if full {
        pen.hide_cursor();
        pen.out.push_str("\x1b[r\x1b[0m\x1b]8;;\x1b\\\x1b[H\x1b[2J");
        pen.at = Some((0, 0));
        let blank = Arc::new(Row::new(w, Color::Default));
        shown = vec![blank; h];
    } else {
        if !last.ds.wrap_pending {
            pen.at = Some((last.ds.cursor_row, last.ds.cursor_col));
        }
        pen.rend = last.ds.rend;
        pen.link = last.ds.link.clone();
        shown = last.rows.clone();
        let k = scrolled_by(&shown, &next.rows);
        if k > 0 {
            // A line feed on the bottom row scrolls the whole screen up.
            pen.hide_cursor();
            pen.style(Rendition::default(), &None);
            pen.go(h - 1, 0);
            for _ in 0..k {
                pen.out.push('\n');
            }
            shown.drain(..k);
            shown.extend((0..k).map(|_| Arc::new(Row::new(w, Color::Default))));
        }
    }

    for (y, (row, old)) in next.rows.iter().zip(shown.iter_mut()).enumerate() {
        if Arc::ptr_eq(row, old) || row.cells == old.cells {
            continue;
        }
        pen.hide_cursor();
        draw_row(&mut pen, y, w, &row.cells, &mut Arc::make_mut(old).cells);
    }

    // The cursor and modes.
    let (cy, cx) = (next.ds.cursor_row, next.ds.cursor_col);
    pen.go(cy.min(h.saturating_sub(1)), cx.min(w.saturating_sub(1)));
    if full {
        pen.out.push_str("\x1b[0m\x1b]8;;\x1b\\");
        pen.rend = Rendition::default();
        pen.link = None;
    }
    pen.style(next.ds.rend, &next.ds.link);
    if pen.cursor_shown != next.ds.cursor_visible || full {
        pen.mode(25, next.ds.cursor_visible);
    }
    let (now, was) = (&next.ds, &last.ds);
    if full || now.bracketed_paste != was.bracketed_paste {
        pen.mode(2004, now.bracketed_paste);
    }
    if full || now.mouse.reporting != was.mouse.reporting {
        // Switch every reporting mode off, then the one in use on.
        for m in [9, 1000, 1001, 1002, 1003] {
            pen.mode(m, false);
        }
        if now.mouse.reporting != 0 {
            pen.mode(now.mouse.reporting, true);
        }
    }
    if full || now.mouse.focus != was.mouse.focus {
        pen.mode(1004, now.mouse.focus);
    }
    if full || now.mouse.encoding != was.mouse.encoding {
        for m in [1005, 1006, 1015] {
            pen.mode(m, false);
        }
        if now.mouse.encoding != 0 {
            pen.mode(now.mouse.encoding, true);
        }
    }
    pen.out
}

/// Draws the cells of row `y` that differ between `want` and `have`,
/// and brings `have` up to date.
fn draw_row(pen: &mut Pen, y: usize, w: usize, want: &[Cell], have: &mut [Cell]) {
    // Where the row's trailing erased blanks of one colour begin: one
    // erase-to-end draws them.
    let mut tail = want.len().min(w);
    if let Some(last) = want.get(tail.wrapping_sub(1)).filter(|c| erased(c)) {
        while tail > 0
            && want
                .get(tail - 1)
                .is_some_and(|c| erased(c) && c.rend == last.rend)
        {
            tail -= 1;
        }
        // The right half of a wide character is blank in the model; an
        // erase from there would cut the character in two.
        if tail > 0 && want.get(tail - 1).is_some_and(|c| c.wide) {
            tail += 1;
        }
    }
    let mut x = 0;
    while x < tail {
        let Some(cell) = want.get(x) else {
            break;
        };
        let width = if cell.wide { 2 } else { 1 };
        if have.get(x) == Some(cell) && (width == 1 || have.get(x + 1) == want.get(x + 1)) {
            x += width;
            continue;
        }
        // A run of erased blanks: erase them without moving.
        let run = want
            .get(x..tail)
            .unwrap_or_default()
            .iter()
            .take_while(|c| erased(c) && c.rend == cell.rend)
            .count();
        if run >= 4 {
            pen.go(y, x);
            pen.style(cell.rend, &None);
            pen.out.push_str(&format!("\x1b[{run}X"));
            for (h, c) in have.iter_mut().zip(want).skip(x).take(run) {
                *h = c.clone();
            }
            x += run;
            continue;
        }
        pen.go(y, x);
        pen.style(cell.rend, &cell.link);
        if cell.wide && x + 2 > w {
            // No room for the right half (left by a resize): a space,
            // which cannot wrap the terminal.
            pen.out.push(' ');
        } else {
            cell.grapheme(&mut pen.out);
        }
        let drawn = if cell.wide && x + 2 <= w { 2 } else { 1 };
        for i in x..x + drawn {
            if let (Some(h), Some(c)) = (have.get_mut(i), want.get(i)) {
                *h = c.clone();
            }
        }
        x += drawn;
        pen.at = (x < w).then_some((y, x));
    }
    if have.get(tail..) != want.get(tail..) {
        if let Some(c) = want.get(tail) {
            pen.go(y, tail);
            pen.style(c.rend, &None);
            pen.out.push_str("\x1b[K");
        }
        for (h, c) in have.iter_mut().zip(want).skip(tail) {
            *h = c.clone();
        }
    }
}

/// How a terminal showing `last`, after the output of `render(init,
/// last, next)`, differs from `next`, if it does: for tests and the
/// fuzzer. The first cell of a row that starts with a combining
/// character is drawn on a no-break space, so those compare without it.
#[doc(hidden)]
pub fn render_mismatch(init: bool, last: &Screen, next: &Screen) -> Option<String> {
    let out = render(init, last, next);
    // The pane is already the new size (the server follows the pane).
    let mut shown = last.clone();
    shown.resize(next.width(), next.height());
    // What the server's programs set but Mosh never passes on: the pane
    // has none of it (each frame leaves the scrolling region reset and
    // the cursor placed).
    let ds = &mut shown.ds;
    ds.top = 0;
    ds.bottom = ds.height - 1;
    ds.origin = false;
    ds.insert = false;
    ds.autowrap = true;
    ds.wrap_pending = false;
    shown.apply(out.as_bytes());
    let norm = |c: &Cell| -> String {
        // Blank, a space and a no-break space look the same.
        if c.text.is_empty() || c.text == " " || c.text == "\u{a0}" {
            return String::new();
        }
        if c.fallback {
            c.text.clone()
        } else {
            c.text
                .strip_prefix('\u{a0}')
                .filter(|t| !t.is_empty())
                .unwrap_or(&c.text)
                .to_owned()
        }
    };
    for y in 0..next.height() {
        for x in 0..next.width() {
            let (Some(a), Some(b)) = (shown.cell(y, x), next.cell(y, x)) else {
                return Some(format!("cell {y},{x} missing"));
            };
            // The right half of a wide character shows that character,
            // whatever a program later wrote there; Mosh draws it so too.
            if x > 0 && next.cell(y, x - 1).is_some_and(|c| c.wide) {
                continue;
            }
            if b.wide && x + 2 > next.width() {
                continue;
            }
            if norm(a) != norm(b) || a.rend != b.rend || a.link != b.link || a.wide != b.wide {
                return Some(format!(
                    "cell {y},{x}: {a:?} != {b:?}; output {out:?}; last ds {:?}",
                    last.ds
                ));
            }
        }
    }
    let (a, b) = (&shown.ds, &next.ds);
    if (a.cursor_row, a.cursor_col) != (b.cursor_row, b.cursor_col) {
        return Some(format!(
            "cursor {:?} != {:?} (size {}x{}, last {}x{}); output {out:?}",
            (a.cursor_row, a.cursor_col),
            (b.cursor_row, b.cursor_col),
            next.width(),
            next.height(),
            last.width(),
            last.height()
        ));
    }
    if a.rend != b.rend
        || a.link != b.link
        || a.cursor_visible != b.cursor_visible
        || a.reverse_video != b.reverse_video
        || a.bracketed_paste != b.bracketed_paste
        || a.mouse.reporting != b.mouse.reporting
        || a.mouse.encoding != b.mouse.encoding
        || a.mouse.focus != b.mouse.focus
    {
        return Some(format!("modes {a:?} != {b:?}"));
    }
    if next.title_set && (shown.title != next.title || shown.icon_name != next.icon_name) {
        return Some(format!("title {:?} != {:?}", shown.title, next.title));
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn text(s: &Screen, row: usize) -> String {
        s.rows[row]
            .cells
            .iter()
            .map(|c| {
                if c.text.is_empty() {
                    " ".into()
                } else {
                    c.text.clone()
                }
            })
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// Rendering `last` to `next`, applied to a copy of `last`, gives
    /// `next`: what keeps the pane equal to the server's state.
    fn round_trip(last: &Screen, next: &Screen) {
        for init in [true, false] {
            let out = render(init, last, next);
            let mut shown = last.clone();
            shown.apply(out.as_bytes());
            for y in 0..next.height() {
                assert_eq!(
                    text(&shown, y),
                    text(next, y),
                    "row {y} init {init}: {out:?}"
                );
                for x in 0..next.width() {
                    let (a, b) = (&shown.rows[y].cells[x], &next.rows[y].cells[x]);
                    assert_eq!(a.text, b.text, "{y},{x}");
                    assert_eq!(a.rend, b.rend, "{y},{x}");
                    assert_eq!(a.link, b.link, "{y},{x}");
                }
            }
            assert_eq!(
                (shown.ds.cursor_row, shown.ds.cursor_col),
                (next.ds.cursor_row, next.ds.cursor_col)
            );
            assert_eq!(shown.ds.rend, next.ds.rend);
            assert_eq!(shown.ds.cursor_visible, next.ds.cursor_visible);
            assert_eq!(shown.ds.mouse, next.ds.mouse);
            assert_eq!(shown.ds.bracketed_paste, next.ds.bracketed_paste);
            assert_eq!(shown.title, next.title);
        }
    }

    #[test]
    fn prints_wraps_and_scrolls() {
        let mut s = Screen::new(5, 3);
        s.apply(b"hello world\r\nabc\r\nxyz");
        // "hello" wrapped into " worl", "d", then two lines scrolled off.
        assert_eq!(text(&s, 0), "d");
        assert_eq!(text(&s, 1), "abc");
        assert_eq!(text(&s, 2), "xyz");
        assert_eq!((s.ds.cursor_row, s.ds.cursor_col), (2, 3));
    }

    #[test]
    fn renditions_and_modes() {
        let mut s = Screen::new(10, 2);
        s.apply(b"\x1b[1;31;48;5;200mA\x1b[0;38;2;1;2;3mB\x1b[?2004h\x1b[?1002h\x1b[?25l");
        assert_eq!(s.rows[0].cells[0].rend.attrs, BOLD);
        assert_eq!(s.rows[0].cells[0].rend.fg, Color::Indexed(1));
        assert_eq!(s.rows[0].cells[0].rend.bg, Color::Indexed(200));
        assert_eq!(s.rows[0].cells[1].rend.fg, Color::Rgb(1, 2, 3));
        assert!(s.ds.bracketed_paste && !s.ds.cursor_visible);
        assert_eq!(s.ds.mouse.reporting, 1002);
        assert_eq!(s.rows[0].cells[0].rend.sgr(), "\x1b[0;1;31;48;5;200m");
    }

    #[test]
    fn wide_and_combining_characters() {
        let mut s = Screen::new(4, 2);
        s.apply("漢e\u{301}x漢".as_bytes());
        assert_eq!(s.rows[0].cells[0].text, "漢");
        assert!(s.rows[0].cells[0].wide);
        assert_eq!(s.rows[0].cells[2].text, "e\u{301}");
        assert_eq!(s.rows[0].cells[3].text, "x");
        assert_eq!(s.rows[1].cells[0].text, "漢");
    }

    #[test]
    fn titles_and_links() {
        let mut s = Screen::new(10, 2);
        s.apply(b"\x1b]0;hi there\x07\x1b]8;id=1;http://x\x1b\\L\x1b]8;;\x1b\\M");
        assert_eq!(
            (s.title.as_str(), s.icon_name.as_str()),
            ("hi there", "hi there")
        );
        assert_eq!(
            s.rows[0].cells[0].link.as_deref(),
            Some(&("id=1".to_owned(), "http://x".to_owned()))
        );
        assert_eq!(s.rows[0].cells[1].link, None);
    }

    #[test]
    fn renders_changes_and_scrolls() {
        let mut a = Screen::new(20, 6);
        a.apply(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
        let mut b = a.clone();
        b.apply(b"\r\nseven\r\n\x1b[1;32meight\x1b[0m");
        round_trip(&a, &b);
        let out = render(true, &a, &b);
        assert!(out.starts_with("\r\n\n") || out.contains('\n'), "{out:?}");
        let mut c = b.clone();
        c.apply(b"\x1b[3;5Hmid\x1b[K\x1b]2;title\x07\x1b[?1006h\x1b[?1000h");
        round_trip(&b, &c);
        let mut d = c.clone();
        d.apply(b"\x1b[2;4r\x1b[4;1H\nnew\x1b[r\x1b[H\x1b[2Jfresh");
        round_trip(&c, &d);
        let mut e = d.clone();
        e.apply("\x1b[6;18Hwrap漢字 here\x1b]8;;http://a\x1b\\link\x1b]8;;\x1b\\".as_bytes());
        round_trip(&d, &e);
        round_trip(&Screen::new(20, 6), &e);
    }

    #[test]
    fn resizes_keep_contents() {
        let mut s = Screen::new(4, 2);
        s.apply(b"abcd");
        s.resize(6, 3);
        assert_eq!(text(&s, 0), "abcd");
        assert_eq!(s.rows.len(), 3);
        assert!(s.rows.iter().all(|r| r.cells.len() == 6));
        s.resize(2, 1);
        assert_eq!(text(&s, 0), "ab");
        let t = Screen::new(3, 3);
        round_trip(&t, &s);
    }

    /// Random output of the kinds mosh-server's renderer and ordinary
    /// programs write: rendering any state to any later one, applied to
    /// the first, gives the later one.
    #[test]
    fn trailing_blanks_leave_a_wide_character_whole() {
        let mut last = Screen::new(6, 1);
        last.apply(b"zzzzzz\r");
        let mut next = last.clone();
        next.apply("ab漢\x1b[K".as_bytes());
        let out = render(true, &last, &next);
        // The erase of the trailing blanks starts after the right half.
        assert!(out.contains("\x1b[K"), "{out:?}");
        assert!(!out.contains("\x1b[1;4H"), "{out:?}");
        round_trip(&last, &next);
    }

    #[test]
    fn x10_mouse_reporting_is_switched_off() {
        let mut last = Screen::new(4, 1);
        last.apply(b"\x1b[?9h");
        let mut next = last.clone();
        next.apply(b"\x1b[?9l");
        assert!(render(true, &last, &next).contains("\x1b[?9l"));
    }

    #[test]
    fn scrolling_output_scrolls_the_terminal() {
        let mut a = Screen::new(20, 6);
        for i in 0..6 {
            a.apply(format!("line {i}\r\n").as_bytes());
        }
        let mut b = a.clone();
        b.apply(b"line 6\r\nline 7\r\n");
        let out = render(true, &a, &b);
        assert!(out.contains("\n\n"), "{out:?}");
        // Only the new lines are drawn, not the ones that moved up.
        assert!(!out.contains("line 3"), "{out:?}");
        assert_eq!(render_mismatch(true, &a, &b), None);
    }

    #[test]
    fn rendering_reproduces_random_screens() {
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut rnd = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        for round in 0..std::env::var("MOSH_ROUNDS")
            .ok()
            .and_then(|r| r.parse().ok())
            .unwrap_or(400)
        {
            let (w, h) = (1 + rnd(30) as usize, 1 + rnd(12) as usize);
            let mut a = Screen::new(w, h);
            let mut states = vec![a.clone()];
            for _ in 0..6 {
                let mut out = String::new();
                for _ in 0..rnd(40) {
                    match rnd(24) {
                        0..=7 => {
                            for _ in 0..rnd(12) {
                                out.push(char::from(b' ' + rnd(95) as u8));
                            }
                        }
                        8 => out.push_str(["漢", "字", "😀", "é"][rnd(4) as usize]),
                        9 => out.push_str(["\u{301}", "\u{308}"][rnd(2) as usize]),
                        10 => out.push_str("\r\n"),
                        11 => out.push(['\r', '\n', '\x08', '\t', '\x07'][rnd(5) as usize]),
                        12 => out.push_str(&format!(
                            "\x1b[{};{}H",
                            rnd(h as u64 + 2),
                            rnd(w as u64 + 2)
                        )),
                        13 => out.push_str(&format!(
                            "\x1b[{}{}",
                            rnd(3),
                            ["J", "K", "X", "@", "P", "L", "M"][rnd(7) as usize]
                        )),
                        14 => out.push_str(&format!(
                            "\x1b[{};{};{}m",
                            rnd(10),
                            [31, 42, 93, 104, 39, 49, 22, 27][rnd(8) as usize],
                            rnd(2)
                        )),
                        15 => out.push_str(&format!(
                            "\x1b[38;5;{}m\x1b[48;2;{};2;3m",
                            rnd(256),
                            rnd(256)
                        )),
                        16 => out.push_str(&format!(
                            "\x1b[{};{}r",
                            1 + rnd(h as u64),
                            1 + rnd(h as u64)
                        )),
                        17 => out.push_str("\x1b[r"),
                        18 => out.push_str(
                            [
                                "\x1b[?25l",
                                "\x1b[?25h",
                                "\x1b[?2004h",
                                "\x1b[?1000h",
                                "\x1b[?1006h",
                                "\x1b[?1004h",
                                "\x1b[?5h",
                                "\x1b[?1002l",
                            ][rnd(8) as usize],
                        ),
                        19 => out.push_str(&format!("\x1b]{};t{}\x07", rnd(3), rnd(9))),
                        20 => out.push_str(
                            ["\x1b]8;;http://x\x1b\\", "\x1b]8;;\x1b\\"][rnd(2) as usize],
                        ),
                        21 => out.push_str(
                            ["\x1b7", "\x1b8", "\x1bM", "\x1bD", "\x1bE"][rnd(5) as usize],
                        ),
                        22 => out.push_str(&format!(
                            "\x1b[{}{}",
                            rnd(4),
                            ["S", "T", "A", "B", "C", "D"][rnd(6) as usize]
                        )),
                        _ => {
                            let (nw, nh) = (1 + rnd(30) as usize, 1 + rnd(12) as usize);
                            a.resize(nw, nh);
                        }
                    }
                }
                a.apply(out.as_bytes());
                states.push(a.clone());
            }
            for (i, last) in states.iter().enumerate() {
                for next in &states[i..] {
                    for init in [true, false] {
                        if let Some(why) = render_mismatch(init, last, next) {
                            panic!("round {round} init {init}: {why}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hostile_input_stays_bounded() {
        let mut s = Screen::new(3, 2);
        s.apply(b"\x1b[99999999999;99999999999H\x1b[99999@\x1b[99999P\x1b[99999L\x1b[99999M");
        s.apply(b"\x1b[99999X\x1b[0;0r\x1b[5;1r\x1b[2;99r\x1b[?6h\x1b[99B\x1b[S\x1b[T");
        s.apply(&[
            0xF0, 0x9F, b'x', 0xC3, 0x28, 0xFF, 0x9B, b'2', b'J', 0x9D, b'0', b';', 0x07,
        ]);
        let mut long = b"\x1b]0;".to_vec();
        long.extend(vec![b'a'; 100_000]);
        long.push(0x07);
        s.apply(&long);
        assert!(s.title.chars().count() <= 256);
        assert_eq!(s.rows.len(), 2);
        assert!(s.rows.iter().all(|r| r.cells.len() == 3));
    }
}
