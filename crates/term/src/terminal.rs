//! The terminal: host output in, grid and replies out.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags as TermFlags;
use alacritty_terminal::term::color::{COUNT as COLOR_COUNT, Colors};
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::{
    ClipboardType, Config, Osc52, Term, TermMode, point_to_viewport, viewport_to_point,
};
use alacritty_terminal::vte::ansi::{
    Color as TermColor, CursorShape as TermCursorShape, Handler, NamedColor, Processor,
    Rgb as TermRgb,
};
use ferossh_render::{Cell, Color, Cursor, CursorShape, Flags, Grid, Rgb, Theme};

use crate::cwd::{CwdScanner, WorkingDir};
use crate::input::{self, Key, Mods, MouseButton, MouseEvent};
use crate::sanitize::{Paste, prepare_paste, sanitize_title};

/// Per-session terminal settings, from the window's settings file and the
/// host's session profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Lines of scrollback kept above the screen.
    pub scrollback: usize,
}

impl Settings {
    pub const DEFAULT_SCROLLBACK: usize = 10_000;
    pub const MAX_SCROLLBACK: usize = 1_000_000;

    /// Settings with the scrollback clamped to [`Self::MAX_SCROLLBACK`].
    pub fn new(scrollback: usize) -> Self {
        Self {
            scrollback: scrollback.min(Self::MAX_SCROLLBACK),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new(Self::DEFAULT_SCROLLBACK)
    }
}

/// Something the UI must do after the host's output was processed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Bytes to send to the host: answers to its queries.
    Reply(Vec<u8>),
    /// The host set the window title (already sanitised), or reset it.
    Title(Option<String>),
    /// The host wants this text on the clipboard (OSC 52). The UI asks the
    /// user before doing it.
    ClipboardRequest(String),
    Bell,
    /// The shell reported its working directory (OSC 7).
    WorkingDirectory(WorkingDir),
}

/// How a mouse selection grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Cell by cell.
    Simple,
    /// A rectangle.
    Block,
    /// Whole words.
    Word,
    /// Whole lines.
    Line,
}

/// Largest OSC 52 text passed on for confirmation.
const MAX_CLIPBOARD_REQUEST: usize = 1 << 20;
/// Events kept between two `take_actions` calls; a host that floods
/// queries gets the excess dropped rather than growing memory.
const MAX_QUEUED_EVENTS: usize = 1024;
/// Combining-mark clusters a grid may collect before they are reset.
const MAX_CLUSTERS: usize = 4096;

/// Collects the emulator's events for `take_actions`.
#[derive(Clone, Debug, Default)]
struct Events(Rc<RefCell<Vec<Event>>>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        if let Ok(mut queue) = self.0.try_borrow_mut()
            && queue.len() < MAX_QUEUED_EVENTS
        {
            queue.push(event);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Debug, Default)]
struct Search {
    query: String,
    regex: Option<RegexSearch>,
    found: Option<Match>,
}

/// One terminal: the emulator, its parser and the pane's search state.
pub struct Terminal {
    term: Term<Events>,
    parser: Processor,
    events: Events,
    /// Answers colour queries for colours the host has not set.
    theme: Theme,
    cell_px: (u16, u16),
    search: Option<Search>,
    cwd: CwdScanner,
    /// Working directories reported since the last `take_actions`.
    reported: Vec<WorkingDir>,
    /// The title the host last set, as the UI was told.
    title: Option<String>,
    /// While the alternate screen is up: the title from before it, and
    /// whether the app on it set another.
    alt_title: Option<(Option<String>, bool)>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("cols", &self.term.columns())
            .field("rows", &self.term.screen_lines())
            .field("history", &self.term.history_size())
            .finish_non_exhaustive()
    }
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, settings: &Settings) -> Self {
        let events = Events::default();
        let size = Size {
            cols: cols.max(2),
            rows: rows.max(1),
        };
        let term = Term::new(config(settings), &size, events.clone());
        Self {
            term,
            parser: Processor::new(),
            events,
            theme: Theme::default(),
            cell_px: (8, 16),
            search: None,
            cwd: CwdScanner::default(),
            reported: Vec::new(),
            title: None,
            alt_title: None,
        }
    }

    pub fn set_settings(&mut self, settings: &Settings) {
        self.term.set_options(config(settings));
    }

    /// The theme colour queries (OSC 4/10/11) are answered from.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    /// The pixel size of a cell, for text-area size queries.
    pub fn set_cell_size(&mut self, width: u32, height: u32) {
        let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
        self.cell_px = (clamp(width), clamp(height));
    }

    pub fn cols(&self) -> usize {
        self.term.columns()
    }

    pub fn rows(&self) -> usize {
        self.term.screen_lines()
    }

    /// Resizes the screen; lines reflow and the scrollback is kept.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let size = Size {
            cols: cols.max(2),
            rows: rows.max(1),
        };
        if size.cols != self.cols() || size.rows != self.rows() {
            self.term.resize(size);
            if let Some(s) = &mut self.search {
                s.found = None;
            }
        }
    }

    /// Processes output from the host. Call [`Self::take_actions`] after.
    pub fn feed(&mut self, bytes: &[u8]) {
        let found = self.cwd.scan(bytes);
        // Only the last report of a burst matters.
        if let Some(last) = found.into_iter().last() {
            self.reported.clear();
            self.reported.push(last);
        }
        // Fed in pieces that end at alternate screen switches, so the
        // titles set on it are told apart from the ones around it.
        let mut start = 0;
        for end in alt_switch_ends(bytes).chain([bytes.len()]) {
            if let Some(piece) = bytes.get(start..end).filter(|p| !p.is_empty()) {
                let queued = self.events.0.borrow().len();
                self.parser.advance(&mut self.term, piece);
                self.follow_title(queued);
            }
            start = end;
        }
    }

    /// Notes the titles set since event `queued`, then what the alternate
    /// screen did. An app that set a title on the alternate screen and
    /// leaves it without restoring the old one (mc on hosts whose
    /// terminfo has no title stack) gets the old one put back: its title
    /// does not describe the shell after it.
    fn follow_title(&mut self, queued: usize) {
        let mut set = None;
        for event in self.events.0.borrow().iter().skip(queued) {
            match event {
                Event::Title(t) => set = Some(Some(t.clone())),
                Event::ResetTitle => set = Some(None),
                _ => {}
            }
        }
        if let Some(title) = set {
            if let Some((before, changed)) = &mut self.alt_title {
                *changed = title != *before;
            }
            self.title = title;
        }
        let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
        match (alt, self.alt_title.take()) {
            (true, None) => self.alt_title = Some((self.title.clone(), false)),
            (true, kept) => self.alt_title = kept,
            (false, Some((before, true))) if before != self.title => {
                self.title.clone_from(&before);
                self.term.set_title(before);
            }
            (false, _) => {}
        }
    }

    /// When a synchronized update (DEC mode 2026) started by the host
    /// times out. Call [`Self::flush_sync`] then if no more output came.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Ends a pending synchronized update and shows what it buffered.
    pub fn flush_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
    }

    /// What the host's output asked the UI to do, in order.
    pub fn take_actions(&mut self) -> Vec<Action> {
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        let mut actions: Vec<Action> = events.into_iter().filter_map(|e| self.action(e)).collect();
        actions.extend(self.reported.drain(..).map(Action::WorkingDirectory));
        actions
    }

    fn action(&self, event: Event) -> Option<Action> {
        Some(match event {
            Event::PtyWrite(text) => Action::Reply(text.into_bytes()),
            Event::Title(title) => Action::Title(Some(sanitize_title(&title))),
            Event::ResetTitle => Action::Title(None),
            Event::ClipboardStore(ClipboardType::Clipboard, text)
                if text.len() <= MAX_CLIPBOARD_REQUEST =>
            {
                Action::ClipboardRequest(text)
            }
            Event::ColorRequest(index, format) => {
                Action::Reply(format(self.color_value(index)).into_bytes())
            }
            Event::TextAreaSizeRequest(format) => {
                let clamp = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
                let size = WindowSize {
                    num_lines: clamp(self.rows()),
                    num_cols: clamp(self.cols()),
                    cell_width: self.cell_px.0,
                    cell_height: self.cell_px.1,
                };
                Action::Reply(format(size).into_bytes())
            }
            Event::Bell => Action::Bell,
            // Clipboard reads are never answered (and OSC 52 is configured
            // copy-only, so they do not arrive). The rest are for a pty
            // event loop FeroSSH does not use.
            _ => return None,
        })
    }

    /// The colour the host sees when it queries `index` (alacritty's colour
    /// numbering: 0-255, then foreground, background, cursor).
    fn color_value(&self, index: usize) -> TermRgb {
        let theme = &self.theme;
        let set = (index < COLOR_COUNT)
            .then(|| self.term.colors()[index])
            .flatten();
        let rgb =
            set.map(|c| Rgb::new(c.r, c.g, c.b))
                .unwrap_or_else(|| match u8::try_from(index) {
                    Ok(i) => theme.indexed(i),
                    Err(_) if index == NamedColor::Background as usize => theme.background,
                    Err(_) if index == NamedColor::Cursor as usize => theme.cursor,
                    Err(_) => theme.foreground,
                });
        TermRgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        }
    }

    fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// Bytes for a non-text key, in the current keyboard modes.
    pub fn key(&self, key: Key, mods: Mods) -> Option<Vec<u8>> {
        input::encode_key(key, mods, self.mode())
    }

    /// Bytes for a typed character; `alt` adds the ESC prefix.
    pub fn text(&self, ch: char, alt: bool) -> Vec<u8> {
        input::encode_char(ch, alt)
    }

    /// A paste, cleaned and bracketed if the host asked for it.
    pub fn paste(&self, text: &str) -> Paste {
        prepare_paste(text, self.mode().contains(TermMode::BRACKETED_PASTE))
    }

    /// The report to send when focus changes, if the host asked for them.
    pub fn focus(&mut self, focused: bool) -> Option<Vec<u8>> {
        self.term.is_focused = focused;
        self.mode()
            .contains(TermMode::FOCUS_IN_OUT)
            .then(|| if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec())
    }

    /// Whether the host wants mouse events. Shift-clicks still select.
    pub fn mouse_reporting(&self) -> bool {
        self.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// The report for a mouse event at a viewport cell, if the host wants
    /// this kind of event.
    pub fn mouse(&self, event: MouseEvent, col: usize, row: usize, mods: Mods) -> Option<Vec<u8>> {
        input::encode_mouse(self.mode(), event, col, row, mods)
    }

    /// Handles a wheel turn of `lines` (positive away from the user): a
    /// mouse report per three lines, arrow keys in a full-screen program,
    /// or scrollback.
    pub fn wheel(&mut self, lines: i32, col: usize, row: usize, mods: Mods) -> Option<Vec<u8>> {
        if lines == 0 {
            return None;
        }
        let up = lines > 0;
        let count = lines.unsigned_abs() as usize;
        let mode = self.mode();
        if self.mouse_reporting() && !mods.shift {
            let button = if up {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            let one = self.mouse(MouseEvent::Press(button), col, row, mods)?;
            return Some(one.repeat(count.div_ceil(3)));
        }
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !mods.shift {
            let key = if up { Key::Up } else { Key::Down };
            let one = self.key(key, Mods::NONE)?;
            return Some(one.repeat(count));
        }
        self.term.scroll_display(Scroll::Delta(lines));
        None
    }

    /// Scrolls the view by lines (positive moves back in history).
    pub fn scroll_lines(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Scrolls the view by whole pages (positive moves back in history).
    pub fn scroll_pages(&mut self, pages: i32) {
        let scroll = if pages > 0 {
            Scroll::PageUp
        } else {
            Scroll::PageDown
        };
        for _ in 0..pages.unsigned_abs() {
            self.term.scroll_display(scroll);
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.term.scroll_display(Scroll::Top);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// How many lines the view is scrolled back.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Lines in the scrollback, above the screen.
    pub fn history_len(&self) -> usize {
        self.term.history_size()
    }

    pub fn clear_scrollback(&mut self) {
        self.term.selection = None;
        self.term.grid_mut().clear_history();
        if let Some(s) = &mut self.search {
            s.found = None;
        }
    }

    fn grid_point(&self, col: usize, row: usize) -> Point {
        let col = col.min(self.cols() - 1);
        let row = row.min(self.rows() - 1);
        viewport_to_point(self.display_offset(), Point::new(row, Column(col)))
    }

    /// Starts a selection at a viewport cell. `right_half` says which half
    /// of the cell the mouse is on.
    pub fn select_start(&mut self, col: usize, row: usize, right_half: bool, kind: SelectionKind) {
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Block => SelectionType::Block,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        let side = if right_half { Side::Right } else { Side::Left };
        let point = self.grid_point(col, row);
        self.term.selection = Some(Selection::new(ty, point, side));
    }

    pub fn select_update(&mut self, col: usize, row: usize, right_half: bool) {
        let point = self.grid_point(col, row);
        let side = if right_half { Side::Right } else { Side::Left };
        if let Some(sel) = &mut self.term.selection {
            sel.update(point, side);
        }
    }

    pub fn select_clear(&mut self) {
        self.term.selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// The raw OSC 8 link target of a viewport cell, if it has one. Check
    /// it with [`crate::check_link`] before opening it.
    pub fn link_at(&self, col: usize, row: usize) -> Option<String> {
        let point = self.grid_point(col, row);
        self.term.grid()[point]
            .hyperlink()
            .map(|h| h.uri().to_owned())
    }

    /// Opens the search bar (or keeps it open).
    pub fn search_open(&mut self) {
        self.search.get_or_insert_with(Search::default);
    }

    pub fn search_close(&mut self) {
        self.search = None;
    }

    pub fn search_active(&self) -> bool {
        self.search.is_some()
    }

    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.query.as_str())
    }

    /// Whether the last search found something.
    pub fn search_found(&self) -> bool {
        self.search.as_ref().is_some_and(|s| s.found.is_some())
    }

    /// Replaces the query and searches back from the bottom of the view.
    /// The query is literal text; it matches case-insensitively unless it
    /// contains capitals.
    pub fn search_set(&mut self, query: &str) {
        let Some(search) = &mut self.search else {
            return;
        };
        search.query = query.to_owned();
        search.regex = if query.is_empty() {
            None
        } else {
            RegexSearch::new(&escape_regex(query)).ok()
        };
        search.found = None;
        let bottom = Point::new(
            Line(self.rows() as i32 - 1 - self.display_offset() as i32),
            self.term.last_column(),
        );
        self.search_from(bottom, Direction::Left, Side::Right);
    }

    /// The next match further back in history (`older`) or towards the
    /// bottom, wrapping around.
    pub fn search_next(&mut self, older: bool) {
        let Some(found) = self.search.as_ref().and_then(|s| s.found.clone()) else {
            // Nothing found yet: start again from the bottom.
            let query = self.search_query().unwrap_or_default().to_owned();
            self.search_set(&query);
            return;
        };
        if older {
            let origin = found.start().sub(&self.term, Boundary::None, 1);
            self.search_from(origin, Direction::Left, Side::Right);
        } else {
            let origin = found.end().add(&self.term, Boundary::None, 1);
            self.search_from(origin, Direction::Right, Side::Left);
        }
    }

    fn search_from(&mut self, origin: Point, direction: Direction, side: Side) {
        let Some(search) = &mut self.search else {
            return;
        };
        let found = search
            .regex
            .as_mut()
            .and_then(|regex| self.term.search_next(regex, origin, direction, side, None));
        search.found = found.clone();
        match found {
            Some(m) => {
                let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                sel.update(*m.end(), Side::Right);
                self.term.selection = Some(sel);
                self.term.scroll_to_point(*m.start());
            }
            None => self.term.selection = None,
        }
    }

    /// Copies the visible screen into `grid` (resizing it to match),
    /// marking only changed rows dirty, and returns where to draw the
    /// cursor. While the search bar is open it takes the bottom row.
    pub fn draw_into(&self, grid: &mut Grid, focused: bool) -> Option<Cursor> {
        let (cols, rows) = (self.cols(), self.rows());
        grid.resize(cols, rows);
        grid.trim_clusters(MAX_CLUSTERS);
        let content = self.term.renderable_content();
        let offset = content.display_offset;
        let selection = content.selection;
        let colors = content.colors;

        let mut row_cells = Vec::with_capacity(cols);
        for y in 0..rows {
            row_cells.clear();
            let line = Line(y as i32 - offset as i32);
            let row = &self.term.grid()[line];
            for x in 0..cols {
                let cell = &row[Column(x)];
                let mut out = convert_cell(cell, colors);
                if selection.is_some_and(|s| s.contains(Point::new(line, Column(x)))) {
                    toggle(&mut out.flags, Flags::INVERSE);
                }
                if let Some(marks) = cell.zerowidth() {
                    out.cluster = grid.add_cluster(&marks.iter().collect::<String>());
                }
                row_cells.push(out);
            }
            grid.set_row(y, row_cells.iter().copied());
        }

        if let Some(search) = &self.search {
            let (bar, cursor_x) = search_bar(search, cols);
            grid.set_row(rows - 1, bar);
            return Some(Cursor {
                x: cursor_x,
                y: rows - 1,
                shape: if focused {
                    CursorShape::Beam
                } else {
                    CursorShape::Hollow
                },
            });
        }

        let cursor = content.cursor;
        let at = point_to_viewport(offset, cursor.point).filter(|p| p.line < rows)?;
        let shape = match cursor.shape {
            TermCursorShape::Hidden => return None,
            _ if !focused => CursorShape::Hollow,
            TermCursorShape::Block | TermCursorShape::HollowBlock => CursorShape::Block,
            TermCursorShape::Underline => CursorShape::Underline,
            TermCursorShape::Beam => CursorShape::Beam,
        };
        Some(Cursor {
            x: at.column.0.min(cols - 1),
            y: at.line.min(rows - 1),
            shape,
        })
    }
}

/// Where in `bytes` a sequence that may switch the alternate screen ends:
/// the `h` or `l` after 1049, the only mode the emulator switches it for.
/// A false match only splits the feed once more.
fn alt_switch_ends(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    bytes.iter().enumerate().filter_map(|(i, &b)| {
        ((b == b'h' || b == b'l') && bytes.get(..i)?.ends_with(b"1049")).then_some(i + 1)
    })
}

fn config(settings: &Settings) -> Config {
    Config {
        scrolling_history: settings.scrollback.min(Settings::MAX_SCROLLBACK),
        // Copy requests reach the UI, which asks first; pastes are never
        // handed to the host.
        osc52: Osc52::OnlyCopy,
        kitty_keyboard: false,
        ..Config::default()
    }
}

fn toggle(flags: &mut Flags, f: Flags) {
    if flags.contains(f) {
        flags.remove(f);
    } else {
        flags.insert(f);
    }
}

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell, colors: &Colors) -> Cell {
    let f = cell.flags;
    let mut flags = Flags::empty();
    for (from, to) in [
        (TermFlags::BOLD, Flags::BOLD),
        (TermFlags::ITALIC, Flags::ITALIC),
        (TermFlags::INVERSE, Flags::INVERSE),
        (TermFlags::DIM, Flags::DIM),
        (TermFlags::STRIKEOUT, Flags::STRIKEOUT),
        (TermFlags::WIDE_CHAR, Flags::WIDE),
        (TermFlags::WIDE_CHAR_SPACER, Flags::WIDE_SPACER),
    ] {
        if f.contains(from) {
            flags.insert(to);
        }
    }
    if f.intersects(TermFlags::ALL_UNDERLINES) {
        flags.insert(Flags::UNDERLINE);
    }
    let hidden = f.intersects(TermFlags::HIDDEN | TermFlags::LEADING_WIDE_CHAR_SPACER);
    let ch = if hidden || cell.c.is_control() {
        ' '
    } else {
        cell.c
    };
    Cell::new(ch, color(cell.fg, colors), color(cell.bg, colors), flags)
}

/// Maps the emulator's colour to the renderer's, keeping palette colours
/// symbolic (so a theme change recolours them) unless the host redefined
/// them with OSC 4/10/11.
fn color(c: TermColor, colors: &Colors) -> Color {
    let rgb = |c: TermRgb| Color::Rgb(Rgb::new(c.r, c.g, c.b));
    let set = |i: usize| (i < COLOR_COUNT).then(|| colors[i]).flatten();
    match c {
        TermColor::Spec(c) => rgb(c),
        TermColor::Indexed(i) => set(usize::from(i)).map_or(Color::Indexed(i), rgb),
        TermColor::Named(n) => {
            if let Some(c) = set(n as usize) {
                return rgb(c);
            }
            let dim = NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize;
            match n as usize {
                i @ 0..=15 => Color::Indexed(i as u8),
                i if dim.contains(&i) => Color::Indexed((i - dim.start()) as u8),
                _ => Color::Default,
            }
        }
    }
}

/// Escapes regex syntax so a search query matches literally.
fn escape_regex(query: &str) -> String {
    let mut out = String::with_capacity(query.len() * 2);
    for c in query.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The search bar's cells and the column of its text cursor.
fn search_bar(search: &Search, cols: usize) -> (Vec<Cell>, usize) {
    let status = match (&search.found, search.query.is_empty()) {
        (_, true) => "Enter: older  Shift+Enter: newer  Esc: close",
        (Some(_), false) => "Enter: older  Shift+Enter: newer",
        (None, false) => "no match",
    };
    let prompt = " Find: ";
    let bar = Cell::new(' ', Color::Default, Color::Default, Flags::INVERSE);
    let mut cells = vec![bar; cols];
    let mut x = 0;
    let mut put = |text: &str, x: &mut usize, flags: Flags| {
        for ch in text.chars() {
            if let Some(cell) = cells.get_mut(*x) {
                *cell = Cell::new(ch, Color::Default, Color::Default, flags);
            }
            *x += 1;
        }
    };
    put(prompt, &mut x, Flags::INVERSE.union(Flags::BOLD));
    // Show the end of a query longer than the bar.
    let room = cols.saturating_sub(prompt.len() + 1);
    let skip = search.query.chars().count().saturating_sub(room);
    let shown: String = search.query.chars().skip(skip).collect();
    put(&shown, &mut x, Flags::INVERSE);
    let cursor_x = x.min(cols - 1);
    let status_x = cols.saturating_sub(status.chars().count() + 1);
    if status_x > x + 2 {
        let mut sx = status_x;
        put(status, &mut sx, Flags::INVERSE);
    }
    (cells, cursor_x)
}
