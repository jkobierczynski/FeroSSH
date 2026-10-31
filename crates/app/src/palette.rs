//! The command palette: one search box over every menu action, every
//! session and every command snippet. Ctrl+Shift+A opens it in the side
//! pane; typing narrows the list with a fuzzy match ("splr" finds Split
//! right), Enter runs the selected entry, Shift+Enter opens a session in a
//! split or types a snippet without its last Enter, and Esc closes it.
//!
//! The actions are read from the window's menu when the palette opens, so
//! the palette offers exactly what the menus offer, with their shortcuts,
//! check marks and greyed-out items. Drawn into a [`Grid`] like the other
//! side pane views; keys come in as [`SideKey`]s and what the user chose
//! goes out as a [`PalAction`].

use ferossh_config::settings::SessionKind;
use ferossh_render::{Flags, Grid};
use unicode_width::UnicodeWidthStr;

use crate::session_form::Line;
use crate::sessions::SideKey;
use crate::tabstrip::{Palette, fit};

/// Lines above the list: the title and the search box.
const HEADER: usize = 2;
/// Lines under the list: the key help.
const FOOTER: usize = 1;
/// Longest search one can type.
const MAX_FILTER: usize = 60;
/// Recently run entries kept at the top of the unfiltered list.
const RECENT: usize = 5;

/// What an entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A menu command, by its id.
    Action(u32),
    /// Open a session from the connection manager's list.
    Session(String),
    /// Type the snippet at this index.
    Snippet(usize),
}

/// One line of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// A dim word before the label: "Connect", "Send", or nothing for
    /// menu actions.
    pub prefix: &'static str,
    pub label: String,
    /// Right-aligned and dim: the shortcut, the session's `user@host`, or
    /// the snippet's first line.
    pub hint: String,
    /// More words the search finds it by (the menu it is in).
    pub extra: String,
    /// A menu item with a check mark.
    pub on: bool,
    pub kind: Kind,
}

impl Entry {
    /// A menu item: `path` is the menus it is in, outermost first, and
    /// `text` its menu text with `&` mnemonics and a tab before the
    /// shortcut.
    pub fn action(path: &[String], text: &str, id: u32, on: bool) -> Self {
        let (text, shortcut) = text.split_once('\t').unwrap_or((text, ""));
        let text = plain(text);
        // Items of a submenu say which one ("Theme: Nord"); the top menu
        // is only searched ("view").
        let label = match path.get(1..).and_then(<[String]>::last) {
            Some(sub) => format!("{}: {text}", short_menu(sub)),
            None => text,
        };
        let extra = path.iter().map(|p| plain(p)).collect::<Vec<_>>().join(" ");
        Self {
            prefix: "",
            label,
            hint: shortcut.trim().to_owned(),
            extra,
            on,
            kind: Kind::Action(id),
        }
    }

    pub fn session(name: &str, detail: &str, kind: SessionKind) -> Self {
        let prefix = match kind {
            SessionKind::Rdp | SessionKind::Vnc => "Open",
            _ => "Connect",
        };
        let tag = match kind {
            SessionKind::Ssh => "",
            SessionKind::Rdp => "rdp",
            SessionKind::Vnc => "vnc",
            SessionKind::Serial => "serial",
            SessionKind::Telnet => "telnet",
        };
        Self {
            prefix,
            label: name.to_owned(),
            hint: detail.to_owned(),
            extra: format!("{prefix} connection {tag}"),
            on: false,
            kind: Kind::Session(name.to_owned()),
        }
    }

    pub fn snippet(index: usize, name: &str, lines: &[String]) -> Self {
        let first = lines.first().map(String::as_str).unwrap_or_default();
        Self {
            prefix: "Send",
            label: name.to_owned(),
            hint: first.to_owned(),
            extra: format!("send snippet {}", lines.join(" ")),
            on: false,
            kind: Kind::Snippet(index),
        }
    }

    /// What remembers it among the recent entries.
    fn key(&self) -> String {
        match &self.kind {
            Kind::Action(id) => format!("a{id}"),
            Kind::Session(name) => format!("s{name}"),
            Kind::Snippet(_) => format!("n{}", self.label),
        }
    }

    /// How well `terms` match, if they all do: in the label (and prefix)
    /// counts double what a match in the other words does.
    fn score(&self, terms: &[String]) -> Option<i32> {
        let label = format!("{} {}", self.prefix, self.label).to_lowercase();
        let rest = format!("{} {}", self.extra, self.hint).to_lowercase();
        let mut total = 0;
        for term in terms {
            let main = fuzzy(term, &label).map(|s| s * 2);
            let other = fuzzy(term, &rest);
            total += main.max(other)?;
        }
        // Shorter labels first among equals.
        Some(total * 4 - self.label.chars().count() as i32 / 8)
    }
}

/// Menu text without its `&` mnemonics ("&&" stays one `&`) and without
/// the "..." of items that open a dialog.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                out.push('&');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out.trim_end_matches("...").trim().to_owned()
}

/// A submenu's name without its note: "Font (this pane)" is "Font".
fn short_menu(text: &str) -> String {
    let text = plain(text);
    match text.split_once(" (") {
        Some((name, _)) => name.to_owned(),
        None => text,
    }
}

/// Whether `term`'s characters all appear in `hay` in order, ignoring
/// case (both are lower case already), and how well: characters at the
/// start of a word and runs of them score more, so "splr" ranks "Split
/// right" over a label that only happens to hold those letters.
fn fuzzy(term: &str, hay: &str) -> Option<i32> {
    let t: Vec<char> = term.chars().collect();
    let h: Vec<char> = hay.chars().collect();
    let first = *t.first()?;
    let word_start = |i: usize| i == 0 || h.get(i - 1).is_some_and(|c| !c.is_alphanumeric());
    let mut best: Option<i32> = None;
    // Each place the first character appears starts a try; the rest is
    // matched greedily, preferring the start of a word when one is near.
    for start in (0..h.len()).filter(|&i| h.get(i) == Some(&first)) {
        let mut score = if word_start(start) { 8 } else { 1 };
        let mut at = start;
        let mut ok = true;
        for &c in t.get(1..).unwrap_or_default() {
            let next = (at + 1..h.len()).find(|&i| h.get(i) == Some(&c));
            let Some(next) = next else {
                ok = false;
                break;
            };
            // A run beats a jump; a jump to a word start beats a jump
            // into the middle of one.
            let jump_to_word = (at + 1..h.len()).find(|&i| h.get(i) == Some(&c) && word_start(i));
            let pick = if next == at + 1 {
                score += 6;
                next
            } else if let Some(w) = jump_to_word {
                score += 5;
                w
            } else {
                score += 1;
                next
            };
            at = pick;
        }
        if ok {
            best = best.max(Some(score));
        }
    }
    best
}

/// What the window should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PalAction {
    /// Run this menu command.
    Run(u32),
    /// Open this session; in a split with `split`.
    Open { host: String, split: bool },
    /// Type snippet `index`; `enter` presses Enter after its last line.
    Send { index: usize, enter: bool },
    /// Back to the terminal.
    Leave,
}

#[derive(Debug, Default)]
pub struct CommandPalette {
    entries: Vec<Entry>,
    filter: String,
    /// The entries the filter keeps, best first.
    shown: Vec<usize>,
    selected: usize,
    scroll: usize,
    /// Keys of the entries run lately, latest first.
    recent: Vec<String>,
}

impl CommandPalette {
    fn visible(rows: usize) -> usize {
        rows.saturating_sub(HEADER + FOOTER).max(1)
    }

    /// Starts afresh on `entries` with an empty search.
    pub fn open(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.filter.clear();
        self.selected = 0;
        self.scroll = 0;
        self.update();
    }

    /// Works out which entries show, and in what order.
    fn update(&mut self) {
        let terms: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let recent = |e: &Entry| self.recent.iter().position(|k| *k == e.key());
        if terms.is_empty() {
            // The recent ones first, latest on top; then the rest in menu
            // order, the sessions and the snippets.
            let mut first: Vec<(usize, usize)> = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| recent(e).map(|r| (r, i)))
                .collect();
            first.sort_unstable();
            let top: Vec<usize> = first.iter().map(|&(_, i)| i).collect();
            let rest = (0..self.entries.len()).filter(|i| !top.contains(i));
            self.shown = top.iter().copied().chain(rest).collect();
        } else {
            let mut scored: Vec<(i32, usize)> = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    let bonus = recent(e).map_or(0, |r| (RECENT - r) as i32 * 2);
                    e.score(&terms).map(|s| (s + bonus, i))
                })
                .collect();
            // Best first; the menu order among equals.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.shown = scored.into_iter().map(|(_, i)| i).collect();
        }
    }

    fn clamp(&mut self, visible: usize) {
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
    }

    /// The selected entry's action; it goes to the top of the recent ones.
    fn pick(&mut self, split: bool) -> Option<PalAction> {
        let e = self.entries.get(*self.shown.get(self.selected)?)?;
        let key = e.key();
        let action = match &e.kind {
            Kind::Action(id) => PalAction::Run(*id),
            Kind::Session(host) => PalAction::Open {
                host: host.clone(),
                split,
            },
            Kind::Snippet(index) => PalAction::Send {
                index: *index,
                enter: !split,
            },
        };
        self.recent.retain(|k| *k != key);
        self.recent.insert(0, key);
        self.recent.truncate(RECENT);
        Some(action)
    }

    pub fn key(&mut self, key: SideKey, rows: usize) -> Option<PalAction> {
        let visible = Self::visible(rows);
        match key {
            SideKey::Up => self.selected = self.selected.saturating_sub(1),
            SideKey::Down | SideKey::Tab => self.selected += 1,
            SideKey::PageUp => self.selected = self.selected.saturating_sub(visible),
            SideKey::PageDown => self.selected += visible,
            SideKey::Home => self.selected = 0,
            SideKey::End => self.selected = self.shown.len().saturating_sub(1),
            SideKey::Enter { split } => return self.pick(split),
            SideKey::Escape if self.filter.is_empty() => return Some(PalAction::Leave),
            SideKey::Escape => {
                self.filter.clear();
                self.selected = 0;
                self.update();
            }
            SideKey::Backspace => {
                self.filter.pop();
                self.selected = 0;
                self.update();
            }
            SideKey::Char(c) if !c.is_control() && self.filter.chars().count() < MAX_FILTER => {
                self.filter.push(c);
                self.selected = 0;
                self.update();
            }
            _ => {}
        }
        self.clamp(visible);
        None
    }

    /// A click on grid row `y` selects the entry there; `double` runs it.
    pub fn click(&mut self, y: usize, double: bool, rows: usize) -> Option<PalAction> {
        let i = y.checked_sub(HEADER)?;
        if i >= Self::visible(rows) || self.scroll + i >= self.shown.len() {
            return None;
        }
        self.selected = self.scroll + i;
        if double { self.pick(false) } else { None }
    }

    /// Wheel scrolling by `by` lines (positive: up).
    pub fn scroll_by(&mut self, by: i32, rows: usize) {
        let max = self.shown.len().saturating_sub(Self::visible(rows));
        self.scroll = if by > 0 {
            self.scroll.saturating_sub(by as usize)
        } else {
            (self.scroll + by.unsigned_abs() as usize).min(max)
        };
    }

    pub fn draw(&mut self, grid: &mut Grid, palette: &Palette, focused: bool) {
        let width = grid.cols();
        let rows = grid.rows();
        let bg = palette.strip;
        let visible = Self::visible(rows);
        self.clamp(visible);
        let mut line = Line::new(width, bg);
        line.put(" Command palette", palette.text, Flags::BOLD);
        grid.set_row(0, line.cells(bg));
        let mut line = Line::new(width, bg);
        if self.filter.is_empty() {
            let hint = if focused {
                " Type an action, connection or snippet▏"
            } else {
                " Ctrl+Shift+A to search"
            };
            line.put(&fit(hint, width), palette.dim, Flags::ITALIC);
        } else {
            line.put(" › ", palette.dim, Flags::empty());
            let room = width.saturating_sub(line.col + 1);
            // The end of a long search stays in view.
            let chars: Vec<char> = self.filter.chars().collect();
            let start = chars.len().saturating_sub(room);
            let tail: String = chars.get(start..).unwrap_or_default().iter().collect();
            line.put(&tail, palette.text, Flags::empty());
            if focused {
                line.put("▏", palette.accent, Flags::empty());
            }
        }
        grid.set_row(1, line.cells(bg));
        for y in 0..visible {
            let at = HEADER + y;
            if at >= rows.saturating_sub(FOOTER) {
                break;
            }
            let i = self.scroll + y;
            let here = i == self.selected;
            let row_bg = if here && focused { palette.active } else { bg };
            let mut line = Line::new(width, row_bg);
            match self.shown.get(i).and_then(|&n| self.entries.get(n)) {
                Some(e) => draw_entry(&mut line, e, width, palette),
                None if i == 0 && self.entries.is_empty() => {
                    line.put(" Nothing to offer.", palette.dim, Flags::ITALIC);
                }
                None if i == 0 => {
                    line.put(" Nothing matches.", palette.dim, Flags::ITALIC);
                }
                None => {}
            }
            grid.set_row(at, line.cells(row_bg));
        }
        if rows > HEADER {
            let mut line = Line::new(width, bg);
            line.put(
                &fit(" Enter runs · Shift+Enter: split / no Enter · Esc", width),
                palette.dim,
                Flags::empty(),
            );
            grid.set_row(rows - 1, line.cells(bg));
        }
    }
}

/// One entry: ` [prefix] label [✓]`, then the hint against the right edge
/// if there is room for some of it.
fn draw_entry(line: &mut Line, e: &Entry, width: usize, palette: &Palette) {
    line.put(" ", palette.text, Flags::empty());
    if !e.prefix.is_empty() {
        line.put(e.prefix, palette.dim, Flags::empty());
        line.put(" ", palette.text, Flags::empty());
    }
    let mark = if e.on { " ✓" } else { "" };
    // The label gets the room it needs first; the hint what is left, and
    // at most half the row.
    let room = width.saturating_sub(line.col + 1 + mark.width());
    let label = fit(&e.label, room);
    line.put(&label, palette.text, Flags::empty());
    line.put(mark, palette.accent, Flags::empty());
    let left = width.saturating_sub(line.col + 3).min(width / 2);
    if left >= 4 && !e.hint.is_empty() {
        let hint = fit(&e.hint, left);
        let pad = width.saturating_sub(line.col + hint.width() + 1);
        line.put(&" ".repeat(pad), palette.dim, Flags::empty());
        line.put(&hint, palette.dim, Flags::empty());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_render::Theme;

    fn path(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| (*s).to_owned()).collect()
    }

    fn entries() -> Vec<Entry> {
        vec![
            Entry::action(&path(&["&Window"]), "Ne&w tab\tCtrl+Shift+T", 105, false),
            Entry::action(
                &path(&["&File"]),
                "&Connect to...\tCtrl+Shift+N",
                102,
                false,
            ),
            Entry::action(&path(&["&Edit"]), "Copy on &select", 504, true),
            Entry::action(&path(&["&Window"]), "Split &right\tAlt+Shift+=", 700, false),
            Entry::action(&path(&["&Window"]), "Split &down\tAlt+Shift+-", 701, false),
            Entry::action(
                &path(&["&Settings", "&Theme (this pane)"]),
                "Nord",
                400,
                true,
            ),
            Entry::session("core-sw1", "admin@10.0.0.1", SessionKind::Ssh),
            Entry::session("lab-desktop", "me@lab", SessionKind::Rdp),
            Entry::snippet(0, "Show interfaces", &["show ip interface brief".into()]),
        ]
    }

    fn typed(p: &mut CommandPalette, text: &str) {
        for c in text.chars() {
            p.key(SideKey::Char(c), 20);
        }
    }

    fn labels(p: &CommandPalette) -> Vec<String> {
        p.shown
            .iter()
            .map(|&i| p.entries[i].label.clone())
            .collect()
    }

    fn texts(p: &mut CommandPalette) -> Vec<String> {
        let mut grid = Grid::new(44, 14);
        p.draw(&mut grid, &Palette::of(&Theme::default()), true);
        (0..14)
            .map(|y| {
                grid.row(y)
                    .unwrap()
                    .iter()
                    .filter(|c| !c.flags.contains(Flags::WIDE_SPACER))
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn menu_text_becomes_label_and_shortcut() {
        let e = &entries();
        assert_eq!(e[0].label, "New tab");
        assert_eq!(e[0].hint, "Ctrl+Shift+T");
        assert_eq!(e[0].extra, "Window");
        assert_eq!(e[1].label, "Connect to", "the dots go");
        assert_eq!(e[5].label, "Theme: Nord");
        assert_eq!(e[5].extra, "Settings Theme (this pane)");
        assert_eq!(plain("Fish && &chips"), "Fish & chips");
    }

    #[test]
    fn fuzzy_search_ranks_word_starts() {
        let mut p = CommandPalette::default();
        p.open(entries());
        typed(&mut p, "splr");
        assert_eq!(labels(&p)[0], "Split right");
        p.key(SideKey::Escape, 20);
        typed(&mut p, "split");
        assert_eq!(labels(&p)[..2], ["Split right", "Split down"]);
        // Words in any order, and the menu name counts.
        p.key(SideKey::Escape, 20);
        typed(&mut p, "down window");
        assert_eq!(labels(&p), ["Split down"]);
        p.key(SideKey::Escape, 20);
        typed(&mut p, "core");
        assert_eq!(labels(&p)[0], "core-sw1");
        p.key(SideKey::Escape, 20);
        typed(&mut p, "ctrl+shift+n");
        assert_eq!(labels(&p)[0], "Connect to", "shortcuts are searched");
        p.key(SideKey::Escape, 20);
        typed(&mut p, "zzz");
        assert!(labels(&p).is_empty());
        assert!(texts(&mut p)[2].contains("Nothing matches"));
        assert_eq!(p.key(SideKey::Enter { split: false }, 20), None);
    }

    #[test]
    fn enter_runs_what_is_selected() {
        let mut p = CommandPalette::default();
        p.open(entries());
        typed(&mut p, "new tab");
        assert_eq!(
            p.key(SideKey::Enter { split: false }, 20),
            Some(PalAction::Run(105))
        );
        p.open(entries());
        typed(&mut p, "core");
        assert_eq!(
            p.key(SideKey::Enter { split: true }, 20),
            Some(PalAction::Open {
                host: "core-sw1".into(),
                split: true
            })
        );
        p.open(entries());
        typed(&mut p, "send show");
        assert_eq!(
            p.key(SideKey::Enter { split: true }, 20),
            Some(PalAction::Send {
                index: 0,
                enter: false
            })
        );
        // Escape clears the search first, then leaves.
        p.open(entries());
        typed(&mut p, "x");
        assert_eq!(p.key(SideKey::Escape, 20), None);
        assert_eq!(p.key(SideKey::Escape, 20), Some(PalAction::Leave));
    }

    #[test]
    fn recent_entries_come_first() {
        let mut p = CommandPalette::default();
        p.open(entries());
        assert_eq!(labels(&p)[0], "New tab");
        typed(&mut p, "split down");
        p.key(SideKey::Enter { split: false }, 20);
        p.open(entries());
        typed(&mut p, "core");
        p.key(SideKey::Enter { split: false }, 20);
        p.open(entries());
        assert_eq!(labels(&p)[..3], ["core-sw1", "Split down", "New tab"]);
        assert_eq!(labels(&p).len(), entries().len(), "nothing twice");
    }

    #[test]
    fn keys_move_and_clicks_pick() {
        let mut p = CommandPalette::default();
        p.open(entries());
        p.key(SideKey::Up, 20);
        p.key(SideKey::Down, 20);
        p.key(SideKey::Down, 20);
        assert_eq!(
            p.key(SideKey::Enter { split: false }, 20),
            Some(PalAction::Run(504))
        );
        p.key(SideKey::End, 20);
        assert_eq!(
            p.key(SideKey::Enter { split: false }, 20),
            Some(PalAction::Send {
                index: 0,
                enter: true
            })
        );
        // A fresh palette: nothing recent above the menu order.
        let mut p = CommandPalette::default();
        p.open(entries());
        assert_eq!(p.click(HEADER + 3, false, 20), None);
        assert_eq!(p.click(HEADER + 3, true, 20), Some(PalAction::Run(700)));
        assert_eq!(p.click(HEADER + 40, true, 20), None);
        assert_eq!(p.click(0, true, 20), None);
    }

    #[test]
    fn draws_labels_hints_and_marks() {
        let mut p = CommandPalette::default();
        p.open(entries());
        let t = texts(&mut p);
        assert_eq!(t[0], " Command palette");
        assert!(t[1].contains("Type an action"));
        assert!(
            t[2].starts_with(" New tab") && t[2].ends_with("Ctrl+Shift+T"),
            "{t:#?}"
        );
        assert!(t[4].starts_with(" Copy on select ✓"), "{t:#?}");
        assert!(t[8].starts_with(" Connect core-sw1") && t[8].ends_with("admin@10.0.0.1"));
        assert!(t[9].starts_with(" Open lab-desktop"));
        assert!(t[10].starts_with(" Send Show interfaces"));
        assert!(t[13].contains("Enter runs"));
        typed(&mut p, "nord");
        let t = texts(&mut p);
        assert!(t[1].contains("› nord"));
        assert!(t[2].starts_with(" Theme: Nord ✓"), "{t:#?}");
    }

    #[test]
    fn long_labels_fit_a_narrow_pane() {
        let mut p = CommandPalette::default();
        p.open(vec![Entry::action(
            &path(&["&Window"]),
            "&Broadcast input to this tab's panes\tCtrl+Shift+B",
            730,
            false,
        )]);
        let mut grid = Grid::new(34, 5);
        p.draw(&mut grid, &Palette::of(&Theme::default()), true);
        let row: String = grid.row(2).unwrap().iter().map(|c| c.ch).collect();
        assert_eq!(row.chars().count(), 34);
        assert!(row.contains('…'), "{row}");
    }
}
