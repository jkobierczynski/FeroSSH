//! Shell integration: the OSC 133 marks a shell sends around each prompt
//! and command (`A` prompt, `B` typing, `C` output, `D;code` done).
//!
//! The marks are kept in the grid itself, so they scroll, reflow on a
//! resize and leave with the scrollback like the text they belong to: the
//! first free cell of the row a mark is about (one of the first
//! [`MARK_COLS`]) carries an OSC 8 link with [`MARK_ID`], whose target
//! lists the row's marks. A cell the shell writes over loses them, as the
//! text it described is gone too. Exit codes and times live in a table
//! here, by command number.
//!
//! Each mark is put on a row that will not be written again: the row
//! above the cursor when the cursor is at the start of a line (the last
//! line of the previous output, the last line of the command), or the
//! text left of the cursor otherwise.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Hyperlink;

use super::{Terminal, max_input_rows};
use crate::cwd::Mark;
use crate::sanitize::sanitize_title;

/// The link id of the cells that carry marks. It starts with BEL, which
/// no OSC 8 sequence can hold, so a host cannot make one.
pub(crate) const MARK_ID: &str = "\x07ferossh-command";
/// Marks go into one of a row's first cells.
const MARK_COLS: usize = 8;
/// The longest list of marks a cell keeps; older ones go first.
const MAX_MARKS_LEN: usize = 160;
/// Commands remembered; their marks have long left the scrollback.
const MAX_RECORDS: usize = 10_000;
/// The longest command line kept, in characters.
const MAX_COMMAND: usize = 1024;

/// What the shell's marks say so far.
#[derive(Debug, Default)]
pub(super) struct Commands {
    /// The next command's number.
    next: u32,
    /// The command whose prompt shows: its number.
    current: Option<u32>,
    /// The command that runs (`C` without `D` yet).
    running: Option<u32>,
    /// Commands that ran, by number.
    records: BTreeMap<u32, Record>,
    /// Commands that finished since the last `take_actions`.
    pub finished: Vec<CommandEnd>,
}

#[derive(Clone, Debug)]
struct Record {
    started: Instant,
    ended: Option<Instant>,
    exit: Option<i32>,
    command: Option<String>,
}

/// A command the shell said finished (OSC 133 `D`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandEnd {
    /// Its exit code, when the shell gave one.
    pub exit: Option<i32>,
    /// How long it ran.
    pub took: Duration,
    /// The command line, when the shell marked where typing started;
    /// sanitised like a title.
    pub command: Option<String>,
}

/// The last command the shell ran, from its OSC 133 marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandStatus {
    /// It still runs.
    pub running: bool,
    /// Its exit code, once finished, when the shell gave one.
    pub exit: Option<i32>,
    /// When it started, or finished.
    pub since: Instant,
    pub command: Option<String>,
}

impl CommandStatus {
    /// It finished with an exit code other than 0.
    pub fn failed(&self) -> bool {
        !self.running && self.exit.is_some_and(|c| c != 0)
    }
}

/// One mark on a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    /// The prompt of command `n` starts on the next row (this row ends the
    /// output before it).
    PromptNext(u32),
    /// The prompt of command `n` starts on this row, after the output
    /// before it.
    PromptHere(u32),
    /// Typing of command `n` starts at this column.
    Input(u32, usize),
    /// Command `n`'s line ends on this row; its output starts on the next.
    OutputNext(u32),
    /// Command `n`'s output starts on this row, after its line.
    OutputHere(u32),
}

impl Token {
    fn write(self) -> String {
        match self {
            Token::PromptNext(n) => format!("a{n}"),
            Token::PromptHere(n) => format!("p{n}"),
            Token::Input(n, col) => format!("b{n}:{col}"),
            Token::OutputNext(n) => format!("c{n}"),
            Token::OutputHere(n) => format!("o{n}"),
        }
    }

    fn read(text: &str) -> Option<Token> {
        let (kind, rest) = text.split_at_checked(1)?;
        if kind == "b" {
            let (n, col) = rest.split_once(':')?;
            return Some(Token::Input(n.parse().ok()?, col.parse().ok()?));
        }
        let n = rest.parse().ok()?;
        Some(match kind {
            "a" => Token::PromptNext(n),
            "p" => Token::PromptHere(n),
            "c" => Token::OutputNext(n),
            "o" => Token::OutputHere(n),
            _ => return None,
        })
    }
}

/// A command as the marks in the grid lay it out, in grid lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    n: u32,
    /// The prompt's first row.
    prompt: Line,
    /// Where typing started, if marked.
    input: Option<Point>,
    /// The first row of its output, once it ran.
    output: Option<Line>,
    /// The last row of its output, once the next prompt came.
    end: Option<Line>,
}

impl Terminal {
    /// Acts on an OSC 133 mark, with the bytes before it on the screen.
    pub(super) fn command_mark(&mut self, mark: Mark) {
        // A synchronized update holds bytes back: the cursor must be
        // where the shell put it.
        if self.sync_deadline().is_some() {
            self.flush_sync();
        }
        // A full-screen program's screen is not the shell's: marks there
        // (from a shell it runs, such as tmux run by hand) are not ours.
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let now = Instant::now();
        let cursor = self.term.grid().cursor.point;
        match mark {
            Mark::Prompt => {
                // A command that never said it finished ends at its next
                // prompt.
                self.end_running(None, now);
                let n = self.commands.new_number();
                self.commands.current = Some(n);
                if cursor.column.0 == 0 {
                    if cursor.line > self.term.topmost_line() {
                        self.put_mark(cursor.line - 1, Token::PromptNext(n), usize::MAX);
                    }
                } else {
                    self.put_mark(cursor.line, Token::PromptHere(n), cursor.column.0);
                }
            }
            Mark::Input => {
                let n = self.commands.current_number();
                if cursor.column.0 > 0 {
                    self.put_mark(
                        cursor.line,
                        Token::Input(n, cursor.column.0),
                        cursor.column.0,
                    );
                }
            }
            Mark::Output => {
                self.end_running(None, now);
                let mut n = self.commands.current_number();
                if self.commands.records.contains_key(&n) {
                    // A second `C` for one prompt: a new command.
                    n = self.commands.new_number();
                    self.commands.current = Some(n);
                }
                let command = self.command_text(n, cursor);
                if cursor.column.0 == 0 {
                    if cursor.line > self.term.topmost_line() {
                        self.put_mark(cursor.line - 1, Token::OutputNext(n), usize::MAX);
                    }
                } else {
                    self.put_mark(cursor.line, Token::OutputHere(n), cursor.column.0);
                }
                self.commands.records.insert(
                    n,
                    Record {
                        started: now,
                        ended: None,
                        exit: None,
                        command,
                    },
                );
                while self.commands.records.len() > MAX_RECORDS {
                    self.commands.records.pop_first();
                }
                self.commands.running = Some(n);
            }
            Mark::Done(exit) => self.end_running(exit, now),
        }
    }

    /// The running command finished, with `exit`.
    fn end_running(&mut self, exit: Option<i32>, now: Instant) {
        let Some(n) = self.commands.running.take() else {
            return;
        };
        let Some(r) = self.commands.records.get_mut(&n) else {
            return;
        };
        r.ended = Some(now);
        r.exit = exit;
        if self.commands.finished.len() < super::MAX_NOTIFICATIONS {
            self.commands.finished.push(CommandEnd {
                exit,
                took: now.saturating_duration_since(r.started),
                command: r.command.clone(),
            });
        }
    }

    /// Adds `token` to the marks of row `line`, in a cell left of column
    /// `before` (the cursor, whose cell the shell still writes).
    fn put_mark(&mut self, line: Line, token: Token, before: usize) {
        let grid = self.term.grid_mut();
        if line < grid.topmost_line() || line > grid.bottommost_line() {
            return;
        }
        let cols = grid.columns().min(MARK_COLS).min(before);
        let row = &mut grid[line];
        let mut free = None;
        for c in 0..cols {
            let cell = &mut row[Column(c)];
            match cell.hyperlink() {
                Some(h) if h.id() == MARK_ID => {
                    let mut list = format!("{} {}", h.uri(), token.write());
                    while list.len() > MAX_MARKS_LEN {
                        match list.split_once(' ') {
                            Some((_, rest)) => list = rest.to_owned(),
                            None => break,
                        }
                    }
                    cell.set_hyperlink(Some(Hyperlink::new(Some(MARK_ID), list)));
                    return;
                }
                None if free.is_none() => free = Some(c),
                _ => {}
            }
        }
        if let Some(c) = free {
            row[Column(c)].set_hyperlink(Some(Hyperlink::new(Some(MARK_ID), token.write())));
        }
    }

    /// The marks of row `line`.
    fn row_marks(&self, line: Line) -> Vec<Token> {
        let grid = self.term.grid();
        let row = &grid[line];
        let mut out = Vec::new();
        for c in 0..grid.columns().min(MARK_COLS) {
            if let Some(h) = row[Column(c)].hyperlink().filter(|h| h.id() == MARK_ID) {
                out.extend(h.uri().split(' ').filter_map(Token::read));
            }
        }
        out
    }

    /// The line of command `n` typed so far: from its input mark, on the
    /// rows just above `cursor`, to the cursor.
    fn command_text(&self, n: u32, cursor: Point) -> Option<String> {
        let grid = self.term.grid();
        let rows = max_input_rows(grid.columns()) as i32 + 2;
        let top = grid.topmost_line().0.max(cursor.line.0 - rows);
        let start = (top..=cursor.line.0).rev().find_map(|l| {
            self.row_marks(Line(l)).into_iter().find_map(|t| match t {
                Token::Input(m, col) if m == n => Some(Point::new(Line(l), Column(col))),
                _ => None,
            })
        })?;
        let mut text = String::new();
        let mut line = start.line;
        let mut from = start.column.0;
        while line <= cursor.line {
            let to = if line == cursor.line {
                cursor.column.0
            } else {
                grid.columns()
            };
            let part = self.row_text(line, from, to);
            let wraps = grid[line][grid.last_column()]
                .flags
                .contains(alacritty_terminal::term::cell::Flags::WRAPLINE);
            text.push_str(if wraps { &part } else { part.trim_end() });
            if !wraps && line < cursor.line {
                text.push(' ');
            }
            line += 1;
            from = 0;
        }
        let text: String = sanitize_title(&text).chars().take(MAX_COMMAND).collect();
        (!text.is_empty()).then_some(text)
    }

    /// Every command the grid still shows marks of, oldest first.
    fn spans(&self) -> Vec<Span> {
        if self.term.mode().contains(TermMode::ALT_SCREEN)
            || self.commands.records.is_empty() && self.commands.current.is_none()
        {
            return Vec::new();
        }
        let (top, bottom) = (self.term.topmost_line(), self.term.bottommost_line());
        let mut spans: Vec<Span> = Vec::new();
        // The command whose output runs on, waiting for its end.
        let mut open: Option<usize> = None;
        let find = |spans: &mut Vec<Span>, n: u32, line: Line| -> usize {
            match spans.iter().rposition(|s| s.n == n) {
                Some(i) => i,
                None => {
                    spans.push(Span {
                        n,
                        prompt: line,
                        input: None,
                        output: None,
                        end: None,
                    });
                    spans.len() - 1
                }
            }
        };
        for l in top.0..=bottom.0 {
            let line = Line(l);
            for token in self.row_marks(line) {
                match token {
                    Token::PromptNext(n) | Token::PromptHere(n) => {
                        let (prompt, end) = match token {
                            Token::PromptNext(_) => (line + 1, line),
                            _ => (line, line - 1),
                        };
                        if let Some(i) = open.take()
                            && let Some(s) = spans.get_mut(i)
                            && let Some(out) = s.output
                        {
                            s.end = Some(end.max(out - 1));
                        }
                        let i = find(&mut spans, n, prompt);
                        if let Some(s) = spans.get_mut(i) {
                            s.prompt = prompt;
                        }
                    }
                    Token::Input(n, col) => {
                        let i = find(&mut spans, n, line);
                        if let Some(s) = spans.get_mut(i) {
                            s.input = Some(Point::new(line, Column(col)));
                            s.prompt = s.prompt.min(line);
                        }
                    }
                    Token::OutputNext(n) | Token::OutputHere(n) => {
                        let output = match token {
                            Token::OutputNext(_) => line + 1,
                            _ => line,
                        };
                        let i = find(&mut spans, n, line);
                        if let Some(s) = spans.get_mut(i) {
                            s.output = Some(output);
                            s.prompt = s.prompt.min(line);
                        }
                        open = Some(i);
                    }
                }
            }
        }
        spans
    }

    /// The last row with text at or above `line`, not above `floor`.
    fn last_text_row(&self, mut line: Line, floor: Line) -> Line {
        while line > floor && self.row_text(line, 0, self.cols()).trim().is_empty() {
            line -= 1;
        }
        line
    }

    /// Where the output of `span` ends: at the next prompt, or at the
    /// cursor while it runs; blank rows at its end left out.
    fn output_end(&self, span: &Span) -> Option<Line> {
        let start = span.output?;
        let end = span.end.unwrap_or_else(|| {
            let cursor = self.term.grid().cursor.point;
            if cursor.column.0 == 0 {
                cursor.line - 1
            } else {
                cursor.line
            }
        });
        let end = end.min(self.term.bottommost_line());
        if end < start {
            return None;
        }
        let end = self.last_text_row(end, start);
        let empty = end == start && self.row_text(end, 0, self.cols()).trim().is_empty();
        (!empty).then_some(end)
    }

    /// Whether the shell has sent prompt marks (OSC 133) in this pane.
    pub fn shell_integration(&self) -> bool {
        self.commands.next > 0
    }

    /// Scrolls to the prompt of the command before the one at the top of
    /// the view (`older`), or after it; past the last prompt, to the live
    /// bottom. `false` when there is none that way.
    pub fn jump_to_command(&mut self, older: bool) -> bool {
        let spans = self.spans();
        let offset = self.display_offset() as i32;
        let top = Line(-offset);
        let target = if older {
            spans.iter().rev().map(|s| s.prompt).find(|&p| p < top)
        } else {
            spans.iter().map(|s| s.prompt).find(|&p| p > top)
        };
        let Some(prompt) = target else {
            if !older && offset > 0 {
                self.scroll_to_bottom();
                return true;
            }
            return false;
        };
        let history = self.term.history_size() as i32;
        let want = (-prompt.0).clamp(0, history);
        if want == offset {
            return false;
        }
        self.term.scroll_display(Scroll::Delta(want - offset));
        self.follow_at_bottom(want > offset);
        true
    }

    /// Selects the output of a command: the last one that finished
    /// (`last`, or while the view is at the live bottom), else the one at
    /// the top of the view. `false` when it printed nothing or the shell
    /// sends no marks.
    pub fn select_command_output(&mut self, last: bool) -> bool {
        let spans = self.spans();
        let offset = self.display_offset() as i32;
        let ran = spans.iter().filter(|s| s.output.is_some());
        let span = if last || offset == 0 {
            ran.clone()
                .rev()
                .find(|s| s.end.is_some())
                .or_else(|| ran.clone().next_back())
        } else {
            let top = Line(-offset);
            ran.clone()
                .rev()
                .find(|s| s.prompt <= top)
                .or_else(|| ran.clone().next())
        };
        let Some(span) = span.copied() else {
            return false;
        };
        let (Some(start), Some(end)) = (span.output, self.output_end(&span)) else {
            return false;
        };
        let last_col = self.term.grid().last_column();
        let mut sel = Selection::new(
            SelectionType::Lines,
            Point::new(start, Column(0)),
            Side::Left,
        );
        sel.update(Point::new(end, last_col), Side::Right);
        self.term.selection = Some(sel);
        self.stop_selected = false;
        true
    }

    /// The viewport rows that show the line of a command that failed
    /// (exit code other than 0), for the window to mark.
    pub fn failed_rows(&self) -> Vec<usize> {
        if self.term.mode().contains(TermMode::ALT_SCREEN) || self.commands.records.is_empty() {
            return Vec::new();
        }
        let offset = self.display_offset() as i32;
        let failed = |n: u32| {
            self.commands
                .records
                .get(&n)
                .is_some_and(|r| r.exit.is_some_and(|c| c != 0))
        };
        let mut rows = Vec::new();
        for y in 0..self.rows() {
            let line = Line(y as i32 - offset);
            let marked = self.row_marks(line).into_iter().any(|t| match t {
                Token::Input(n, _) | Token::OutputNext(n) | Token::OutputHere(n) => failed(n),
                _ => false,
            });
            if marked {
                rows.push(y);
            }
        }
        rows
    }

    /// The last command the shell ran (from its OSC 133 marks).
    pub fn last_command(&self) -> Option<CommandStatus> {
        let (_, r) = self.commands.records.last_key_value()?;
        Some(CommandStatus {
            running: r.ended.is_none(),
            exit: r.exit,
            since: r.ended.unwrap_or(r.started),
            command: r.command.clone(),
        })
    }
}

impl Commands {
    fn new_number(&mut self) -> u32 {
        let n = self.next;
        self.next = self.next.wrapping_add(1);
        n
    }

    /// The command whose prompt shows, numbered now if its prompt had no
    /// `A`.
    fn current_number(&mut self) -> u32 {
        match self.current {
            Some(n) => n,
            None => {
                let n = self.new_number();
                self.current = Some(n);
                n
            }
        }
    }
}
