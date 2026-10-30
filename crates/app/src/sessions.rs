//! The connection manager: the side pane's list of hosts from ssh_config (and
//! sessions imported from PuTTY), in folders, with favourites first and a
//! search line that filters as one types.
//!
//! Drawn into a [`Grid`] like the tab strip. Keys and clicks come in as
//! [`SideKey`]s and row numbers; what the user picked comes out as a
//! [`SideAction`] for the window to carry out.

use std::collections::HashSet;

use ferossh_config::settings::SessionKind;
use ferossh_render::{Cell, Color, Flags, Grid, Rgb};

use crate::tabstrip::{Palette, fit};

/// One host in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostItem {
    /// The `Host` name, which is what connecting uses.
    pub name: String,
    pub folder: Option<String>,
    /// `user@hostname:port` as ssh_config resolves it, for the second
    /// column and the search.
    pub detail: String,
    /// What opening it does; anything but SSH is tagged in the list.
    pub kind: SessionKind,
}

/// A visible row.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// "+ New session", first when not searching.
    New,
    Folder {
        name: String,
        open: bool,
        count: usize,
    },
    Host {
        index: usize,
        favourite: bool,
    },
}

const FAVOURITES: &str = "Favourites";
/// Header lines above the rows: the title and the search line.
const HEADER: usize = 2;

/// Keys the side pane understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    /// Enter; with Shift, in a split.
    Enter {
        split: bool,
    },
    Escape,
    Backspace,
    /// Ctrl+D.
    ToggleFavourite,
    Delete,
    Tab,
    /// F5.
    Refresh,
    /// F2: edit the selected session.
    Edit,
    /// Insert: a new session.
    New,
    Char(char),
}

/// What the window should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SideAction {
    Open {
        host: String,
        split: bool,
    },
    ToggleFavourite(String),
    /// Open the session form for a new session.
    New,
    /// Open the session form on this host.
    Edit(String),
    /// Remove this saved session (the window asks first).
    Delete(String),
    /// Give the keyboard back to the terminal.
    Leave,
}

#[derive(Debug, Default)]
pub struct SessionList {
    hosts: Vec<HostItem>,
    favourites: Vec<String>,
    query: String,
    collapsed: HashSet<String>,
    selected: usize,
    scroll: usize,
}

impl SessionList {
    #[cfg(test)]
    pub fn new(hosts: Vec<HostItem>, favourites: Vec<String>) -> Self {
        Self {
            hosts,
            favourites,
            ..Self::default()
        }
    }

    /// Replaces the hosts (after the config or an import changed), keeping
    /// the selection on the same host when it is still there.
    pub fn set_hosts(&mut self, hosts: Vec<HostItem>, favourites: Vec<String>) {
        let keep = self.selected_host();
        self.hosts = hosts;
        self.favourites = favourites;
        self.selected = 0;
        if let Some(name) = keep
            && let Some(i) = self
                .rows()
                .iter()
                .position(|r| self.row_host(r) == Some(&name))
        {
            self.selected = i;
        }
    }

    pub fn set_favourites(&mut self, favourites: Vec<String>) {
        let keep = self.selected_host();
        self.favourites = favourites;
        if let Some(name) = keep
            && let Some(i) = self
                .rows()
                .iter()
                .position(|r| self.row_host(r) == Some(&name))
        {
            self.selected = i;
        }
    }

    /// Selects host `name` if it is listed.
    pub fn select(&mut self, name: &str) {
        if let Some(i) = self.rows().iter().position(|r| {
            self.row_host(r)
                .is_some_and(|h| h.eq_ignore_ascii_case(name))
        }) {
            self.selected = i;
        }
    }

    /// Selects the row at grid row `y` without opening anything (for a
    /// right click).
    pub fn select_at(&mut self, y: usize, visible: usize) {
        if let Some(i) = y.checked_sub(HEADER).map(|i| i + self.scroll)
            && i < self.rows().len()
        {
            self.selected = i;
            self.clamp(visible);
        }
    }

    fn is_favourite(&self, name: &str) -> bool {
        self.favourites.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    fn matches(&self, h: &HostItem) -> bool {
        let q = self.query.to_lowercase();
        q.split_whitespace().all(|word| {
            h.name.to_lowercase().contains(word)
                || h.detail.to_lowercase().contains(word)
                || h.folder
                    .as_deref()
                    .is_some_and(|f| f.to_lowercase().contains(word))
        })
    }

    /// The rows as shown: favourites, hosts without a folder, then the
    /// folders in file order. A search shows matching hosts only, flat.
    fn rows(&self) -> Vec<Row> {
        let host = |index: usize| Row::Host {
            index,
            favourite: self
                .hosts
                .get(index)
                .is_some_and(|h| self.is_favourite(&h.name)),
        };
        if !self.query.trim().is_empty() {
            let mut hits: Vec<usize> = (0..self.hosts.len())
                .filter(|&i| self.hosts.get(i).is_some_and(|h| self.matches(h)))
                .collect();
            // Favourites first, otherwise file order.
            hits.sort_by_key(|&i| {
                !self
                    .hosts
                    .get(i)
                    .is_some_and(|h| self.is_favourite(&h.name))
            });
            return hits.into_iter().map(host).collect();
        }
        let mut rows = vec![Row::New];
        let group = |name: &str, members: Vec<usize>, rows: &mut Vec<Row>| {
            if members.is_empty() {
                return;
            }
            let open = !self.collapsed.contains(name);
            rows.push(Row::Folder {
                name: name.to_owned(),
                open,
                count: members.len(),
            });
            if open {
                rows.extend(members.into_iter().map(host));
            }
        };
        let favs: Vec<usize> = (0..self.hosts.len())
            .filter(|&i| {
                self.hosts
                    .get(i)
                    .is_some_and(|h| self.is_favourite(&h.name))
            })
            .collect();
        group(FAVOURITES, favs, &mut rows);
        rows.extend(
            (0..self.hosts.len())
                .filter(|&i| self.hosts.get(i).is_some_and(|h| h.folder.is_none()))
                .map(host),
        );
        let mut folders: Vec<&str> = Vec::new();
        for h in &self.hosts {
            if let Some(f) = h.folder.as_deref()
                && !folders.contains(&f)
            {
                folders.push(f);
            }
        }
        for f in folders {
            let members = (0..self.hosts.len())
                .filter(|&i| {
                    self.hosts
                        .get(i)
                        .is_some_and(|h| h.folder.as_deref() == Some(f))
                })
                .collect();
            group(f, members, &mut rows);
        }
        rows
    }

    fn row_host(&self, row: &Row) -> Option<&String> {
        match row {
            Row::Host { index, .. } => self.hosts.get(*index).map(|h| &h.name),
            Row::Folder { .. } | Row::New => None,
        }
    }

    /// The host under the selection, if a host row is selected.
    pub fn selected_host(&self) -> Option<String> {
        self.rows()
            .get(self.selected)
            .and_then(|r| self.row_host(r))
            .cloned()
    }

    fn clamp(&mut self, visible: usize) {
        let n = self.rows().len();
        self.selected = self.selected.min(n.saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        let visible = visible.max(1);
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
    }

    /// Handles a key; `visible` is how many rows fit.
    pub fn key(&mut self, key: SideKey, visible: usize) -> Option<SideAction> {
        let rows = self.rows();
        let current = rows.get(self.selected).cloned();
        let page = visible.saturating_sub(1).max(1);
        match key {
            SideKey::Up => self.selected = self.selected.saturating_sub(1),
            SideKey::Down => self.selected += 1,
            SideKey::PageUp => self.selected = self.selected.saturating_sub(page),
            SideKey::PageDown => self.selected += page,
            SideKey::Home => self.selected = 0,
            SideKey::End => self.selected = rows.len().saturating_sub(1),
            SideKey::Left | SideKey::Right => {
                let open = key == SideKey::Right;
                match current {
                    Some(Row::Folder { name, .. }) => {
                        if open {
                            self.collapsed.remove(&name);
                        } else {
                            self.collapsed.insert(name);
                        }
                    }
                    // Left on a host goes to its folder.
                    Some(Row::Host { .. }) if !open => {
                        if let Some(i) = rows
                            .get(..self.selected)
                            .and_then(|r| r.iter().rposition(|r| matches!(r, Row::Folder { .. })))
                        {
                            self.selected = i;
                        }
                    }
                    _ => {}
                }
            }
            SideKey::Enter { split } => match current {
                Some(Row::New) => return Some(SideAction::New),
                Some(Row::Folder { name, open, .. }) => {
                    if open {
                        self.collapsed.insert(name);
                    } else {
                        self.collapsed.remove(&name);
                    }
                }
                Some(row) => {
                    let host = self.row_host(&row)?.clone();
                    return Some(SideAction::Open { host, split });
                }
                None => {
                    // Nothing listed: connect to what was typed.
                    let typed = self.query.trim().to_owned();
                    if !typed.is_empty() {
                        return Some(SideAction::Open { host: typed, split });
                    }
                }
            },
            SideKey::Escape => {
                if self.query.is_empty() {
                    return Some(SideAction::Leave);
                }
                self.query.clear();
                self.selected = 0;
            }
            SideKey::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.scroll = 0;
            }
            SideKey::ToggleFavourite => {
                return self.selected_host().map(SideAction::ToggleFavourite);
            }
            SideKey::New => return Some(SideAction::New),
            SideKey::Edit => {
                return Some(match self.selected_host() {
                    Some(host) => SideAction::Edit(host),
                    None => SideAction::New,
                });
            }
            SideKey::Delete => return self.selected_host().map(SideAction::Delete),
            SideKey::Char(c) if !c.is_control() => {
                if self.query.chars().count() < 64 {
                    self.query.push(c);
                }
                self.selected = 0;
                self.scroll = 0;
            }
            SideKey::Char(_) | SideKey::Tab | SideKey::Refresh => {}
        }
        self.clamp(visible);
        None
    }

    /// A click on grid row `y`: selects it; a double click opens a host or
    /// folds a folder.
    pub fn click(&mut self, y: usize, double: bool, visible: usize) -> Option<SideAction> {
        let i = y.checked_sub(HEADER)? + self.scroll;
        if i >= self.rows().len() {
            return None;
        }
        self.selected = i;
        self.clamp(visible);
        let folder = matches!(self.rows().get(i), Some(Row::Folder { .. } | Row::New));
        if double || folder {
            return self.key(SideKey::Enter { split: false }, visible);
        }
        None
    }

    /// Wheel scrolling by `lines` (positive: up).
    pub fn scroll_by(&mut self, lines: i32, visible: usize) {
        let max = self.rows().len().saturating_sub(visible.max(1));
        self.scroll = if lines > 0 {
            self.scroll.saturating_sub(lines as usize)
        } else {
            (self.scroll + lines.unsigned_abs() as usize).min(max)
        };
    }

    /// Rows that fit in a grid of `rows` lines.
    pub fn visible(rows: usize) -> usize {
        rows.saturating_sub(HEADER).max(1)
    }

    /// Draws the pane into `grid`. `focused` shows the selection bar and
    /// the search cursor.
    pub fn draw(&mut self, grid: &mut Grid, palette: &Palette, focused: bool) {
        let width = grid.cols();
        let visible = Self::visible(grid.rows());
        self.clamp(visible);
        let accent = Rgb::new(0xe5, 0xb5, 0x3b);
        let bg = palette.strip;
        let line = |text: &str, fg: Rgb, bg: Rgb, flags: Flags| -> Vec<Cell> {
            let mut cells: Vec<Cell> = Vec::new();
            for c in fit(text, width).chars() {
                cells.push(Cell::new(c, Color::Rgb(fg), Color::Rgb(bg), flags));
                if unicode_width::UnicodeWidthChar::width(c) == Some(2) {
                    if let Some(last) = cells.last_mut() {
                        last.flags.insert(Flags::WIDE);
                    }
                    cells.push(Cell::new(
                        ' ',
                        Color::Rgb(fg),
                        Color::Rgb(bg),
                        Flags::WIDE_SPACER,
                    ));
                }
            }
            cells.resize(
                width,
                Cell::new(' ', Color::Rgb(fg), Color::Rgb(bg), Flags::empty()),
            );
            cells.truncate(width);
            cells
        };
        grid.set_row(0, line(" Connections", palette.text, bg, Flags::BOLD));
        let search = if self.query.is_empty() && !focused {
            " Ctrl+Shift+S to search".to_owned()
        } else if self.query.is_empty() {
            " Type to search…".to_owned()
        } else {
            format!(" / {}{}", self.query, if focused { "▏" } else { "" })
        };
        let search_fg = if self.query.is_empty() {
            palette.dim
        } else {
            palette.text
        };
        grid.set_row(1, line(&search, search_fg, palette.active, Flags::empty()));
        let rows = self.rows();
        for y in 0..visible {
            let i = self.scroll + y;
            let selected = i == self.selected && !rows.is_empty();
            let row_bg = if selected && focused {
                palette.active
            } else {
                bg
            };
            let cells = match rows.get(i) {
                Some(Row::New) => line(
                    " + New session",
                    if selected && focused {
                        palette.text
                    } else {
                        palette.dim
                    },
                    row_bg,
                    Flags::empty(),
                ),
                Some(Row::Folder { name, open, count }) => {
                    let label = format!(" {} {name}", if *open { "▾" } else { "▸" });
                    let mut cells = line(&label, palette.text, row_bg, Flags::BOLD);
                    let used = label.chars().count() + 1;
                    put_right(&mut cells, &count.to_string(), used, palette.dim, row_bg);
                    cells
                }
                Some(Row::Host { index, favourite }) => {
                    let Some(h) = self.hosts.get(*index) else {
                        continue;
                    };
                    let star = if *favourite { "★" } else { " " };
                    let grouped = h.folder.is_some() || *favourite;
                    let indent = if self.query.trim().is_empty() && grouped {
                        "  "
                    } else {
                        ""
                    };
                    let mut cells = line(
                        &format!(" {indent}{star} {}", h.name),
                        palette.text,
                        row_bg,
                        if selected {
                            Flags::BOLD
                        } else {
                            Flags::empty()
                        },
                    );
                    if *favourite {
                        let at = 1 + indent.len();
                        if let Some(c) = cells.get_mut(at) {
                            c.fg = Color::Rgb(accent);
                        }
                    }
                    // The kind and detail, dimmed and to the right, where
                    // there is room after the name.
                    let used = 3 + indent.len() + h.name.chars().count() + 2;
                    let detail = match h.kind {
                        SessionKind::Ssh => h.detail.clone(),
                        kind if h.detail.is_empty() => kind.name().to_owned(),
                        kind => format!("{} · {}", kind.name(), h.detail),
                    };
                    put_right(&mut cells, &detail, used, palette.dim, row_bg);
                    cells
                }
                None if i == 0 && rows.is_empty() => {
                    let text = if self.hosts.is_empty() {
                        " No hosts in ~/.ssh/config"
                    } else {
                        " No match; Enter connects to it"
                    };
                    line(text, palette.dim, bg, Flags::empty())
                }
                None => line("", palette.dim, bg, Flags::empty()),
            };
            grid.set_row(HEADER + y, cells);
        }
    }
}

/// Writes `text` right-aligned into `cells`, one cell short of the edge
/// and starting no further left than `from`; shortened with an ellipsis
/// when it does not fit, left out when even that would not read.
pub(crate) fn put_right(cells: &mut [Cell], text: &str, from: usize, fg: Rgb, bg: Rgb) {
    let end = cells.len().saturating_sub(1);
    let room = end.saturating_sub(from);
    if text.is_empty() || room < 4 {
        return;
    }
    let text = fit(text, room);
    let width: usize = text
        .chars()
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
        .sum();
    let mut x = end - width;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w == 0 {
            continue;
        }
        if let Some(cell) = cells.get_mut(x) {
            *cell = Cell::new(c, Color::Rgb(fg), Color::Rgb(bg), Flags::empty());
            if w == 2 {
                cell.flags.insert(Flags::WIDE);
            }
        }
        if w == 2
            && let Some(cell) = cells.get_mut(x + 1)
        {
            *cell = Cell::new(' ', Color::Rgb(fg), Color::Rgb(bg), Flags::WIDE_SPACER);
        }
        x += w;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_render::Theme;

    fn item(name: &str, folder: Option<&str>) -> HostItem {
        HostItem {
            name: name.into(),
            folder: folder.map(Into::into),
            detail: format!("me@{name}.example"),
            kind: SessionKind::Ssh,
        }
    }

    fn list() -> SessionList {
        SessionList::new(
            vec![
                item("web", None),
                item("db1", Some("Work")),
                item("db2", Some("Work")),
                item("pi", Some("Home")),
            ],
            vec!["pi".into()],
        )
    }

    fn texts(l: &mut SessionList) -> Vec<String> {
        let mut grid = Grid::new(40, 10);
        l.draw(&mut grid, &Palette::of(&Theme::default()), true);
        (0..10)
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

    /// A folder row as `texts` shows it in 40 columns: the count
    /// right-aligned, one cell from the edge.
    fn folder(label: &str, count: usize) -> String {
        format!(" {label:<37}{count}")
    }

    #[test]
    fn details_sit_right_and_tag_desktops() {
        let mut desk = item("desk", None);
        desk.kind = SessionKind::Rdp;
        let mut l = SessionList::new(vec![item("web", None), desk], Vec::new());
        let t = texts(&mut l);
        // Right-aligned: ending one cell before the 40th column.
        let row = |name: &str, detail: &str| {
            format!("{name:<w$}{detail}", w = 39 - detail.chars().count())
        };
        assert_eq!(t[3], row("   web", "me@web.example"));
        assert_eq!(t[4], row("   desk", "rdp · me@desk.example"));
        // Too narrow for the detail: shortened, then left out.
        let mut grid = Grid::new(16, 6);
        l.draw(&mut grid, &Palette::of(&Theme::default()), true);
        let row: String = grid.row(HEADER + 1).unwrap().iter().map(|c| c.ch).collect();
        assert_eq!(row, "   web  me@web… ");
        let mut grid = Grid::new(10, 6);
        l.draw(&mut grid, &Palette::of(&Theme::default()), true);
        let row: String = grid.row(HEADER + 1).unwrap().iter().map(|c| c.ch).collect();
        assert_eq!(row.trim_end(), "   web");
    }

    #[test]
    fn groups_favourites_and_folders() {
        let mut l = list();
        let t = texts(&mut l);
        assert_eq!(t[0], " Connections");
        assert_eq!(t[2], " + New session");
        assert_eq!(t[3], folder("▾ Favourites", 1));
        assert!(t[4].starts_with("   ★ pi"), "{:?}", t[4]);
        assert!(t[5].starts_with("   web"), "{:?}", t[5]);
        assert_eq!(t[6], folder("▾ Work", 2));
        assert!(t[7].contains("db1") && t[7].contains("me@db1.example"));
        assert_eq!(t[9], folder("▾ Home", 1));
    }

    #[test]
    fn keys_navigate_and_open() {
        let mut l = list();
        assert_eq!(
            l.key(SideKey::Enter { split: false }, 8),
            Some(SideAction::New)
        );
        l.key(SideKey::Down, 8);
        assert_eq!(l.selected_host(), None, "the favourites folder is next");
        l.key(SideKey::Down, 8);
        assert_eq!(l.selected_host().as_deref(), Some("pi"));
        assert_eq!(
            l.key(SideKey::Enter { split: true }, 8),
            Some(SideAction::Open {
                host: "pi".into(),
                split: true
            })
        );
        assert_eq!(
            l.key(SideKey::ToggleFavourite, 8),
            Some(SideAction::ToggleFavourite("pi".into()))
        );
        // Collapse Work.
        l.key(SideKey::Down, 8);
        l.key(SideKey::Down, 8);
        l.key(SideKey::Down, 8);
        assert_eq!(l.selected_host(), Some("db1".into()));
        l.key(SideKey::Left, 8);
        assert_eq!(l.selected_host(), None);
        l.key(SideKey::Left, 8);
        let t = texts(&mut l);
        assert_eq!(t[6], folder("▸ Work", 2));
        assert_eq!(t[7], folder("▾ Home", 1));
        assert_eq!(l.key(SideKey::Escape, 8), Some(SideAction::Leave));
    }

    #[test]
    fn edit_new_and_delete_keys() {
        let mut l = list();
        assert_eq!(l.key(SideKey::Edit, 8), Some(SideAction::New));
        l.key(SideKey::Down, 8);
        l.key(SideKey::Down, 8);
        assert_eq!(l.key(SideKey::Edit, 8), Some(SideAction::Edit("pi".into())));
        assert_eq!(
            l.key(SideKey::Delete, 8),
            Some(SideAction::Delete("pi".into()))
        );
        assert_eq!(l.key(SideKey::New, 8), Some(SideAction::New));
    }

    #[test]
    fn search_filters() {
        let mut l = list();
        for c in "db2".chars() {
            l.key(SideKey::Char(c), 8);
        }
        let t = texts(&mut l);
        assert!(t[1].contains("/ db2"));
        assert!(t[2].contains("db2"));
        assert_eq!(t[3], "");
        assert_eq!(l.selected_host().as_deref(), Some("db2"));
        // Search hits folders and details too.
        l.key(SideKey::Escape, 8);
        for c in "home".chars() {
            l.key(SideKey::Char(c), 8);
        }
        assert_eq!(l.selected_host().as_deref(), Some("pi"));
        // No match: Enter connects to what was typed.
        l.key(SideKey::Escape, 8);
        for c in "new.host".chars() {
            l.key(SideKey::Char(c), 8);
        }
        assert_eq!(
            l.key(SideKey::Enter { split: false }, 8),
            Some(SideAction::Open {
                host: "new.host".into(),
                split: false
            })
        );
    }

    #[test]
    fn clicks_and_scrolling() {
        let mut l = list();
        assert_eq!(l.click(0, false, 3), None);
        assert_eq!(l.click(2, false, 3), Some(SideAction::New));
        assert_eq!(l.click(4, false, 3), None);
        assert_eq!(l.selected_host().as_deref(), Some("pi"));
        assert_eq!(
            l.click(4, true, 3),
            Some(SideAction::Open {
                host: "pi".into(),
                split: false
            })
        );
        l.key(SideKey::End, 3);
        assert_eq!(l.selected_host().as_deref(), Some("pi"));
        assert!(l.scroll > 0);
        l.scroll_by(10, 3);
        assert_eq!(l.scroll, 0);
    }

    #[test]
    fn keeps_selection_when_hosts_change() {
        let mut l = list();
        l.key(SideKey::Down, 8);
        l.key(SideKey::Down, 8);
        l.key(SideKey::Down, 8);
        assert_eq!(l.selected_host().as_deref(), Some("web"));
        l.set_favourites(vec!["web".into(), "pi".into()]);
        assert_eq!(l.selected_host().as_deref(), Some("web"));
        assert_eq!(l.selected, 2);
        let mut hosts = vec![item("new", None)];
        hosts.extend(list().hosts);
        l.set_hosts(hosts, vec![]);
        assert_eq!(l.selected_host().as_deref(), Some("web"));
        assert_eq!(l.selected, 2);
    }
}
