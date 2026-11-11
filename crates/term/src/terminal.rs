//! The terminal: host output in, grid and replies out.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
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
use crate::highlight::{self, Highlighter};
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
    /// The bar asks for the stop pattern instead of searching.
    stop: bool,
    /// Why the stop pattern typed so far is not valid.
    error: Option<String>,
}

/// Lines added to the history limit while new output is watched for the
/// stop pattern, so that the lines scrolled out can be counted.
const WATCH_BUMP: usize = 512;

/// Stops following the output where a line matches a pattern.
#[derive(Debug)]
struct Stop {
    regex: highlight::Regex,
    cache: highlight::Cache,
    /// The first line not yet looked at, counted from the first line
    /// ever written (see `Terminal::scrolled`).
    scan_from: u64,
    /// The line the last stop was on, so a line that grows is not
    /// stopped at twice.
    last_hit: Option<u64>,
}

/// Where typing on a command line started: from [`Terminal::input_mark`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputMark {
    /// The row, counted from the oldest line kept.
    line: usize,
    col: usize,
    /// The text before the mark on its row.
    prompt: String,
}

impl InputMark {
    /// The text before the mark on its row: the shell's prompt, or a
    /// program's question such as `Password: `.
    pub fn prompt(&self) -> &str {
        &self.prompt
    }
}

/// The command line typed since an [`InputMark`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputLine {
    /// From the mark to the cursor.
    pub before: String,
    /// After the cursor, without a right-hand prompt at the row's end,
    /// trailing blanks dropped.
    pub after: String,
}

/// Where the command line ends on a row that goes on with a right-hand
/// prompt.
const INPUT_GAP: &str = "   ";

/// Blank cells a right-hand prompt may leave at the row's end (zsh leaves
/// one, `ZLE_RPROMPT_INDENT`).
const RIGHT_PROMPT_INDENT: usize = 2;

/// `text` (whole rows, to the right edge) without a right-hand prompt: the
/// last run of text after a gap of [`INPUT_GAP`], when it ends at the
/// right edge. A gap inside a command (`echo "a   b"`) stays.
fn without_right_prompt(text: &str) -> &str {
    let Some(i) = text.rfind(INPUT_GAP) else {
        return text;
    };
    let tail = text.get(i..).unwrap_or("").trim_start();
    let at_edge = text.len() - text.trim_end().len() <= RIGHT_PROMPT_INDENT;
    if tail.is_empty() || !at_edge {
        return text;
    }
    text.get(..i).unwrap_or(text)
}

/// The most rows a command line read by [`Terminal::input_line`] spans.
const MAX_INPUT_ROWS: usize = 16;

/// One terminal: the emulator, its parser and the pane's search state.
pub struct Terminal {
    term: Term<Events>,
    parser: Processor,
    events: Events,
    /// Answers colour queries for colours the host has not set.
    theme: Theme,
    cell_px: (u16, u16),
    search: Option<Search>,
    scrollback: usize,
    highlighter: Option<Rc<Highlighter>>,
    highlight_cache: RefCell<Option<highlight::Cache>>,
    /// Stop patterns from the settings; with the one typed into the stop
    /// bar they make `stop`.
    stop_rules: Vec<String>,
    stop_typed: Option<String>,
    stop: Option<Stop>,
    /// Lines scrolled into the history since the start, while a stop
    /// pattern is set.
    scrolled: u64,
    /// Following the output stopped at a match: the view holds still
    /// until it is scrolled to the bottom or the user types.
    stopped: bool,
    /// Whether the selection is still the match a stop selected.
    stop_selected: bool,
    scroll_scan: ScrollScan,
    cwd: CwdScanner,
    /// Working directories reported since the last `take_actions`.
    reported: Vec<WorkingDir>,
    /// The title the host last set, as the UI was told.
    title: Option<String>,
    /// While the alternate screen is up: the title from before it, and
    /// whether the app on it set another.
    alt_title: Option<(Option<String>, bool)>,
    /// Feeds of host output so far, to tell whether the host answered
    /// something typed.
    fed: u64,
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
            scrollback: settings.scrollback.min(Settings::MAX_SCROLLBACK),
            highlighter: None,
            highlight_cache: RefCell::new(None),
            stop_rules: Vec::new(),
            stop_typed: None,
            stop: None,
            scrolled: 0,
            stopped: false,
            stop_selected: false,
            scroll_scan: ScrollScan::default(),
            cwd: CwdScanner::default(),
            reported: Vec::new(),
            title: None,
            alt_title: None,
            fed: 0,
        }
    }

    pub fn set_settings(&mut self, settings: &Settings) {
        self.scrollback = settings.scrollback.min(Settings::MAX_SCROLLBACK);
        self.term.set_options(config(settings));
    }

    /// The keyword highlighting rules for this pane, or none.
    pub fn set_highlighter(&mut self, highlighter: Option<Rc<Highlighter>>) {
        let same = match (&self.highlighter, &highlighter) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            *self.highlight_cache.borrow_mut() = highlighter.as_ref().map(|h| h.create_cache());
            self.highlighter = highlighter;
        }
    }

    pub fn highlighting(&self) -> bool {
        self.highlighter.is_some()
    }

    /// Stop patterns from the settings (already checked; a pattern that
    /// does not compile is left out).
    pub fn set_stop_rules(&mut self, patterns: Vec<String>) {
        if patterns != self.stop_rules {
            self.stop_rules = patterns;
            self.rebuild_stop();
        }
    }

    /// Sets or clears the pane's own stop pattern.
    pub fn set_stop_pattern(&mut self, pattern: Option<&str>) -> Result<(), String> {
        let pattern = pattern.filter(|p| !p.is_empty());
        if let Some(p) = pattern {
            highlight::check(p)?;
        }
        self.stop_typed = pattern.map(str::to_owned);
        self.rebuild_stop();
        Ok(())
    }

    /// The pane's own stop pattern.
    pub fn stop_pattern(&self) -> Option<&str> {
        self.stop_typed.as_deref()
    }

    /// Whether output is watched for a stop pattern.
    pub fn stop_armed(&self) -> bool {
        self.stop.is_some()
    }

    /// Whether following the output stopped at a match.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    fn rebuild_stop(&mut self) {
        let mut patterns: Vec<&str> = self.stop_typed.iter().map(String::as_str).collect();
        patterns.extend(self.stop_rules.iter().map(String::as_str));
        patterns.truncate(highlight::MAX_RULES);
        let regex = (!patterns.is_empty())
            .then(|| highlight::compile(&patterns).ok())
            .flatten();
        self.stop = regex.map(|regex| {
            let cache = regex.create_cache();
            Stop {
                cache,
                regex,
                scan_from: 0,
                last_hit: None,
            }
        });
        if self.stopped && self.stop_selected {
            // The match selected when it stopped goes with the stop; a
            // selection made since stays.
            self.term.selection = None;
        }
        self.stop_selected = false;
        self.stopped = false;
        self.rescan_from_cursor();
    }

    /// Looks for stops only in output still to come.
    fn rescan_from_cursor(&mut self) {
        let cursor = self.term.grid().cursor.point.line.0.max(0) as u64;
        let from = self.scrolled + cursor;
        if let Some(stop) = &mut self.stop {
            stop.scan_from = from;
            stop.last_hit = None;
        }
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
            self.drop_lone_spacers();
            self.rescan_from_cursor();
            if let Some(s) = &mut self.search {
                s.found = None;
            }
        }
    }

    /// Processes output from the host. Call [`Self::take_actions`] after.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.fed = self.fed.wrapping_add(1);
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
                if self.stop.is_some() {
                    let ends = watch_parts(piece, self.cols(), self.rows(), &mut self.scroll_scan);
                    let mut from = 0;
                    for to in ends {
                        if let Some(part) = piece.get(from..to).filter(|p| !p.is_empty()) {
                            self.watched(|t| t.parser.advance(&mut t.term, part));
                        }
                        from = to;
                    }
                } else {
                    self.parser.advance(&mut self.term, piece);
                }
                self.repair();
                self.follow_title(queued);
            }
            start = end;
        }
    }

    /// Puts right what alacritty_terminal 0.26 can leave wrong after
    /// processing output, before anything draws or reads the grid. Every
    /// path that hands it bytes calls this: [`Self::feed`], and
    /// [`Self::flush_sync`] for a synchronized update's buffered bytes.
    fn repair(&mut self) {
        self.clamp_view();
        self.drop_lone_spacers();
    }

    /// Keeps a scrolled-back view inside the history. alacritty_terminal
    /// 0.26 moves the view back a line for every line scrolled out of a
    /// region that does not start at the top of the screen (delete lines
    /// below the cursor, a scroll region under a status line), although
    /// none of those lines go into history. The view then reaches past
    /// the oldest line and drawing it panics. Scrolling by nothing clamps
    /// the view to the history there is.
    fn clamp_view(&mut self) {
        let grid = self.term.grid();
        if grid.display_offset() > grid.history_size() {
            self.term.scroll_display(Scroll::Delta(0));
        }
    }

    /// Clears wide-character spacers left at the start of a screen line.
    /// alacritty_terminal 0.26 can leave one there (deleting characters
    /// with CSI P moves the spacer of a wide character without the
    /// character), and with the cursor on it, finding the cursor's cell
    /// subtracts one from column 0. A spacer with nothing before it is
    /// a blank cell anyway.
    fn drop_lone_spacers(&mut self) {
        let grid = self.term.grid_mut();
        for line in 0..grid.screen_lines() {
            let cell = &mut grid[Line(line as i32)][Column(0)];
            cell.flags.remove(TermFlags::WIDE_CHAR_SPACER);
        }
    }

    /// Makes wide characters and their spacers agree again, over the whole
    /// scrollback: a wide character must have its spacer in the next cell,
    /// a spacer must follow a wide character, and a spacer saying a wide
    /// character goes on at the start of the next line belongs only at the
    /// end of a line whose next line starts with one. alacritty_terminal
    /// 0.26 breaks these: deleting characters can move a spacer without
    /// its character (to column 0 too, and a line can reach history before
    /// `feed` clears that, see [`Self::drop_lone_spacers`]), a resize can
    /// leave the end-of-line spacer mid-line or a wide character without
    /// its spacer, and a character can be overwritten since. Copying text
    /// from a spacer in column 0 steps back to column -1 and panics, and
    /// search loops for ever on the others. Run before each copy and
    /// search, which are rare.
    fn drop_stray_spacers(&mut self) {
        let (top, bottom) = (self.term.topmost_line(), self.term.bottommost_line());
        let grid = self.term.grid_mut();
        let last = grid.last_column().0;
        // From the bottom up, so each line's next line is already fixed.
        for line in (top.0..=bottom.0).rev() {
            let continued = line < bottom.0
                && grid[Line(line + 1)][Column(0)]
                    .flags
                    .contains(TermFlags::WIDE_CHAR);
            let row = &mut grid[Line(line)];
            for col in 0..=last {
                let spacer_next = col < last
                    && row[Column(col + 1)]
                        .flags
                        .contains(TermFlags::WIDE_CHAR_SPACER);
                if !spacer_next {
                    row[Column(col)].flags.remove(TermFlags::WIDE_CHAR);
                }
                let wide_before =
                    col > 0 && row[Column(col - 1)].flags.contains(TermFlags::WIDE_CHAR);
                let flags = &mut row[Column(col)].flags;
                if !wide_before {
                    flags.remove(TermFlags::WIDE_CHAR_SPACER);
                }
                if col < last || !continued {
                    flags.remove(TermFlags::LEADING_WIDE_CHAR_SPACER);
                }
            }
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
        if self.stop.is_some() {
            self.watched(|t| t.parser.stop_sync(&mut t.term));
        } else {
            self.parser.stop_sync(&mut self.term);
        }
        self.repair();
    }

    /// Runs `process` (which hands output to the emulator) while counting
    /// the lines it scrolls into the history, then holds a stopped view
    /// still and looks at the new lines for the stop pattern.
    ///
    /// alacritty_terminal keeps a scrolled-back view in place by itself,
    /// but at the bottom it follows, and once the history is full lines
    /// leave it as fast as they come, so its size says nothing. For the
    /// time of `process` the history may grow by [`WATCH_BUMP`] lines and
    /// a screen more, which [`watch_parts`] keeps it under; the growth is the
    /// count. The limit goes back after.
    fn watched(&mut self, process: impl FnOnce(&mut Self)) {
        let alt = self.mode().contains(TermMode::ALT_SCREEN);
        if alt {
            process(self);
            return;
        }
        let before = self.term.history_size();
        let offset = self.display_offset();
        let bumped = self.scrollback.saturating_add(WATCH_BUMP + self.rows());
        self.term.grid_mut().update_history(bumped);
        process(self);
        if self.mode().contains(TermMode::ALT_SCREEN) {
            // The main screen went away mid-way; set its limit back
            // through the options (which also say the title again).
            let queued = self.events.0.borrow().len();
            self.term
                .set_options(config(&Settings::new(self.scrollback)));
            self.events.0.borrow_mut().truncate(queued);
            return;
        }
        let after = self.term.history_size();
        if after < before {
            // The history was cleared (CSI 3 J): start again from here.
            self.term.grid_mut().update_history(self.scrollback);
            self.rescan_from_cursor();
            return;
        }
        let added = after - before;
        self.scrolled += added as u64;
        if self.stopped && offset == 0 && added > 0 {
            // The view still follows until the line stopped at reaches
            // the middle row, then holds it there.
            let middle = (self.rows() as i64 - 1) / 2;
            match self.stop.as_ref().and_then(|s| s.last_hit) {
                Some(hit) => {
                    let line = hit as i64 - self.scrolled as i64;
                    if line < middle {
                        self.centre_line(Line(line.max(i64::from(i32::MIN)) as i32));
                    }
                }
                None => self.term.scroll_display(Scroll::Delta(added as i32)),
            }
        }
        if self.stopped {
            // Held still above; nothing to look for until it follows.
        } else if offset == 0 {
            self.scan_for_stop();
        } else {
            // Scrolled back by hand, the user is not following the
            // output anyway: the view does not jump to a match.
            let hit = self.stop.as_ref().and_then(|s| s.last_hit);
            self.rescan_from_cursor();
            if let Some(stop) = &mut self.stop {
                stop.last_hit = hit;
            }
        }
        self.term.grid_mut().update_history(self.scrollback);
    }

    /// Looks at the lines written since the last look, up to the cursor's,
    /// and stops following at the first that matches.
    fn scan_for_stop(&mut self) {
        let Some(stop) = &mut self.stop else {
            return;
        };
        let history = self.term.history_size() as u64;
        let cursor = self.term.grid().cursor.point.line.0.max(0) as u64;
        let oldest = self.scrolled.saturating_sub(history);
        let last = self.scrolled + cursor;
        let mut abs = stop.scan_from.max(oldest);
        let cols = self.term.columns();
        let mut cells = Vec::with_capacity(cols);
        let mut hit = None;
        while abs <= last {
            if stop.last_hit != Some(abs) {
                let line = Line((abs as i64 - self.scrolled as i64) as i32);
                let row = &self.term.grid()[line];
                cells.clear();
                cells.extend((0..cols).map(|x| plain_cell(&row[Column(x)])));
                if let Some(cols) = highlight::find_in(&stop.regex, &mut stop.cache, &cells) {
                    hit = Some((abs, line, cols));
                    break;
                }
            }
            abs += 1;
        }
        // The cursor's line may still grow: look at it again next time.
        stop.scan_from = last;
        let Some((abs, line, (first, end))) = hit else {
            return;
        };
        stop.last_hit = Some(abs);
        stop.scan_from = abs;
        self.stopped = true;
        let mut sel = Selection::new(
            SelectionType::Simple,
            Point::new(line, Column(first)),
            Side::Left,
        );
        sel.update(Point::new(line, Column(end)), Side::Right);
        self.term.selection = Some(sel);
        self.stop_selected = true;
        self.centre_line(line);
    }

    /// Scrolls so that `line` is on the middle row of the pane, where the
    /// eye finds it first, or as near as the scrollback allows.
    fn centre_line(&mut self, line: Line) {
        let rows = self.rows() as i32;
        let offset = self.display_offset() as i32;
        let history = self.term.history_size() as i32;
        let want = ((rows - 1) / 2 - line.0).clamp(0, history);
        self.term.scroll_display(Scroll::Delta(want - offset));
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
        self.follow_at_bottom(up);
        None
    }

    /// Following the output starts again once the view is scrolled down
    /// to the bottom.
    fn follow_at_bottom(&mut self, up: bool) {
        if !up && self.display_offset() == 0 && self.stopped {
            self.stopped = false;
            if self.stop_selected {
                self.term.selection = None;
            }
            self.stop_selected = false;
            // The line stopped at may still be the cursor's: it is not
            // stopped at again.
            let hit = self.stop.as_ref().and_then(|s| s.last_hit);
            self.rescan_from_cursor();
            if let Some(stop) = &mut self.stop {
                stop.last_hit = hit;
            }
        }
    }

    /// Scrolls the view by lines (positive moves back in history).
    pub fn scroll_lines(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
        self.follow_at_bottom(lines > 0);
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
        self.follow_at_bottom(pages > 0);
    }

    pub fn scroll_to_top(&mut self) {
        self.term.scroll_display(Scroll::Top);
    }

    /// Goes to the bottom and follows the output again.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.follow_at_bottom(false);
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
        self.stopped = false;
        self.rescan_from_cursor();
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
        self.stop_selected = false;
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
        self.stop_selected = false;
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// The selected text, built the way alacritty_terminal 0.26 builds it
    /// in `selection_to_string`, around three of its bugs that panic.
    pub fn selection_text(&mut self) -> Option<String> {
        self.term.selection.as_ref()?;
        self.drop_stray_spacers();
        let selection = self.term.selection.as_ref()?;
        let SelectionRange { start, end, .. } = selection.to_range(&self.term)?;
        let text = match selection.ty {
            SelectionType::Block => {
                // A block selection that starts on the right half of a cell
                // drops that cell, so one started on the right half of the
                // last column and ended left of it covers no cell; upstream
                // still reads that cell, one past the end of the row.
                if start.column > end.column {
                    return None;
                }
                (start.line.0..=end.line.0)
                    .map(|line| {
                        let wide = line == end.line.0 || start.column.0 != 0;
                        let (from, to) = (
                            Point::new(Line(line), start.column),
                            Point::new(Line(line), end.column),
                        );
                        self.bounds_text(from, to, wide).trim_end().to_owned()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            SelectionType::Lines => self.bounds_text(start, end, true) + "\n",
            _ => self.bounds_text(start, end, true),
        };
        Some(text).filter(|s| !s.is_empty())
    }

    /// `Term::bounds_to_string`, except where the selection ends on the
    /// spacer left at the end of a line by a wide character that did not
    /// fit there. Upstream then adds the first character of the line above
    /// instead of the line below, and panics on the oldest line of history,
    /// which has none above it. Here that spacer is left out and, with
    /// `wide`, the wide character from the line below is added.
    fn bounds_text(&self, start: Point, end: Point, wide: bool) -> String {
        let grid = self.term.grid();
        let last = grid.last_column();
        let spacer = end.column == last
            && last.0 >= 1
            && grid[end.line][last]
                .flags
                .contains(TermFlags::LEADING_WIDE_CHAR_SPACER);
        if !spacer {
            return self.term.bounds_to_string(start, end);
        }
        let before = Point::new(end.line, last - 1);
        let mut text = if start <= before {
            self.term.bounds_to_string(start, before)
        } else {
            String::new()
        };
        if wide && end.line < grid.bottommost_line() {
            text.push(grid[end.line + 1i32][Column(0)].c);
        }
        text
    }

    /// Everything the pane holds as plain text, oldest line first: the
    /// scrollback and the screen. Lines the terminal wrapped are joined
    /// again, trailing blanks are dropped from each line, and the empty
    /// lines under the last one with text are left out. While a full-screen
    /// program shows the alternate screen, that screen is what is saved.
    /// Read cell by cell, so none of the upstream copy bugs can panic.
    pub fn all_text(&self) -> Vec<String> {
        let grid = self.term.grid();
        let (top, bottom) = (self.term.topmost_line(), self.term.bottommost_line());
        let last = grid.last_column();
        let mut lines = Vec::new();
        let mut text = String::new();
        for line in top.0..=bottom.0 {
            let row = &grid[Line(line)];
            for col in 0..=last.0 {
                let cell = &row[Column(col)];
                if cell
                    .flags
                    .intersects(TermFlags::WIDE_CHAR_SPACER | TermFlags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                text.push(if cell.c == '\t' { ' ' } else { cell.c });
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra);
                }
            }
            if !row[last].flags.contains(TermFlags::WRAPLINE) {
                lines.push(std::mem::take(&mut text).trim_end().to_owned());
            }
        }
        if !text.is_empty() {
            lines.push(text.trim_end().to_owned());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }

    /// The text before the cursor on its line, when the main screen is
    /// shown (full-screen programs draw no prompt). Shells that do not
    /// report their directory with OSC 7 often show it in the prompt;
    /// [`crate::prompt_dir`] reads it from this.
    pub fn prompt_line(&self) -> Option<String> {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let grid = self.term.grid();
        let at = grid.cursor.point;
        let row = &grid[at.line];
        let cols = at.column.0.min(grid.columns());
        let mut text = String::with_capacity(cols);
        for c in 0..cols {
            let cell = &row[Column(c)];
            if cell
                .flags
                .intersects(TermFlags::WIDE_CHAR_SPACER | TermFlags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(cell.c);
        }
        Some(text)
    }

    /// Where typing on the command line starts: the cursor, with the
    /// text before it on its row (the prompt). `None` while a full-screen
    /// program shows the alternate screen. Read the line typed since with
    /// [`Self::input_line`].
    pub fn input_mark(&self) -> Option<InputMark> {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let grid = self.term.grid();
        let at = grid.cursor.point;
        let col = at.column.0.min(grid.columns());
        Some(InputMark {
            line: self.absolute(at.line)?,
            col,
            prompt: self.row_text(at.line, 0, col),
        })
    }

    /// The command line typed since `mark`: the text from the mark to the
    /// cursor, rows the line wrapped onto included, and what follows the
    /// cursor on it. When output moved the mark's row (Ctrl+L, a redrawn
    /// prompt), the mark follows a row that starts with the same prompt
    /// on the cursor's line. `None` on the alternate screen, or when the
    /// cursor is no longer on the mark's line.
    pub fn input_line(&self, mark: &mut InputMark) -> Option<InputLine> {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let cols = grid.columns();
        // Rows the cursor's line wrapped from, nearest first.
        let mut starts = vec![cursor.line];
        let mut l = cursor.line;
        while l > grid.topmost_line()
            && grid[l - 1i32][Column(cols - 1)]
                .flags
                .contains(TermFlags::WRAPLINE)
            && starts.len() < MAX_INPUT_ROWS
        {
            l -= 1i32;
            starts.push(l);
        }
        let kept = self
            .relative(mark.line)
            .filter(|l| self.row_text(*l, 0, mark.col) == mark.prompt);
        let start = match kept {
            Some(l) if starts.contains(&l) => l,
            // The prompt was drawn again elsewhere (Ctrl+L): find it on
            // the cursor's line.
            _ if mark.prompt.trim().is_empty() => return None,
            _ => starts
                .iter()
                .copied()
                .find(|l| self.row_text(*l, 0, mark.col) == mark.prompt)?,
        };
        mark.line = self.absolute(start)?;
        if start == cursor.line && cursor.column.0 < mark.col {
            return None;
        }
        // The cursor's column; past the last one when the next character
        // wraps.
        let cursor_col = if grid.cursor.input_needs_wrap {
            cols
        } else {
            cursor.column.0
        };
        let mut before = String::new();
        let mut line = start;
        let mut from = mark.col;
        while line < cursor.line {
            before.push_str(&self.row_text(line, from, cols));
            from = 0;
            line += 1i32;
        }
        before.push_str(&self.row_text(cursor.line, from, cursor_col));
        // What follows the cursor, up to a wide gap (a right-hand prompt).
        let mut rest = self.row_text(cursor.line, cursor_col, cols);
        let mut l = cursor.line;
        while l < grid.bottommost_line()
            && grid[l][Column(cols - 1)]
                .flags
                .contains(TermFlags::WRAPLINE)
        {
            l += 1i32;
            rest.push_str(&self.row_text(l, 0, cols));
        }
        Some(InputLine {
            before,
            after: without_right_prompt(&rest).trim_end().to_owned(),
        })
    }

    /// How many times host output was fed in: a change means the host
    /// answered (echoed) what was typed before.
    pub fn output_count(&self) -> u64 {
        self.fed
    }

    /// Whether the cursor waits at the end of a full row: the next
    /// character goes on the next row, not where the cursor is drawn.
    pub fn wrap_pending(&self) -> bool {
        self.term.grid().cursor.input_needs_wrap
    }

    /// Whether the cursor is still on the line `mark` starts, where it
    /// started (no looking for the prompt elsewhere): after Enter, whether
    /// the host has answered yet.
    pub fn input_here(&self, mark: &InputMark) -> bool {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        let grid = self.term.grid();
        let Some(start) = self
            .relative(mark.line)
            .filter(|l| self.row_text(*l, 0, mark.col) == mark.prompt)
        else {
            return false;
        };
        let cols = grid.columns();
        let mut l = grid.cursor.point.line;
        for _ in 0..MAX_INPUT_ROWS {
            if l == start {
                return true;
            }
            if l <= grid.topmost_line()
                || !grid[l - 1i32][Column(cols - 1)]
                    .flags
                    .contains(TermFlags::WRAPLINE)
            {
                return false;
            }
            l -= 1i32;
        }
        false
    }

    /// The whole command line at `mark`, wherever the cursor is: from the
    /// mark to the end of the line (rows it wrapped onto included), up to
    /// a right-hand prompt. For reading a line once Enter moved past it.
    /// `None` on the alternate screen or when the mark's row no longer
    /// starts with its prompt.
    pub fn input_text(&self, mark: &InputMark) -> Option<String> {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let grid = self.term.grid();
        let cols = grid.columns();
        let mut line = self.relative(mark.line)?;
        if self.row_text(line, 0, mark.col) != mark.prompt {
            return None;
        }
        let mut text = self.row_text(line, mark.col, cols);
        let mut rows = 1;
        while line < grid.bottommost_line()
            && rows < MAX_INPUT_ROWS
            && grid[line][Column(cols - 1)]
                .flags
                .contains(TermFlags::WRAPLINE)
        {
            line += 1i32;
            rows += 1;
            text.push_str(&self.row_text(line, 0, cols));
        }
        Some(without_right_prompt(&text).trim_end().to_owned())
    }

    /// The text of grid line `line` from column `from` to before `to`,
    /// wide-character spacers left out, tabs as spaces.
    fn row_text(&self, line: Line, from: usize, to: usize) -> String {
        let grid = self.term.grid();
        if line < grid.topmost_line() || line > grid.bottommost_line() {
            return String::new();
        }
        let row = &grid[line];
        let to = to.min(grid.columns());
        let mut text = String::new();
        for c in from..to {
            let cell = &row[Column(c)];
            if cell
                .flags
                .intersects(TermFlags::WIDE_CHAR_SPACER | TermFlags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(if cell.c == '\t' || cell.c.is_control() {
                ' '
            } else {
                cell.c
            });
            if let Some(extra) = cell.zerowidth() {
                text.extend(extra);
            }
        }
        text
    }

    /// A grid line counted from the oldest line kept, which stays put as
    /// output scrolls (until the history is full).
    fn absolute(&self, line: Line) -> Option<usize> {
        usize::try_from(i64::from(line.0) + self.term.grid().history_size() as i64).ok()
    }

    /// The grid line of an absolute line, if it is still kept.
    fn relative(&self, line: usize) -> Option<Line> {
        let grid = self.term.grid();
        let l = i64::try_from(line).ok()? - grid.history_size() as i64;
        let l = Line(i32::try_from(l).ok()?);
        (l >= grid.topmost_line() && l <= grid.bottommost_line()).then_some(l)
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
        if self.search.as_ref().is_some_and(|s| s.stop) {
            self.search = None;
        }
        self.search.get_or_insert_with(Search::default);
    }

    /// Opens the bar that asks for the stop pattern, holding the current
    /// one.
    pub fn stop_bar_open(&mut self) {
        self.search = Some(Search {
            query: self.stop_typed.clone().unwrap_or_default(),
            stop: true,
            ..Search::default()
        });
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
        if search.stop {
            search.query = query.to_owned();
            search.error = if query.is_empty() {
                None
            } else {
                highlight::check(query).err()
            };
            return;
        }
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
        if let Some(search) = self.search.as_ref().filter(|s| s.stop) {
            // Enter in the stop bar sets the pattern and closes it.
            if search.error.is_none() {
                let query = search.query.clone();
                if self.set_stop_pattern(Some(&query)).is_ok() {
                    self.search = None;
                }
            }
            return;
        }
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
        if self.search.is_none() {
            return;
        }
        self.drop_stray_spacers();
        let Some(search) = &mut self.search else {
            return;
        };
        let found = search
            .regex
            .as_mut()
            .and_then(|regex| self.term.search_next(regex, origin, direction, side, None));
        search.found = found.clone();
        self.stop_selected = false;
        match found {
            Some(m) => {
                let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                sel.update(*m.end(), Side::Right);
                self.term.selection = Some(sel);
                self.centre_line(m.start().line);
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

        // Full-screen programs draw their own screens: no highlighting
        // there.
        let alt = self.mode().contains(TermMode::ALT_SCREEN);
        let mut cache = self.highlight_cache.borrow_mut();
        let mut highlight = self
            .highlighter
            .as_deref()
            .zip(cache.as_mut())
            .filter(|_| !alt);
        let mut row_cells = Vec::with_capacity(cols);
        for y in 0..rows {
            row_cells.clear();
            let line = Line(y as i32 - offset as i32);
            let row = &self.term.grid()[line];
            for x in 0..cols {
                let cell = &row[Column(x)];
                let mut out = convert_cell(cell, colors);
                if let Some(marks) = cell.zerowidth() {
                    out.cluster = grid.add_cluster(&marks.iter().collect::<String>());
                }
                row_cells.push(out);
            }
            if let Some((h, cache)) = &mut highlight {
                h.apply(cache, &mut row_cells);
            }
            if let Some(s) = selection {
                for (x, out) in row_cells.iter_mut().enumerate() {
                    if s.contains(Point::new(line, Column(x))) {
                        toggle(&mut out.flags, Flags::INVERSE);
                    }
                }
            }
            if y == 0 && self.stopped && self.search.is_none() {
                stopped_badge(&mut row_cells);
            }
            grid.set_row(y, row_cells.iter().copied());
        }
        drop(cache);

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

/// A cell's character and width, for matching its text.
fn plain_cell(cell: &alacritty_terminal::term::cell::Cell) -> Cell {
    let mut flags = Flags::empty();
    if cell.flags.contains(TermFlags::WIDE_CHAR_SPACER) {
        flags.insert(Flags::WIDE_SPACER);
    }
    if cell.flags.contains(TermFlags::WIDE_CHAR) {
        flags.insert(Flags::WIDE);
    }
    let ch = if cell.c.is_control() { ' ' } else { cell.c };
    Cell::new(ch, Color::Default, Color::Default, flags)
}

/// Where the output is in an escape sequence, as far as counting the
/// lines it can scroll goes. Kept between feeds, since a sequence can be
/// split across them.
#[derive(Clone, Copy, Debug, Default)]
enum ScrollScan {
    #[default]
    Ground,
    Esc,
    /// In a control sequence: its first number, and whether it is still
    /// a plain one (no private marker or intermediate byte).
    Csi {
        param: usize,
        first: bool,
        plain: bool,
    },
}

impl ScrollScan {
    /// How many lines `b` may scroll into the history, at most: one for a
    /// line feed, vertical tab, form feed, index or next line, the count
    /// of a scroll up (CSI n S, at most a screen). Wrapping is counted by
    /// the caller.
    fn cost(&mut self, b: u8, rows: usize) -> usize {
        match (*self, b) {
            // C0 controls run even inside a sequence; CAN and SUB end it.
            (_, b'\n' | 0x0B | 0x0C) => 1,
            (_, 0x18 | 0x1A) => {
                *self = Self::Ground;
                0
            }
            (_, 0x1B) => {
                *self = Self::Esc;
                0
            }
            (Self::Ground, _) => 0,
            (Self::Esc, b'[') => {
                *self = Self::Csi {
                    param: 0,
                    first: true,
                    plain: true,
                };
                0
            }
            (Self::Esc, b'D' | b'E') => {
                *self = Self::Ground;
                1
            }
            (Self::Esc, _) => {
                *self = Self::Ground;
                0
            }
            (
                Self::Csi {
                    param,
                    first,
                    plain,
                },
                _,
            ) => match b {
                b'0'..=b'9' if first => {
                    let param = (param * 10 + usize::from(b - b'0')).min(1 << 20);
                    *self = Self::Csi {
                        param,
                        first,
                        plain,
                    };
                    0
                }
                b';' | b':' => {
                    *self = Self::Csi {
                        param,
                        first: false,
                        plain,
                    };
                    0
                }
                0x20..=0x2F | 0x3C..=0x3F => {
                    *self = Self::Csi {
                        param,
                        first,
                        plain: false,
                    };
                    0
                }
                0x40..=0x7E => {
                    *self = Self::Ground;
                    if b == b'S' && plain {
                        param.clamp(1, rows.max(1))
                    } else {
                        0
                    }
                }
                _ => 0,
            },
        }
    }
}

/// Where to split output so that no part can scroll more than
/// [`WATCH_BUMP`] lines plus a screen into the history (see
/// [`ScrollScan::cost`]; every screen width of other bytes may wrap one
/// more line). Returns the end of each part. Splitting mid-sequence is
/// fine: the parser keeps its state between parts.
fn watch_parts(bytes: &[u8], cols: usize, rows: usize, scan: &mut ScrollScan) -> Vec<usize> {
    let cols = cols.max(1);
    let mut ends = Vec::new();
    let mut spent = 0;
    let mut run = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let mut cost = scan.cost(b, rows);
        if cost == 0 {
            run += 1;
            if run % cols == 0 {
                cost = 1;
            }
        }
        if spent > 0 && spent + cost > WATCH_BUMP {
            ends.push(i);
            spent = 0;
        }
        spent += cost;
    }
    ends.push(bytes.len());
    ends
}

/// Marks the top right corner while following the output is stopped at
/// a match.
fn stopped_badge(cells: &mut [Cell]) {
    let text = " Stopped at a match \u{b7} End follows ";
    let width = text.chars().count();
    let start = cells.len().saturating_sub(width);
    let flags = Flags::INVERSE.union(Flags::BOLD);
    for (cell, ch) in cells.iter_mut().skip(start).zip(text.chars()) {
        *cell = Cell::new(ch, Color::Default, Color::Default, flags);
    }
    // A wide character cut in half by the badge loses its right half.
    if let Some(cell) = start.checked_sub(1).and_then(|x| cells.get_mut(x))
        && cell.flags.contains(Flags::WIDE)
    {
        *cell = Cell::BLANK;
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
        _ if search.stop => match (&search.error, search.query.is_empty()) {
            (Some(_), _) => "not a valid pattern",
            (None, true) => "Enter: stop at nothing  Esc: close",
            (None, false) => "Enter: stop following at a match",
        },
        (_, true) => "Enter: older  Shift+Enter: newer  Esc: close",
        (Some(_), false) => "Enter: older  Shift+Enter: newer",
        (None, false) => "no match",
    };
    let prompt = if search.stop { " Stop at: " } else { " Find: " };
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
