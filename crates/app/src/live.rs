//! The side pane's session manager: every session open right now, in
//! every tab (and a snippet run's sessions that no tab shows), with its
//! kind, tab and state. Enter or a double-click shows a session, Delete
//! closes it (the window asks first), and typing filters the list. The
//! saved hosts are the connection manager's (see [`crate::sessions`]).
//!
//! The window builds the [`LiveSession`]s on each draw, so the list is
//! always the live state; the view keeps only the filter, the selection
//! (by pane, so it stays on its session as others come and go) and the
//! scroll. Keys come in as [`SideKey`]s and what the user asked for goes
//! out as a [`LiveAction`].

use ferossh_render::{Cell, Color, Flags, Grid, Rgb};

use crate::layout::PaneId;
use crate::sessions::{SideKey, put_right};
use crate::tabstrip::{Palette, fit};

/// Lines above the list: the title and the filter line.
pub const HEADER: usize = 2;

/// Lines each session takes: its target, then its tab, state and notes.
const LINES: usize = 2;

/// What a session runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ssh,
    Telnet,
    Serial,
    Tmux,
    Desktop,
    Local,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Ssh => "SSH",
            Kind::Telnet => "Telnet",
            Kind::Serial => "serial",
            Kind::Tmux => "tmux",
            Kind::Desktop => "desktop",
            Kind::Local => "local",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    Connected,
    Connecting,
    /// Dropped, waiting to connect again on its own.
    Reconnecting,
    /// Closed, refused or blocked; the pane still shows what it printed.
    Ended,
}

impl LiveState {
    fn name(self) -> &'static str {
        match self {
            LiveState::Connected => "connected",
            LiveState::Connecting => "connecting",
            LiveState::Reconnecting => "reconnecting",
            LiveState::Ended => "ended",
        }
    }
}

/// One open session, as the window sees it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    pub pane: PaneId,
    /// The tab it is in, from 0; `None` for a snippet run's session that
    /// no tab shows.
    pub tab: Option<usize>,
    pub kind: Kind,
    /// `user@host[:port]`, the COM port, or the tmux pane's host.
    pub target: String,
    /// The title the host set, when there is one.
    pub title: Option<String>,
    pub state: LiveState,
    /// Short notes for the detail line: tmux, forwards, logging and so on.
    pub notes: Vec<&'static str>,
    /// The window's focused pane.
    pub focused: bool,
}

impl LiveSession {
    fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty()
            || self.target.to_lowercase().contains(&q)
            || self
                .title
                .as_deref()
                .is_some_and(|t| t.to_lowercase().contains(&q))
            || self.kind.name().to_lowercase().contains(&q)
    }

    /// The second line: where it is, its state and the notes.
    fn detail(&self) -> String {
        let mut parts = vec![match self.tab {
            Some(i) => format!("tab {}", i + 1),
            None => "no tab".to_owned(),
        }];
        parts.push(self.state.name().to_owned());
        parts.extend(self.notes.iter().map(|n| (*n).to_owned()));
        if let Some(t) = self.title.as_deref().filter(|t| *t != self.target) {
            parts.push(t.to_owned());
        }
        parts.join(" · ")
    }
}

/// What the window should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveAction {
    /// Show the session: its tab and pane, with the keyboard.
    Show(PaneId),
    /// Close the session's pane, after asking.
    Close(PaneId),
    Leave,
}

#[derive(Debug, Default)]
pub struct LiveView {
    query: String,
    /// The selected session's pane, kept across changes to the list.
    pane: Option<PaneId>,
    /// Its index in the filtered list, for when that pane goes away.
    at: usize,
    /// The first session shown.
    scroll: usize,
}

impl LiveView {
    /// Sessions that fit in a grid of `rows` lines.
    pub fn visible(rows: usize) -> usize {
        (rows.saturating_sub(HEADER) / LINES).max(1)
    }

    fn shown<'a>(&self, all: &'a [LiveSession]) -> Vec<&'a LiveSession> {
        all.iter().filter(|s| s.matches(&self.query)).collect()
    }

    /// Puts the selection back on its pane, or on the session now at its
    /// place; `follow` scrolls it into sight (not after wheel scrolling).
    fn clamp(&mut self, shown: &[&LiveSession], visible: usize, follow: bool) {
        if let Some(i) = self
            .pane
            .and_then(|p| shown.iter().position(|s| s.pane == p))
        {
            self.at = i;
        }
        self.at = self.at.min(shown.len().saturating_sub(1));
        self.pane = shown.get(self.at).map(|s| s.pane);
        if follow && self.at < self.scroll {
            self.scroll = self.at;
        }
        if follow && self.at >= self.scroll + visible {
            self.scroll = self.at + 1 - visible;
        }
        self.scroll = self.scroll.min(shown.len().saturating_sub(visible));
    }

    fn pick(&mut self, at: usize, shown: &[&LiveSession]) {
        self.at = at.min(shown.len().saturating_sub(1));
        self.pane = shown.get(self.at).map(|s| s.pane);
    }

    /// Starts on the focused pane's session, with no filter.
    pub fn reset(&mut self, all: &[LiveSession]) {
        self.query.clear();
        self.scroll = 0;
        self.pane = all.iter().find(|s| s.focused).map(|s| s.pane);
        self.at = 0;
    }

    pub fn key(&mut self, key: SideKey, all: &[LiveSession], rows: usize) -> Option<LiveAction> {
        let visible = Self::visible(rows);
        let mut shown = self.shown(all);
        self.clamp(&shown, visible, false);
        match key {
            SideKey::Up => self.pick(self.at.saturating_sub(1), &shown),
            SideKey::Down => self.pick(self.at + 1, &shown),
            SideKey::PageUp => self.pick(self.at.saturating_sub(visible), &shown),
            SideKey::PageDown => self.pick(self.at + visible, &shown),
            SideKey::Home => self.pick(0, &shown),
            SideKey::End => self.pick(usize::MAX, &shown),
            SideKey::Enter { .. } => return self.pane.map(LiveAction::Show),
            SideKey::Delete => return self.pane.map(LiveAction::Close),
            SideKey::Escape => {
                if self.query.is_empty() {
                    return Some(LiveAction::Leave);
                }
                self.query.clear();
            }
            SideKey::Backspace => {
                self.query.pop();
            }
            SideKey::Char(c) if !c.is_control() && self.query.chars().count() < 60 => {
                self.query.push(c);
                // A new filter starts at its first match.
                self.pane = None;
                self.at = 0;
            }
            _ => {}
        }
        shown = self.shown(all);
        self.clamp(&shown, visible, true);
        None
    }

    /// A click on grid row `y` selects the session there; a double-click
    /// also shows it.
    pub fn click(
        &mut self,
        y: usize,
        double: bool,
        all: &[LiveSession],
        rows: usize,
    ) -> Option<LiveAction> {
        let shown = self.shown(all);
        let i = y.checked_sub(HEADER)? / LINES + self.scroll;
        let pane = shown.get(i)?.pane;
        self.pick(i, &shown);
        self.clamp(&shown, Self::visible(rows), true);
        double.then_some(LiveAction::Show(pane))
    }

    /// Wheel scrolling by `by` lines (positive: up).
    pub fn scroll_by(&mut self, by: i32, all: &[LiveSession], rows: usize) {
        let max = self.shown(all).len().saturating_sub(Self::visible(rows));
        // In sessions, which take two lines each.
        let n = (by.unsigned_abs() as usize).div_ceil(LINES);
        self.scroll = if by > 0 {
            self.scroll.saturating_sub(n)
        } else {
            (self.scroll + n).min(max)
        };
    }

    /// Draws the view; `focused` shows the selection and the input cursor.
    pub fn draw(&mut self, grid: &mut Grid, palette: &Palette, focused: bool, all: &[LiveSession]) {
        let width = grid.cols();
        let visible = Self::visible(grid.rows());
        let shown = self.shown(all);
        self.clamp(&shown, visible, false);
        let line = |text: &str, fg: Rgb, bg: Rgb, flags: Flags| -> Vec<Cell> {
            let mut cells: Vec<Cell> = fit(text, width)
                .chars()
                .map(|c| Cell::new(c, Color::Rgb(fg), Color::Rgb(bg), flags))
                .collect();
            cells.resize(
                width,
                Cell::new(' ', Color::Rgb(fg), Color::Rgb(bg), Flags::empty()),
            );
            cells.truncate(width);
            cells
        };
        let bg = palette.strip;
        let mut title = line(" Sessions", palette.text, bg, Flags::BOLD);
        if !all.is_empty() {
            put_right(&mut title, &all.len().to_string(), 10, palette.dim, bg);
        }
        grid.set_row(0, title);
        let (filter, fg) = match (self.query.is_empty(), focused) {
            (true, false) => (" Ctrl+Shift+L to pick one".to_owned(), palette.dim),
            (true, true) => (
                " Enter shows, Del closes, type to filter".to_owned(),
                palette.dim,
            ),
            (false, _) => (
                format!(" / {}{}", self.query, if focused { "▏" } else { "" }),
                palette.text,
            ),
        };
        grid.set_row(1, line(&filter, fg, palette.active, Flags::empty()));
        let green = Rgb::new(0x98, 0xc3, 0x79);
        let amber = Rgb::new(0xe5, 0xb5, 0x3b);
        for y in 0..visible * LINES {
            let row = HEADER + y;
            let i = self.scroll + y / LINES;
            let Some(s) = shown.get(i) else {
                let text = match (y, all.is_empty()) {
                    (0, true) => " No sessions are open.",
                    (0, false) => " No match",
                    _ => "",
                };
                grid.set_row(row, line(text, palette.dim, bg, Flags::empty()));
                continue;
            };
            let chosen = focused && i == self.at;
            let row_bg = if chosen { palette.active } else { bg };
            if y % LINES == 1 {
                let detail = format!("   {}", s.detail());
                grid.set_row(row, line(&detail, palette.dim, row_bg, Flags::empty()));
                continue;
            }
            let flags = if s.focused || chosen {
                Flags::BOLD
            } else {
                Flags::empty()
            };
            let text = format!(" ● {}", s.target);
            let mut cells = line(&text, palette.text, row_bg, flags);
            if let Some(dot) = cells.get_mut(1) {
                dot.fg = Color::Rgb(match s.state {
                    LiveState::Connected => green,
                    LiveState::Connecting | LiveState::Reconnecting => amber,
                    LiveState::Ended => palette.dim,
                });
                dot.flags = Flags::empty();
            }
            let used = text.chars().count() + 2;
            put_right(&mut cells, s.kind.name(), used, palette.dim, row_bg);
            grid.set_row(row, cells);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_render::Theme;

    fn session(pane: PaneId, tab: Option<usize>, target: &str, state: LiveState) -> LiveSession {
        LiveSession {
            pane,
            tab,
            kind: Kind::Ssh,
            target: target.into(),
            title: None,
            state,
            notes: Vec::new(),
            focused: false,
        }
    }

    fn sample() -> Vec<LiveSession> {
        let mut db = session(2, Some(0), "me@db", LiveState::Connecting);
        db.notes = vec!["tmux"];
        db.title = Some("vim".into());
        let mut com = session(3, Some(1), "COM3 115200 8N1", LiveState::Connected);
        com.kind = Kind::Serial;
        com.focused = true;
        vec![
            session(1, Some(0), "me@web", LiveState::Connected),
            db,
            com,
            session(4, None, "me@core", LiveState::Ended),
        ]
    }

    fn texts(v: &mut LiveView, all: &[LiveSession], rows: usize) -> Vec<String> {
        let mut grid = Grid::new(40, rows);
        v.draw(&mut grid, &Palette::of(&Theme::default()), true, all);
        (0..rows)
            .map(|y| {
                grid.row(y)
                    .unwrap()
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn lists_each_session_with_its_tab_state_and_kind() {
        let all = sample();
        let mut v = LiveView::default();
        let t = texts(&mut v, &all, 12);
        assert!(
            t[0].starts_with(" Sessions") && t[0].ends_with('4'),
            "{:?}",
            t[0]
        );
        assert_eq!(t[2], format!("{:<36}SSH", " ● me@web"));
        assert_eq!(t[3], "   tab 1 · connected");
        assert_eq!(t[5], "   tab 1 · connecting · tmux · vim");
        assert!(t[6].starts_with(" ● COM3") && t[6].ends_with("serial"));
        assert_eq!(t[9], "   no tab · ended");
        assert!(texts(&mut v, &[], 6)[2].contains("No sessions are open."));
    }

    #[test]
    fn enter_shows_and_delete_closes_the_selected_session() {
        let all = sample();
        let mut v = LiveView::default();
        v.reset(&all);
        // It starts on the focused pane's session.
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 12),
            Some(LiveAction::Show(3))
        );
        v.key(SideKey::Up, &all, 12);
        assert_eq!(v.key(SideKey::Delete, &all, 12), Some(LiveAction::Close(2)));
        v.key(SideKey::End, &all, 12);
        assert_eq!(
            v.key(SideKey::Enter { split: true }, &all, 12),
            Some(LiveAction::Show(4))
        );
        assert_eq!(v.key(SideKey::Escape, &all, 12), Some(LiveAction::Leave));
    }

    #[test]
    fn selection_follows_its_session_when_others_close() {
        let mut all = sample();
        let mut v = LiveView::default();
        v.key(SideKey::Down, &all, 12);
        v.key(SideKey::Down, &all, 12);
        all.remove(0);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 12),
            Some(LiveAction::Show(3))
        );
        // Its own session gone, the one now at its place is picked.
        all.retain(|s| s.pane != 3);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 12),
            Some(LiveAction::Show(4))
        );
    }

    #[test]
    fn typing_filters_and_escape_clears_first() {
        let all = sample();
        let mut v = LiveView::default();
        for c in "com".chars() {
            v.key(SideKey::Char(c), &all, 12);
        }
        let t = texts(&mut v, &all, 12);
        assert_eq!(t[1], " / com▏");
        // Only the serial port's target has "com" in it.
        assert!(t[2].starts_with(" ● COM3"));
        assert_eq!(t[4], "");
        v.key(SideKey::Escape, &all, 12);
        v.key(SideKey::Char('x'), &all, 12);
        assert!(texts(&mut v, &all, 12)[2].contains("No match"));
        assert_eq!(v.key(SideKey::Enter { split: false }, &all, 12), None);
    }

    #[test]
    fn clicks_pick_and_double_clicks_show() {
        let all = sample();
        let mut v = LiveView::default();
        assert_eq!(v.click(HEADER + 3, false, &all, 12), None);
        assert_eq!(v.key(SideKey::Delete, &all, 12), Some(LiveAction::Close(2)));
        assert_eq!(
            v.click(HEADER + 4, true, &all, 12),
            Some(LiveAction::Show(3))
        );
        assert_eq!(v.click(0, true, &all, 12), None);
        assert_eq!(v.click(HEADER + 40, true, &all, 12), None);
    }

    #[test]
    fn wheel_scrolls_by_sessions() {
        let all = sample();
        let mut v = LiveView::default();
        // Room for two sessions in 6 rows.
        v.scroll_by(-3, &all, 6);
        assert_eq!(
            texts(&mut v, &all, 6)[2],
            format!("{:<33}serial", " ● COM3 115200 8N1")
        );
        v.scroll_by(-9, &all, 6);
        assert!(texts(&mut v, &all, 6)[2].starts_with(" ● COM3"));
        v.scroll_by(1, &all, 6);
        assert!(texts(&mut v, &all, 6)[2].starts_with(" ● me@db"));
    }
}
