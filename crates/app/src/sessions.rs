//! The connection manager: the side pane's list of hosts from ssh_config (and
//! sessions imported from PuTTY), in folders, with favourites first and a
//! search line that filters as one types.
//!
//! Drawn into a [`Grid`] like the tab strip. Keys and clicks come in as
//! [`SideKey`]s and row numbers; what the user picked comes out as a
//! [`SideAction`] for the window to carry out.

use std::collections::HashSet;

use ferossh_config::host_query::{HostFacts, Query};
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
    /// Its tags: ssh_config's `Tag`, then the connection's own `Tags`.
    pub tags: Vec<String>,
    /// As connecting resolves them, for `user:`, `host:` and `port:`
    /// searches; unknown when it does not resolve.
    pub user: Option<String>,
    pub hostname: Option<String>,
    pub port: Option<u16>,
}

impl HostItem {
    /// What a search looks at.
    pub fn facts(&self) -> HostFacts<'_> {
        HostFacts {
            name: &self.name,
            folder: self.folder.as_deref(),
            user: self.user.as_deref(),
            hostname: self.hostname.as_deref(),
            port: self.port,
            kind: self.kind.name(),
            tags: &self.tags,
            detail: &self.detail,
        }
    }

    /// The detail with the tags after it, `#` before each:
    /// `me@web.example  #prod #web`.
    pub fn detail_with_tags(&self) -> String {
        if self.tags.is_empty() {
            return self.detail.clone();
        }
        let tags: Vec<String> = self.tags.iter().map(|t| format!("#{t}")).collect();
        if self.detail.is_empty() {
            tags.join(" ")
        } else {
            format!("{}  {}", self.detail, tags.join(" "))
        }
    }
}

/// A visible row.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// "+ New connection", first when not searching.
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
/// What marks a saved search's smart folder: a control character, which
/// no ordinary folder name holds ([`SessionList::set_hosts`] drops them),
/// so a folder of one's own called `Search: x` stays an ordinary folder.
const SEARCH_KEY: char = '\u{1}';
/// What a smart folder is shown as: this, then the search.
const SEARCH_LABEL: &str = "Search: ";

/// The search behind a smart folder's key, if it is one.
pub fn saved_search_of(folder: &str) -> Option<&str> {
    folder.strip_prefix(SEARCH_KEY)
}

/// The smart folder key of a saved search.
pub fn saved_search_folder(query: &str) -> String {
    format!("{SEARCH_KEY}{query}")
}

/// How folder `name` is shown.
pub fn folder_label(name: &str) -> String {
    match saved_search_of(name) {
        Some(q) => format!("{SEARCH_LABEL}{q}"),
        None => name.to_owned(),
    }
}

/// Whether a folder starts open: smart folders start closed, since they
/// list hosts that are in their own folders too.
pub fn open_by_default(folder: &str) -> bool {
    saved_search_of(folder).is_none()
}
/// The native tree's folder of saved workspaces.
const WORKSPACES: &str = "Workspaces";
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
    /// Open this saved workspace (the native tree's Workspaces folder).
    OpenWorkspace(String),
    /// Remove this saved workspace (the window asks first).
    DeleteWorkspace(String),
    /// Open this saved workspace's file in the text editor (F2).
    EditWorkspace(String),
    /// Keep this search as a smart folder.
    SaveSearch(String),
    /// Drop this saved search.
    RemoveSearch(String),
    /// Change several connections at once.
    Bulk(crate::bulk::BulkAction),
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
    /// Saved workspaces for the native tree: name and what it holds.
    workspaces: Vec<(String, String)>,
    /// Saved searches, shown as smart folders above the others.
    saved_searches: Vec<String>,
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
    pub fn set_hosts(&mut self, mut hosts: Vec<HostItem>, favourites: Vec<String>) {
        let keep = self.selected_host();
        for h in &mut hosts {
            if let Some(f) = &mut h.folder {
                f.retain(|c| !c.is_control());
            }
        }
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

    /// The search typed in the search box.
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_saved_searches(&mut self, searches: Vec<String>) {
        self.saved_searches = searches;
    }

    /// Whether the search typed is one of the saved ones already.
    pub fn query_saved(&self) -> bool {
        let q = self.query.trim();
        self.saved_searches
            .iter()
            .any(|s| s.eq_ignore_ascii_case(q))
    }

    /// The hosts a search finds, in file order.
    fn found(&self, query: &str) -> Vec<usize> {
        let q = Query::parse(query);
        (0..self.hosts.len())
            .filter(|&i| self.hosts.get(i).is_some_and(|h| q.matches(&h.facts())))
            .collect()
    }

    /// Whether folder `name` shows its hosts.
    fn is_open(&self, name: &str) -> bool {
        open_by_default(name) != self.collapsed.contains(name)
    }

    fn set_open(&mut self, name: String, open: bool) {
        if open == open_by_default(&name) {
            self.collapsed.remove(&name);
        } else {
            self.collapsed.insert(name);
        }
    }

    /// The saved search under the selection, if a smart folder is selected.
    pub fn selected_saved_search(&self) -> Option<String> {
        match self.rows().get(self.selected)? {
            Row::Folder { name, .. } => saved_search_of(name).map(str::to_owned),
            _ => None,
        }
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
            let mut hits = self.found(&self.query);
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
            // A smart folder stays when nothing matches, as in the tree.
            if members.is_empty() && saved_search_of(name).is_none() {
                return;
            }
            let open = self.is_open(name);
            rows.push(Row::Folder {
                name: name.to_owned(),
                open,
                count: members.len(),
            });
            if open {
                rows.extend(members.into_iter().map(host));
            }
        };
        for q in &self.saved_searches {
            group(&saved_search_folder(q), self.found(q), &mut rows);
        }
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
                    Some(Row::Folder { name, .. }) => self.set_open(name, open),
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
                Some(Row::Folder { name, open, .. }) => self.set_open(name, !open),
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
                    " + New connection",
                    if selected && focused {
                        palette.text
                    } else {
                        palette.dim
                    },
                    row_bg,
                    Flags::empty(),
                ),
                Some(Row::Folder { name, open, count }) => {
                    let label =
                        format!(" {} {}", if *open { "▾" } else { "▸" }, folder_label(name));
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
                    let shown = h.detail_with_tags();
                    let detail = match h.kind {
                        SessionKind::Ssh => shown,
                        kind if shown.is_empty() => kind.name().to_owned(),
                        kind => format!("{} · {shown}", kind.name()),
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

/// A host in the native connection tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeHost {
    /// The `Host` name, which connecting uses.
    pub name: String,
    /// What the tree shows: a star for a favourite, the name, and the kind
    /// when it is not SSH.
    pub label: String,
    /// The tooltip: `user@hostname:port`, and the kind.
    pub tip: String,
    /// `user@hostname:port`, shown dimmed after the label.
    pub detail: String,
    /// A saved workspace, not a host: opening it opens its tabs.
    pub workspace: bool,
}

/// A top-level entry of the native connection tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeNode {
    Folder { name: String, hosts: Vec<TreeHost> },
    Host(TreeHost),
}

impl TreeNode {
    pub fn hosts(&self) -> &[TreeHost] {
        match self {
            TreeNode::Folder { hosts, .. } => hosts,
            TreeNode::Host(h) => std::slice::from_ref(h),
        }
    }
}

impl SessionList {
    /// Sets the search text (from the native search box), at most 64
    /// characters.
    pub fn set_query(&mut self, query: &str) {
        let query: String = query.chars().take(64).collect();
        if query != self.query {
            self.query = query;
            self.selected = 0;
            self.scroll = 0;
        }
    }

    fn tree_host(&self, index: usize) -> Option<TreeHost> {
        let h = self.hosts.get(index)?;
        let star = if self.is_favourite(&h.name) {
            "\u{2605} "
        } else {
            ""
        };
        let label = match h.kind {
            SessionKind::Ssh => format!("{star}{}", h.name),
            kind => format!("{star}{} ({})", h.name, kind.name()),
        };
        let mut tip = match h.kind {
            SessionKind::Ssh => h.detail.clone(),
            kind if h.detail.is_empty() => kind.name().to_owned(),
            kind => format!("{} \u{b7} {}", kind.name(), h.detail),
        };
        if !h.tags.is_empty() {
            if !tip.is_empty() {
                tip.push('\n');
            }
            tip.push_str(&format!("Tags: {}", h.tags.join(", ")));
        }
        Some(TreeHost {
            name: h.name.clone(),
            label,
            tip,
            detail: h.detail_with_tags(),
            workspace: false,
        })
    }

    /// Sets the saved workspaces the native tree shows: each one's name
    /// and what it holds.
    pub fn set_workspaces(&mut self, workspaces: Vec<(String, String)>) {
        self.workspaces = workspaces;
    }

    /// The saved workspaces as tree items; while searching, only those
    /// whose name matches.
    fn tree_workspaces(&self) -> Vec<TreeHost> {
        let query = self.query.trim().to_lowercase();
        self.workspaces
            .iter()
            .filter(|(name, _)| query.is_empty() || name.to_lowercase().contains(&query))
            .map(|(name, detail)| TreeHost {
                name: name.clone(),
                label: name.clone(),
                tip: format!("Workspace \u{b7} {detail}: opens its tabs and panes"),
                detail: detail.clone(),
                workspace: true,
            })
            .collect()
    }

    /// The hosts as the native tree shows them: the same order and folders
    /// as the drawn list (favourites, hosts without a folder, then the
    /// folders), or the matches alone, flat, while searching.
    pub fn tree(&self) -> Vec<TreeNode> {
        let host = |i: usize| self.tree_host(i);
        if !self.query.trim().is_empty() {
            // The drawn list's search rows: matches only, favourites
            // first, then the workspaces that match.
            let mut found: Vec<TreeNode> = self
                .rows()
                .into_iter()
                .filter_map(|r| match r {
                    Row::Host { index, .. } => host(index).map(TreeNode::Host),
                    Row::Folder { .. } | Row::New => None,
                })
                .collect();
            found.extend(self.tree_workspaces().into_iter().map(TreeNode::Host));
            return found;
        }
        let mut out = Vec::new();
        for q in &self.saved_searches {
            out.push(TreeNode::Folder {
                name: saved_search_folder(q),
                hosts: self.found(q).into_iter().filter_map(host).collect(),
            });
        }
        let favs: Vec<TreeHost> = (0..self.hosts.len())
            .filter(|&i| {
                self.hosts
                    .get(i)
                    .is_some_and(|h| self.is_favourite(&h.name))
            })
            .filter_map(host)
            .collect();
        if !favs.is_empty() {
            out.push(TreeNode::Folder {
                name: FAVOURITES.to_owned(),
                hosts: favs,
            });
        }
        let workspaces = self.tree_workspaces();
        if !workspaces.is_empty() {
            out.push(TreeNode::Folder {
                name: WORKSPACES.to_owned(),
                hosts: workspaces,
            });
        }
        out.extend(
            (0..self.hosts.len())
                .filter(|&i| self.hosts.get(i).is_some_and(|h| h.folder.is_none()))
                .filter_map(host)
                .map(TreeNode::Host),
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
            let hosts = (0..self.hosts.len())
                .filter(|&i| {
                    self.hosts
                        .get(i)
                        .is_some_and(|h| h.folder.as_deref() == Some(f))
                })
                .filter_map(host)
                .collect();
            out.push(TreeNode::Folder {
                name: f.to_owned(),
                hosts,
            });
        }
        out
    }

    /// What Enter in the search box opens: the first match, or what was
    /// typed when nothing matches.
    pub fn first_match(&self) -> Option<String> {
        let tree = self.tree();
        if let Some(h) = tree.iter().flat_map(TreeNode::hosts).find(|h| !h.workspace) {
            return Some(h.name.clone());
        }
        let typed = self.query.trim();
        (!typed.is_empty()).then(|| typed.to_owned())
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
            tags: Vec::new(),
            user: Some("me".into()),
            hostname: Some(format!("{name}.example")),
            port: Some(22),
        }
    }

    #[test]
    fn field_searches_and_smart_folders() {
        let mut web = item("web", None);
        web.tags = vec!["prod".into()];
        let mut db = item("db1", Some("Work"));
        db.tags = vec!["prod".into(), "legacy".into()];
        let mut l = SessionList::new(vec![web, db, item("pi", Some("Home"))], Vec::new());
        l.set_query("tag:prod -tag:legacy");
        let names = |l: &SessionList| {
            l.tree()
                .iter()
                .flat_map(|n| n.hosts().iter().map(|h| h.name.clone()).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&l), ["web"]);
        assert!(!l.query_saved());
        l.set_saved_searches(vec!["tag:prod".into()]);
        l.set_query("TAG:prod ");
        assert!(l.query_saved());
        l.set_query("");
        // The smart folder comes first, closed, and lists the matches.
        let tree = l.tree();
        assert!(
            matches!(&tree[0], TreeNode::Folder { name, .. } if folder_label(name) == "Search: tag:prod")
        );
        assert_eq!(
            tree[0]
                .hosts()
                .iter()
                .map(|h| h.name.as_str())
                .collect::<Vec<_>>(),
            ["web", "db1"]
        );
        let t = texts(&mut l);
        assert!(t[3].starts_with(" ▸ Search: tag:prod"), "{t:#?}");
        assert!(t[5].contains("Work"), "closed at first: {t:#?}");
        l.key(SideKey::Down, 8);
        assert_eq!(l.selected_saved_search().as_deref(), Some("tag:prod"));
        l.key(SideKey::Enter { split: false }, 8);
        let t = texts(&mut l);
        assert!(
            t[4].contains("web") && t[5].contains("db1"),
            "opened: {t:#?}"
        );
        // A folder of one's own called "Search: tag:prod" is no smart
        // folder and keeps its own open state.
        let mut own = item("own", Some("Search: tag:prod"));
        own.tags = vec!["x".into()];
        l.set_hosts(vec![own], Vec::new());
        let tree = l.tree();
        assert_eq!(tree.len(), 2, "{tree:#?}");
        assert!(matches!(&tree[1], TreeNode::Folder { name, .. } if name == "Search: tag:prod"));
        assert!(saved_search_of("Search: tag:prod").is_none());
        // With nothing matching, the smart folder stays, and can be
        // selected and removed.
        let t = texts(&mut l);
        assert!(
            t[3].contains("Search: tag:prod") && t[3].ends_with('0'),
            "{t:#?}"
        );
        l.key(SideKey::Home, 8);
        l.key(SideKey::Down, 8);
        assert_eq!(l.selected_saved_search().as_deref(), Some("tag:prod"));
    }

    #[test]
    fn tags_show_after_the_detail_and_are_searched() {
        let mut web = item("web", None);
        web.tags = vec!["prod".into(), "eu".into()];
        let mut l = SessionList::new(vec![web, item("db1", Some("Work"))], Vec::new());
        let tree = l.tree();
        let h = &tree[0].hosts()[0];
        assert_eq!(h.detail, "me@web.example  #prod #eu");
        assert_eq!(h.tip, "me@web.example\nTags: prod, eu");
        for q in ["prod", "#prod", "#eu web"] {
            l.set_query(q);
            let names: Vec<String> = l
                .tree()
                .iter()
                .flat_map(|n| n.hosts().iter().map(|h| h.name.clone()).collect::<Vec<_>>())
                .collect();
            assert_eq!(names, ["web"], "{q}");
        }
        l.set_query("#work");
        assert!(l.tree().is_empty(), "a folder is no tag");
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
    fn tree_groups_like_the_list() {
        let l = list();
        let tree = l.tree();
        let names = |n: &TreeNode| n.hosts().iter().map(|h| h.name.clone()).collect::<Vec<_>>();
        assert_eq!(tree.len(), 4);
        assert!(matches!(&tree[0], TreeNode::Folder { name, .. } if name == "Favourites"));
        assert_eq!(names(&tree[0]), ["pi"]);
        assert!(matches!(&tree[1], TreeNode::Host(h) if h.name == "web"));
        assert!(matches!(&tree[2], TreeNode::Folder { name, .. } if name == "Work"));
        assert_eq!(names(&tree[2]), ["db1", "db2"]);
        assert!(matches!(&tree[3], TreeNode::Folder { name, .. } if name == "Home"));
        // A favourite carries a star wherever it is listed.
        assert_eq!(tree[3].hosts()[0].label, "\u{2605} pi");
        assert_eq!(tree[1].hosts()[0].tip, "me@web.example");
        assert_eq!(tree[1].hosts()[0].detail, "me@web.example");
    }

    #[test]
    fn tree_ignores_folds_of_the_drawn_list() {
        let mut l = list();
        l.collapsed.insert("Work".into());
        assert_eq!(l.tree()[2].hosts().len(), 2);
    }

    #[test]
    fn tree_search_is_flat_and_enter_opens_the_first_match() {
        let mut l = list();
        l.set_query("db");
        let tree = l.tree();
        assert_eq!(tree.len(), 2);
        assert!(tree.iter().all(|n| matches!(n, TreeNode::Host(_))));
        assert_eq!(l.first_match().as_deref(), Some("db1"));
        l.set_query("new.example.org");
        assert!(l.tree().is_empty());
        assert_eq!(l.first_match().as_deref(), Some("new.example.org"));
        l.set_query("");
        assert_eq!(l.first_match().as_deref(), Some("pi"));
    }

    #[test]
    fn workspaces_get_a_folder_and_are_found_but_never_opened_as_hosts() {
        let mut l = list();
        l.set_workspaces(vec![("Web day".into(), "2 tabs, 3 panes".into())]);
        let tree = l.tree();
        assert!(matches!(&tree[1], TreeNode::Folder { name, .. } if name == "Workspaces"));
        let w = &tree[1].hosts()[0];
        assert!(w.workspace);
        assert_eq!(w.detail, "2 tabs, 3 panes");
        l.set_query("web");
        let found = l.tree();
        assert_eq!(found.len(), 2);
        assert!(found[1].hosts()[0].workspace);
        assert_eq!(l.first_match().as_deref(), Some("web"));
        l.set_query("day");
        assert_eq!(l.first_match().as_deref(), Some("day"));
    }

    #[test]
    fn tree_tags_other_kinds() {
        let mut desk = item("desk", None);
        desk.kind = SessionKind::Rdp;
        let l = SessionList::new(vec![desk], Vec::new());
        let tree = l.tree();
        let h = &tree[0].hosts()[0];
        assert!(matches!(tree[0], TreeNode::Host(_)));
        assert!(h.label.starts_with("desk ("), "{}", h.label);
        assert!(h.tip.contains("me@desk.example"));
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
        assert_eq!(t[2], " + New connection");
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
