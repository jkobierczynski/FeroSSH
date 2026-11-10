//! The terminal state Mosh synchronises: a screen of cells and the modes
//! that show, as `mosh-server`'s emulator keeps them.
//!
//! The server sends each new state as terminal output that turns an older
//! state's screen into the new one. That older state need not be the one
//! on show here (the server may not have heard our acknowledgement yet),
//! so the worker keeps the screens of the states the server may still
//! refer to and applies each diff to the right one: [`Screen::apply`]
//! interprets the output the way Mosh's emulator does. [`Screen::render`]
//! then writes the output that turns the pane's terminal from what it
//! shows into a new screen, as Mosh's client does for a real terminal.
//!
//! The emulator follows Mosh's for everything the server's renderer
//! writes (text, cursor moves, erasing, scrolling regions, renditions,
//! modes, titles, hyperlinks) and the usual VT100 controls besides. Input
//! is hostile: sizes are capped, and nothing panics or grows unbounded.

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
/// Parameters kept of a control sequence.
const PARAMS: usize = 16;

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
/// as in Mosh, so a cleared row is never mistaken for a scrolled one.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Saved {
    col: usize,
    row: usize,
    rend: Rendition,
    autowrap: bool,
    origin: bool,
}

/// The cursor and the modes that go with the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawState {
    pub width: usize,
    pub height: usize,
    pub cursor_col: usize,
    pub cursor_row: usize,
    combining: Option<(usize, usize)>,
    top: usize,
    bottom: usize,
    pub rend: Rendition,
    pub link: Link,
    saved: Saved,
    next_print_wraps: bool,
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
            combining: Some((0, 0)),
            top: 0,
            bottom: height - 1,
            rend: Rendition::default(),
            link: None,
            saved: Saved {
                autowrap: true,
                ..Saved::default()
            },
            next_print_wraps: false,
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

    fn limit_top(&self) -> usize {
        if self.origin { self.top } else { 0 }
    }

    fn limit_bottom(&self) -> usize {
        if self.origin {
            self.bottom
        } else {
            self.height - 1
        }
    }

    fn snap(&mut self, row: i64, col: i64) {
        let row = row.clamp(self.limit_top() as i64, self.limit_bottom() as i64);
        self.cursor_row = row as usize;
        self.cursor_col = col.clamp(0, self.width as i64 - 1) as usize;
    }

    fn new_grapheme(&mut self) {
        self.combining = Some((self.cursor_row, self.cursor_col));
    }

    fn move_row(&mut self, n: i64, relative: bool) {
        let row = if relative {
            self.cursor_row as i64 + n
        } else {
            n + self.limit_top() as i64
        };
        self.snap(row, self.cursor_col as i64);
        self.new_grapheme();
        self.next_print_wraps = false;
    }

    fn move_col(&mut self, n: i64, relative: bool, implicit: bool) {
        if implicit {
            self.new_grapheme();
        }
        let col = if relative {
            self.cursor_col as i64 + n
        } else {
            n
        };
        if implicit {
            self.next_print_wraps = col >= self.width as i64;
        }
        self.snap(self.cursor_row as i64, col);
        if !implicit {
            self.new_grapheme();
            self.next_print_wraps = false;
        }
    }

    fn set_region(&mut self, top: usize, bottom: usize) {
        self.top = top.min(self.height - 1);
        self.bottom = bottom.min(self.height - 1).max(self.top);
        if self.origin {
            self.snap(self.cursor_row as i64, self.cursor_col as i64);
            self.new_grapheme();
        }
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
    parser: Parser,
}

/// Where the escape-sequence parser is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    CsiIgnore,
    Osc,
    OscEscape,
    /// A DCS, SOS, PM or APC string: skipped up to its end.
    Skip,
    SkipEscape,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Parser {
    state: State,
    /// UTF-8 bytes of a character still to come.
    utf8: Vec<u8>,
    params: Vec<Option<u32>>,
    current: Option<u32>,
    collect: Vec<u8>,
    osc: String,
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
            parser: Parser::default(),
        }
    }

    pub fn width(&self) -> usize {
        self.ds.width
    }

    pub fn height(&self) -> usize {
        self.ds.height
    }

    /// Whether two states look the same (the parser's state aside).
    pub fn same_as(&self, other: &Screen) -> bool {
        self.rows == other.rows
            && self.ds == other.ds
            && self.title == other.title
            && self.icon_name == other.icon_name
            && self.clipboard == other.clipboard
            && self.bells == other.bells
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

    fn new_row(&self) -> Arc<Row> {
        Arc::new(Row::new(self.ds.width, self.bg()))
    }

    fn reset_cell(&mut self, row: usize, col: usize) {
        let bg = self.bg();
        if let Some(c) = self.cell_mut(row, col) {
            *c = Cell::blank(bg);
        }
    }

    fn reset_row(&mut self, row: usize) {
        let bg = self.bg();
        if let Some(r) = self.row_mut(row) {
            r.reset(bg);
        }
    }

    /// Resizes the screen, as the server's resize instruction does.
    pub fn resize(&mut self, width: usize, height: usize) {
        let width = width.clamp(1, MAX_SIDE);
        let height = height.clamp(1, MAX_SIDE);
        let (old_w, old_h) = (self.ds.width, self.ds.height);
        if (old_w, old_h) != (width, height) {
            self.ds.top = 0;
            self.ds.bottom = height - 1;
        }
        self.ds.width = width;
        self.ds.height = height;
        self.ds.snap(self.ds.cursor_row as i64, self.ds.cursor_col as i64);
        if let Some((r, c)) = self.ds.combining
            && (c >= width || r >= height)
        {
            self.ds.combining = None;
        }
        let blank = self.new_row();
        if old_h != height {
            self.rows.resize(height, blank.clone());
        }
        if old_w == width {
            return;
        }
        let bg = self.bg();
        for row in &mut self.rows {
            if Arc::ptr_eq(row, &blank) {
                break;
            }
            let r = Arc::make_mut(row);
            r.set_wrap(false);
            r.cells.resize(width, Cell::blank(bg));
        }
        for row in &mut self.rows {
            if row.cells.len() != width {
                Arc::make_mut(row).cells.resize(width, Cell::blank(bg));
            }
        }
    }

    fn scroll(&mut self, n: i64) {
        if n >= 0 {
            self.delete_lines(self.ds.top, n as usize);
        } else {
            self.insert_lines(self.ds.top, n.unsigned_abs() as usize);
        }
    }

    fn insert_lines(&mut self, before: usize, count: usize) {
        if before < self.ds.top || before > self.ds.bottom + 1 {
            return;
        }
        let n = count.min(self.ds.bottom + 1 - before);
        if n == 0 {
            return;
        }
        let end = self.ds.bottom + 1;
        self.rows.drain(end - n..end);
        let fresh = self.new_row();
        for _ in 0..n {
            self.rows.insert(before, fresh.clone());
        }
    }

    fn delete_lines(&mut self, row: usize, count: usize) {
        if row < self.ds.top || row > self.ds.bottom {
            return;
        }
        let n = count.min(self.ds.bottom + 1 - row);
        if n == 0 {
            return;
        }
        self.rows.drain(row..row + n);
        let fresh = self.new_row();
        let at = self.ds.bottom + 1 - n;
        for _ in 0..n {
            self.rows.insert(at, fresh.clone());
        }
    }

    fn move_rows_autoscroll(&mut self, rows: i64) {
        let row = self.ds.cursor_row as i64;
        let (top, bottom) = (self.ds.top as i64, self.ds.bottom as i64);
        if row < top || row > bottom {
            self.ds.move_row(rows, true);
            return;
        }
        if row + rows > bottom {
            let n = row + rows - bottom;
            self.scroll(n);
            self.ds.move_row(-n, true);
        } else if row + rows < top {
            let n = row + rows - top;
            self.scroll(n);
            self.ds.move_row(-n, true);
        }
        self.ds.move_row(rows, true);
    }

    /// Interprets terminal output, as the server's diffs hold.
    pub fn apply(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    fn byte(&mut self, b: u8) {
        // Mosh decodes UTF-8 before parsing; C1 controls arrive only as
        // two-byte characters.
        if !self.parser.utf8.is_empty() || b >= 0x80 {
            if b & 0xC0 == 0x80 && !self.parser.utf8.is_empty() {
                self.parser.utf8.push(b);
            } else {
                if !self.parser.utf8.is_empty() {
                    // An unfinished character is dropped.
                    self.parser.utf8.clear();
                }
                if (0xC2..=0xF4).contains(&b) {
                    self.parser.utf8.push(b);
                    return;
                }
                if b >= 0x80 {
                    return;
                }
            }
            let need = match self.parser.utf8.first() {
                Some(0xC2..=0xDF) => 2,
                Some(0xE0..=0xEF) => 3,
                _ => 4,
            };
            if self.parser.utf8.len() < need {
                return;
            }
            let ch = std::str::from_utf8(&self.parser.utf8)
                .ok()
                .and_then(|s| s.chars().next());
            self.parser.utf8.clear();
            if let Some(ch) = ch {
                self.char(ch);
            }
            return;
        }
        self.char(char::from(b));
    }

    fn char(&mut self, c: char) {
        let u = u32::from(c);
        // Anywhere: CAN and SUB cancel, ESC starts over.
        match (&self.parser.state, u) {
            (State::Osc, 0x07) => {
                self.osc_end();
                return;
            }
            (State::Skip | State::SkipEscape, 0x07) => {
                self.parser.state = State::Ground;
                return;
            }
            (State::Osc, 0x1B) => {
                self.parser.state = State::OscEscape;
                return;
            }
            (State::Skip, 0x1B) => {
                self.parser.state = State::SkipEscape;
                return;
            }
            (State::OscEscape, _) => {
                // ESC \ ends the string; anything else ends it too.
                self.osc_end();
                if u != u32::from(b'\\') {
                    self.parser.state = State::Escape;
                    self.esc_start();
                    self.char(c);
                }
                return;
            }
            (State::SkipEscape, _) => {
                self.parser.state = State::Ground;
                return;
            }
            (State::Osc, _) => {
                if u >= 0x20 && self.parser.osc.len() < OSC_MAX {
                    self.parser.osc.push(c);
                }
                return;
            }
            (State::Skip, _) => return,
            (_, 0x18 | 0x1A) => {
                self.parser.state = State::Ground;
                return;
            }
            (_, 0x1B) => {
                self.parser.state = State::Escape;
                self.esc_start();
                return;
            }
            (_, 0x9B) => {
                self.esc_start();
                self.parser.state = State::Csi;
                return;
            }
            (_, 0x9D) => {
                self.parser.osc.clear();
                self.parser.state = State::Osc;
                return;
            }
            (_, 0x90 | 0x98 | 0x9E | 0x9F) => {
                self.parser.state = State::Skip;
                return;
            }
            (_, 0..=0x1F) => {
                // C0 controls act inside sequences too.
                self.control(u);
                return;
            }
            (_, 0x80..=0x9F) => {
                self.parser.state = State::Ground;
                self.control(u);
                return;
            }
            _ => {}
        }
        match self.parser.state {
            State::Ground => {
                if u != 0x7F {
                    self.print(c);
                }
            }
            State::Escape | State::EscapeIntermediate => {
                if (0x20..=0x2F).contains(&u) {
                    self.collect(u);
                    self.parser.state = State::EscapeIntermediate;
                } else if self.parser.state == State::Escape && u == u32::from(b'[') {
                    self.parser.state = State::Csi;
                } else if self.parser.state == State::Escape && u == u32::from(b']') {
                    self.parser.osc.clear();
                    self.parser.state = State::Osc;
                } else if self.parser.state == State::Escape
                    && matches!(c, 'P' | 'X' | '^' | '_')
                {
                    self.parser.state = State::Skip;
                } else if (0x30..=0x7E).contains(&u) {
                    self.parser.state = State::Ground;
                    self.esc_dispatch(c);
                } else {
                    self.parser.state = State::Ground;
                }
            }
            State::Csi => match c {
                '0'..='9' => {
                    let d = u - u32::from(b'0');
                    let v = self.parser.current.unwrap_or(0);
                    self.parser.current = Some(v.saturating_mul(10).saturating_add(d).min(99_999));
                }
                ';' => {
                    if self.parser.params.len() < PARAMS {
                        self.parser.params.push(self.parser.current.take());
                    } else {
                        self.parser.current = None;
                    }
                }
                ':' => self.parser.state = State::CsiIgnore,
                '<'..='?' | ' '..='/' => self.collect(u),
                '@'..='~' => {
                    if self.parser.params.len() < PARAMS {
                        self.parser.params.push(self.parser.current.take());
                    }
                    self.parser.state = State::Ground;
                    self.csi_dispatch(c);
                }
                _ => self.parser.state = State::Ground,
            },
            State::CsiIgnore => {
                if (0x40..=0x7E).contains(&u) {
                    self.parser.state = State::Ground;
                }
            }
            State::Osc | State::OscEscape | State::Skip | State::SkipEscape => {}
        }
    }

    fn esc_start(&mut self) {
        self.parser.params.clear();
        self.parser.current = None;
        self.parser.collect.clear();
    }

    fn collect(&mut self, u: u32) {
        if self.parser.collect.len() < 8 {
            self.parser.collect.push(u as u8);
        }
    }

    /// Parameter `i`, or `default` when missing or zero (as Mosh reads
    /// them).
    fn param(&self, i: usize, default: u32) -> u32 {
        match self.parser.params.get(i).copied().flatten() {
            Some(v) if v >= 1 => v,
            _ => default,
        }
    }

    fn param_count(&self) -> usize {
        self.parser.params.len()
    }

    fn control(&mut self, u: u32) {
        let keep_wrap = u == 0x09;
        let saved = self.ds.next_print_wraps;
        if !keep_wrap {
            self.ds.next_print_wraps = false;
        }
        match u {
            0x07 => self.bells = self.bells.wrapping_add(1),
            0x08 => self.ds.move_col(-1, true, false),
            0x09 => {
                let col = ((self.ds.cursor_col / 8) + 1) * 8;
                self.ds.move_col(col.min(self.ds.width - 1) as i64, false, false);
                self.ds.next_print_wraps = saved;
            }
            0x0A..=0x0C | 0x84 => self.move_rows_autoscroll(1),
            0x0D => self.ds.move_col(0, false, false),
            0x85 => {
                self.ds.move_col(0, false, false);
                self.move_rows_autoscroll(1);
            }
            0x8D => self.move_rows_autoscroll(-1),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, c: char) {
        if self.parser.collect.is_empty() && ('@'..='_').contains(&c) {
            // ESC Fe: the 7-bit form of a C1 control.
            self.control(u32::from(c) + 0x40);
            return;
        }
        self.ds.next_print_wraps = false;
        match (self.parser.collect.as_slice(), c) {
            (b"", '7') => {
                self.ds.saved = Saved {
                    col: self.ds.cursor_col,
                    row: self.ds.cursor_row,
                    rend: self.ds.rend,
                    autowrap: self.ds.autowrap,
                    origin: self.ds.origin,
                };
            }
            (b"", '8') => {
                let s = self.ds.saved;
                self.ds.cursor_col = s.col;
                self.ds.cursor_row = s.row;
                self.ds.rend = s.rend;
                self.ds.autowrap = s.autowrap;
                self.ds.origin = s.origin;
                self.ds.snap(s.row as i64, s.col as i64);
                self.ds.new_grapheme();
            }
            (b"", 'c') => {
                let (w, h) = (self.ds.width, self.ds.height);
                let keep = (self.title.clone(), self.icon_name.clone(), self.title_set);
                let bells = self.bells;
                *self = Screen::new(w, h);
                (self.title, self.icon_name, self.title_set) = keep;
                self.bells = bells;
            }
            (b"#", '8') => {
                for row in 0..self.ds.height {
                    for col in 0..self.ds.width {
                        self.reset_cell(row, col);
                        if let Some(cell) = self.cell_mut(row, col) {
                            cell.text.push('E');
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, c: char) {
        let private = self.parser.collect.as_slice() == b"?";
        let plain = self.parser.collect.is_empty();
        // Mode and rendition changes keep the pending wrap.
        let keeps = matches!(c, 'h' | 'l') && private || plain && matches!(c, 'm' | 'g' | 'I' | 'Z');
        if !keeps {
            self.ds.next_print_wraps = false;
        }
        if private {
            match c {
                'h' => self.dec_modes(true),
                'l' => self.dec_modes(false),
                _ => {}
            }
            return;
        }
        if !plain {
            return;
        }
        let n = i64::from(self.param(0, 1));
        let w = self.ds.width;
        match c {
            'A' => self.ds.move_row(-n, true),
            'B' => self.ds.move_row(n, true),
            'C' => self.ds.move_col(n, true, false),
            'D' => self.ds.move_col(-n, true, false),
            'H' | 'f' => {
                self.ds.move_row(i64::from(self.param(0, 1)) - 1, false);
                self.ds.move_col(i64::from(self.param(1, 1)) - 1, false, false);
            }
            'G' | '`' => self.ds.move_col(n - 1, false, false),
            'd' => self.ds.move_row(n - 1, false),
            'J' => {
                let (row, col) = (self.ds.cursor_row, self.ds.cursor_col);
                match self.param(0, 0) {
                    0 => {
                        for x in col..w {
                            self.reset_cell(row, x);
                        }
                        for y in row + 1..self.ds.height {
                            self.reset_row(y);
                        }
                    }
                    1 => {
                        for y in 0..row {
                            self.reset_row(y);
                        }
                        for x in 0..=col {
                            self.reset_cell(row, x);
                        }
                    }
                    2 => {
                        for y in 0..self.ds.height {
                            self.reset_row(y);
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let (row, col) = (self.ds.cursor_row, self.ds.cursor_col);
                match self.param(0, 0) {
                    0 => (col..w).for_each(|x| self.reset_cell(row, x)),
                    1 => (0..=col).for_each(|x| self.reset_cell(row, x)),
                    2 => self.reset_row(row),
                    _ => {}
                }
            }
            'X' => {
                let (row, col) = (self.ds.cursor_row, self.ds.cursor_col);
                let end = (col + n as usize).min(w);
                (col..end).for_each(|x| self.reset_cell(row, x));
            }
            '@' => {
                let (row, col) = (self.ds.cursor_row, self.ds.cursor_col);
                let bg = self.bg();
                if let Some(r) = self.row_mut(row) {
                    for _ in 0..(n as usize).min(w) {
                        r.cells.insert(col, Cell::blank(bg));
                        r.cells.pop();
                    }
                }
            }
            'P' => {
                let (row, col) = (self.ds.cursor_row, self.ds.cursor_col);
                let bg = self.bg();
                if let Some(r) = self.row_mut(row) {
                    for _ in 0..(n as usize).min(w) {
                        if col < r.cells.len() {
                            r.cells.remove(col);
                            r.cells.push(Cell::blank(bg));
                        }
                    }
                }
            }
            'L' => {
                let row = self.ds.cursor_row;
                self.insert_lines(row, n as usize);
                self.ds.move_col(0, false, false);
            }
            'M' => {
                let row = self.ds.cursor_row;
                self.delete_lines(row, n as usize);
                self.ds.move_col(0, false, false);
            }
            'S' => self.scroll(n),
            'T' => self.scroll(-n),
            'r' => {
                let top = self.param(0, 1) as usize;
                let bottom = self.param(1, self.ds.height as u32) as usize;
                if bottom <= top || top > self.ds.height {
                    return;
                }
                self.ds.set_region(top - 1, bottom - 1);
                self.ds.move_row(0, false);
                self.ds.move_col(0, false, false);
            }
            'm' => self.sgr(),
            'h' | 'l' => {
                for i in 0..self.param_count() {
                    if self.param(i, 0) == 4 {
                        self.ds.insert = c == 'h';
                    }
                }
            }
            _ => {}
        }
    }

    fn dec_modes(&mut self, on: bool) {
        for i in 0..self.param_count() {
            let p = self.param(i, 0);
            match p {
                9 | 1000..=1003 => self.ds.mouse.reporting = if on { p as u16 } else { 0 },
                1005 | 1006 | 1015 => self.ds.mouse.encoding = if on { p as u16 } else { 0 },
                1 => self.ds.app_cursor_keys = on,
                3 => {
                    self.ds.move_row(0, false);
                    self.ds.move_col(0, false, false);
                    for y in 0..self.ds.height {
                        self.reset_row(y);
                    }
                }
                5 => self.ds.reverse_video = on,
                6 => {
                    self.ds.move_row(0, false);
                    self.ds.move_col(0, false, false);
                    self.ds.origin = on;
                }
                7 => self.ds.autowrap = on,
                25 => self.ds.cursor_visible = on,
                1004 => self.ds.mouse.focus = on,
                1007 => self.ds.mouse.alternate_scroll = on,
                2004 => self.ds.bracketed_paste = on,
                _ => {}
            }
        }
    }

    fn sgr(&mut self) {
        let count = self.param_count();
        let mut i = 0;
        while i < count {
            let r = self.param(i, 0);
            let sub = self.parser.params.get(i + 1).copied().flatten();
            if (r == 38 || r == 48) && count - i >= 3 && sub == Some(5) {
                let n = self.param(i + 2, 0);
                if n <= 255 {
                    let color = Color::Indexed(n as u8);
                    if r == 38 {
                        self.ds.rend.fg = color;
                    } else {
                        self.ds.rend.bg = color;
                    }
                }
                i += 3;
                continue;
            }
            if (r == 38 || r == 48) && count - i >= 5 && sub == Some(2) {
                let ch = |k: usize| self.param(i + k, 0).min(255) as u8;
                let color = Color::Rgb(ch(2), ch(3), ch(4));
                if r == 38 {
                    self.ds.rend.fg = color;
                } else {
                    self.ds.rend.bg = color;
                }
                i += 5;
                continue;
            }
            self.ds.rend.set(r);
            i += 1;
        }
    }

    fn osc_end(&mut self) {
        self.parser.state = State::Ground;
        let osc = std::mem::take(&mut self.parser.osc);
        if let Some(clip) = osc.strip_prefix("52;c;") {
            self.clipboard = clip.to_owned();
            return;
        }
        let (cmd, text) = if let Some(t) = osc.strip_prefix(';') {
            (Some('0'), t)
        } else {
            let mut cs = osc.chars();
            match (cs.next(), cs.next()) {
                (Some(d), Some(';')) => (Some(d), cs.as_str()),
                _ => (None, ""),
            }
        };
        match cmd {
            Some('8') => {
                // 8;params;url
                if let Some((params, url)) = text.split_once(';') {
                    self.ds.link = if url.is_empty() {
                        None
                    } else {
                        Some(Arc::new((params.to_owned(), url.to_owned())))
                    };
                }
            }
            Some(d @ ('0' | '1' | '2')) => {
                self.title_set = true;
                // Mosh keeps the first 256 characters of the whole OSC.
                let keep = TITLE.saturating_sub(osc.chars().count() - text.chars().count());
                let title: String = text.chars().take(keep).collect();
                if d != '2' {
                    self.icon_name = title.clone();
                }
                if d != '1' {
                    self.title = title;
                }
            }
            _ => {}
        }
    }

    fn print(&mut self, c: char) {
        let width = if c == '\0' {
            None
        } else if (' '..='~').contains(&c) || ('\u{a0}'..='\u{ff}').contains(&c) {
            Some(1)
        } else {
            c.width()
        };
        let w = self.ds.width;
        match width {
            Some(cw @ (1 | 2)) => {
                if self.ds.autowrap && self.ds.next_print_wraps {
                    let row = self.ds.cursor_row;
                    if let Some(r) = self.row_mut(row) {
                        r.set_wrap(true);
                    }
                    self.ds.move_col(0, false, false);
                    self.move_rows_autoscroll(1);
                } else if self.ds.autowrap && cw == 2 && self.ds.cursor_col == w - 1 {
                    let (row, col) = (self.ds.cursor_row, self.ds.cursor_col);
                    self.reset_cell(row, col);
                    if let Some(r) = self.row_mut(row) {
                        r.set_wrap(false);
                    }
                    self.ds.move_col(0, false, false);
                    self.move_rows_autoscroll(1);
                }
                let (row, col) = (self.ds.cursor_row, self.ds.cursor_col);
                if self.ds.insert {
                    let bg = self.bg();
                    if let Some(r) = self.row_mut(row) {
                        for _ in 0..cw {
                            r.cells.insert(col, Cell::blank(bg));
                            r.cells.pop();
                        }
                    }
                }
                let (rend, link) = (self.ds.rend, self.ds.link.clone());
                self.reset_cell(row, col);
                if let Some(cell) = self.cell_mut(row, col) {
                    cell.push(c);
                    cell.wide = cw == 2;
                    cell.rend = rend;
                    cell.link = link;
                }
                if cw == 2 && col + 1 < w {
                    self.reset_cell(row, col + 1);
                }
                self.ds.move_col(cw as i64, true, true);
            }
            Some(0) => {
                let Some((row, col)) = self.ds.combining else {
                    return;
                };
                if row >= self.ds.height || col >= w {
                    return;
                }
                let empty = self.cell(row, col).is_some_and(|c| c.text.is_empty());
                if empty {
                    if let Some(cell) = self.cell_mut(row, col) {
                        cell.fallback = true;
                    }
                    self.ds.move_col(1, true, true);
                }
                if let Some(cell) = self.cell_mut(row, col) {
                    cell.push(c);
                }
            }
            _ => {}
        }
    }
}

/// What the pane's terminal is drawing with while a frame is written.
struct Frame<'a> {
    out: String,
    x: i64,
    y: i64,
    rend: Rendition,
    link: Link,
    cursor_visible: bool,
    last: &'a Screen,
}

impl Frame<'_> {
    fn hide_cursor(&mut self) {
        if self.cursor_visible {
            self.out.push_str("\x1b[?25l");
            self.cursor_visible = false;
        }
    }

    fn silent_move(&mut self, y: usize, x: usize) {
        if self.x == x as i64 && self.y == y as i64 {
            return;
        }
        self.hide_cursor();
        self.move_to(y, x);
    }

    fn move_to(&mut self, y: usize, x: usize) {
        let (lx, ly) = (self.x, self.y);
        let (x, y) = (x as i64, y as i64);
        self.x = x;
        self.y = y;
        if lx != -1 && ly != -1 {
            if x == 0 && y - ly >= 0 && y - ly < 5 {
                if lx != 0 {
                    self.out.push('\r');
                }
                for _ in 0..y - ly {
                    self.out.push('\n');
                }
                return;
            }
            if y == ly && x - lx < 0 && x - lx > -5 {
                for _ in 0..lx - x {
                    self.out.push('\x08');
                }
                return;
            }
        }
        self.out.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
    }

    fn rendition(&mut self, r: Rendition, force: bool) {
        if force || self.rend != r {
            self.out.push_str(&r.sgr());
            self.rend = r;
        }
    }

    fn hyperlink(&mut self, l: &Link, force: bool) {
        if force || self.link != *l {
            self.out.push_str(&osc8(l));
            self.link = l.clone();
        }
    }

    fn cell(&mut self, c: &Cell) {
        c.grapheme(&mut self.out);
    }
}

/// Writes the output that turns a terminal showing `last` into one
/// showing `next`. Without `initialized`, the terminal's contents are
/// unknown and everything is drawn. This is Mosh's own display algorithm,
/// so the output scrolls the terminal (and its scrollback) as the screen
/// scrolled, rather than redrawing it.
pub fn render(initialized: bool, last: &Screen, next: &Screen) -> String {
    let mut initialized = initialized;
    let mut f = Frame {
        out: String::new(),
        x: 0,
        y: 0,
        rend: Rendition::default(),
        link: None,
        cursor_visible: last.ds.cursor_visible,
        last,
    };
    if next.bells != f.last.bells {
        f.out.push('\x07');
    }
    if next.title_set
        && (!initialized || next.icon_name != f.last.icon_name || next.title != f.last.title)
    {
        let clean = |s: &str| -> String { s.chars().filter(|c| !c.is_control()).collect() };
        if next.icon_name == next.title {
            f.out.push_str(&format!("\x1b]0;{}\x07", clean(&next.title)));
        } else {
            f.out.push_str(&format!("\x1b]1;{}\x07", clean(&next.icon_name)));
            f.out.push_str(&format!("\x1b]2;{}\x07", clean(&next.title)));
        }
    }
    if next.clipboard != f.last.clipboard {
        let clip: String = next
            .clipboard
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            .collect();
        f.out.push_str(&format!("\x1b]52;c;{clip}\x07"));
    }
    if !initialized || next.ds.reverse_video != f.last.ds.reverse_video {
        f.out
            .push_str(if next.ds.reverse_video { "\x1b[?5h" } else { "\x1b[?5l" });
    }
    let (w, h) = (next.width(), next.height());
    if !initialized || w != f.last.width() || h != f.last.height() {
        f.out.push_str("\x1b[r\x1b[0m\x1b[H\x1b[2J");
        initialized = false;
        f.x = 0;
        f.y = 0;
        f.rend = Rendition::default();
        f.link = None;
    } else {
        f.x = f.last.ds.cursor_col as i64;
        f.y = f.last.ds.cursor_row as i64;
        f.rend = f.last.ds.rend;
        f.link = f.last.ds.link.clone();
    }
    if !initialized {
        f.cursor_visible = false;
        f.out.push_str("\x1b[?25l");
    }

    let mut rows: Vec<Arc<Row>> = f.last.rows.clone();
    if f.last.width() < w {
        for r in &mut rows {
            let mut wider = (**r).clone();
            wider.cells.resize(w, Cell::blank(Color::Default));
            *r = Arc::new(wider);
        }
    }
    let blank = Arc::new(Row::new(w, Color::Default));
    if rows.len() < h {
        rows.resize(h, blank.clone());
    }

    let mut frame_y = 0;
    if initialized {
        let mut lines_scrolled = 0;
        let mut scroll_height = 0;
        if let Some(first) = next.rows.first() {
            for (row, old) in rows.iter().enumerate().take(h) {
                if !(Arc::ptr_eq(first, old) || **first == **old) {
                    continue;
                }
                if row == 0 {
                    break;
                }
                lines_scrolled = row;
                scroll_height = 1;
                let mut region = 1;
                while lines_scrolled + region < h {
                    match (next.rows.get(region), rows.get(lines_scrolled + region)) {
                        (Some(a), Some(b)) if **a == **b => scroll_height = region + 1,
                        _ => break,
                    }
                    region += 1;
                }
                break;
            }
        }
        if scroll_height > 0 {
            frame_y = scroll_height;
            if lines_scrolled > 0 {
                f.rendition(Rendition::default(), true);
                f.hyperlink(&None, true);
                let bottom = lines_scrolled + scroll_height - 1;
                if scroll_height + lines_scrolled == h && f.y + 1 == h as i64 {
                    f.out.push('\r');
                    for _ in 0..lines_scrolled {
                        f.out.push('\n');
                    }
                    f.x = 0;
                } else {
                    f.out.push_str(&format!("\x1b[1;{}r", bottom + 1));
                    f.x = -1;
                    f.y = -1;
                    f.silent_move(bottom, 0);
                    for _ in 0..lines_scrolled {
                        f.out.push('\n');
                    }
                    f.out.push_str("\x1b[r");
                    f.x = -1;
                    f.y = -1;
                }
                for i in 0..=bottom {
                    let moved = if i + lines_scrolled <= bottom {
                        rows.get(i + lines_scrolled).cloned()
                    } else {
                        None
                    };
                    if let Some(slot) = rows.get_mut(i) {
                        *slot = moved.unwrap_or_else(|| blank.clone());
                    }
                }
            }
        }
    }

    let mut wrap = false;
    while frame_y < h {
        if let (Some(row), Some(old)) = (next.rows.get(frame_y), rows.get(frame_y)) {
            wrap = put_row(initialized, &mut f, next, frame_y, row, old, wrap);
        }
        frame_y += 1;
    }

    let (cy, cx) = (next.ds.cursor_row, next.ds.cursor_col);
    if !initialized || cy as i64 != f.y || cx as i64 != f.x {
        f.move_to(cy, cx);
    }
    if !initialized || next.ds.cursor_visible != f.cursor_visible {
        f.out.push_str(if next.ds.cursor_visible {
            "\x1b[?25h"
        } else {
            "\x1b[?25l"
        });
    }
    f.rendition(next.ds.rend, !initialized);
    f.hyperlink(&next.ds.link, !initialized);
    let lds = &f.last.ds;
    if !initialized || next.ds.bracketed_paste != lds.bracketed_paste {
        f.out.push_str(if next.ds.bracketed_paste {
            "\x1b[?2004h"
        } else {
            "\x1b[?2004l"
        });
    }
    let (m, lm) = (next.ds.mouse, lds.mouse);
    if !initialized || m.reporting != lm.reporting {
        if m.reporting == 0 {
            f.out.push_str("\x1b[?1003l\x1b[?1002l\x1b[?1001l\x1b[?1000l");
        } else {
            if lm.reporting != 0 {
                f.out.push_str(&format!("\x1b[?{}l", lm.reporting));
            }
            f.out.push_str(&format!("\x1b[?{}h", m.reporting));
        }
    }
    if !initialized || m.focus != lm.focus {
        f.out
            .push_str(if m.focus { "\x1b[?1004h" } else { "\x1b[?1004l" });
    }
    if !initialized || m.encoding != lm.encoding {
        if m.encoding == 0 {
            f.out.push_str("\x1b[?1015l\x1b[?1006l\x1b[?1005l");
        } else {
            if lm.encoding != 0 {
                f.out.push_str(&format!("\x1b[?{}l", lm.encoding));
            }
            f.out.push_str(&format!("\x1b[?{}h", m.encoding));
        }
    }
    f.out
}

fn put_row(
    initialized: bool,
    f: &mut Frame<'_>,
    next: &Screen,
    y: usize,
    row: &Arc<Row>,
    old: &Arc<Row>,
    wrap: bool,
) -> bool {
    let mut x = 0;
    let cells = &row.cells;
    if wrap && let Some(cell) = cells.first() {
        f.rendition(cell.rend, false);
        f.hyperlink(&cell.link, false);
        f.cell(cell);
        x += cell.width();
        f.x += cell.width() as i64;
    }
    if initialized && Arc::ptr_eq(row, old) {
        return false;
    }
    let wrap_this = row.wraps();
    let width = next.width();
    let mut clear = 0;
    let mut wrote_last = false;
    let mut blank_rend = Rendition::default();
    let mut blank_link: Link = None;
    while x < width {
        let Some(cell) = cells.get(x) else {
            break;
        };
        if initialized && clear == 0 && old.cells.get(x) == Some(cell) {
            x += cell.width();
            continue;
        }
        if cell.text.is_empty() {
            if clear == 0 {
                blank_rend = cell.rend;
                blank_link = cell.link.clone();
            }
            if cell.rend == blank_rend && cell.link == blank_link {
                clear += 1;
                x += 1;
                continue;
            }
        }
        if clear > 0 {
            f.silent_move(y, x - clear);
            f.rendition(blank_rend, false);
            f.hyperlink(&blank_link, false);
            if clear > 4 {
                f.out.push_str(&format!("\x1b[{clear}X"));
            } else {
                for _ in 0..clear {
                    f.out.push(' ');
                }
                f.x = x as i64;
            }
            clear = 0;
            if cell.text.is_empty() {
                blank_rend = cell.rend;
                blank_link = cell.link.clone();
                clear = 1;
                x += 1;
                continue;
            }
        }
        let cw = cell.width();
        if wrap_this && x + cw >= width {
            f.x = -1;
            f.y = -1;
        }
        f.silent_move(y, x);
        f.rendition(cell.rend, false);
        f.hyperlink(&cell.link, false);
        f.cell(cell);
        x += cw;
        f.x += cw as i64;
        if x >= width {
            wrote_last = true;
        }
    }
    if clear > 0 {
        f.silent_move(y, x - clear);
        f.rendition(blank_rend, false);
        f.hyperlink(&blank_link, false);
        if !wrap_this {
            f.out.push_str("\x1b[K");
        } else {
            for _ in 0..clear {
                f.out.push(' ');
            }
            f.x = x as i64;
            wrote_last = true;
        }
    }
    if !(wrote_last && y + 1 < next.height()) {
        return false;
    }
    if wrap_this {
        f.x = 0;
        f.y += 1;
        return true;
    }
    f.out.push_str("\r\n");
    f.x = 0;
    f.y += 1;
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn text(s: &Screen, row: usize) -> String {
        s.rows[row]
            .cells
            .iter()
            .map(|c| if c.text.is_empty() { " ".into() } else { c.text.clone() })
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
                assert_eq!(text(&shown, y), text(next, y), "row {y} init {init}: {out:?}");
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
        assert_eq!((s.title.as_str(), s.icon_name.as_str()), ("hi there", "hi there"));
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

    #[test]
    fn hostile_input_stays_bounded() {
        let mut s = Screen::new(3, 2);
        s.apply(b"\x1b[99999999999;99999999999H\x1b[99999@\x1b[99999P\x1b[99999L\x1b[99999M");
        s.apply(b"\x1b[99999X\x1b[0;0r\x1b[5;1r\x1b[2;99r\x1b[?6h\x1b[99B\x1b[S\x1b[T");
        s.apply(&[0xF0, 0x9F, b'x', 0xC3, 0x28, 0xFF, 0x9B, b'2', b'J', 0x9D, b'0', b';', 0x07]);
        let long = vec![b'a'; 100_000];
        s.apply(b"\x1b]0;");
        s.apply(&long);
        s.apply(b"\x07");
        assert!(s.title.chars().count() <= 256);
        assert_eq!(s.rows.len(), 2);
        assert!(s.rows.iter().all(|r| r.cells.len() == 3));
    }
}
