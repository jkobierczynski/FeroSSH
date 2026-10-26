//! The side pane's import page: which other SSH clients have saved
//! sessions on this account, then a preview with a box per session, so
//! nothing is written until the user has said which ones.
//!
//! Keys come in as [`SideKey`]s; what the user chose comes out as an
//! [`ImportAction`]. The window reads the clients and writes the import.

use ferossh_config::SessionKind;
use ferossh_config::import::{Client, Session};
use ferossh_render::{Flags, Grid};

use crate::session_form::Line;
use crate::sessions::SideKey;
use crate::tabstrip::{Palette, fit};

/// Sessions found in one place.
#[derive(Clone, Debug)]
pub struct Found {
    pub client: Client,
    /// Where they were read, for the user: "registry" or a file's path.
    pub origin: String,
    pub sessions: Vec<Session>,
    /// Per session: its name is already a host elsewhere, so it is left
    /// out.
    pub taken: Vec<bool>,
}

impl Found {
    fn taken(&self, i: usize) -> bool {
        self.taken.get(i).copied().unwrap_or(false)
    }

    fn importable(&self) -> usize {
        (0..self.sessions.len()).filter(|&i| !self.taken(i)).count()
    }
}

/// What the user chose.
#[derive(Clone, Debug, PartialEq)]
pub enum ImportAction {
    /// Pick a `WinSCP.ini` or a file of KiTTY's portable sessions.
    FromFile,
    /// Write these sessions as `client`'s import.
    Import {
        client: Client,
        sessions: Vec<Session>,
    },
    Close,
}

#[derive(Clone, Debug, PartialEq)]
enum Row {
    Source(usize),
    FromFile,
    All,
    Session(usize),
    Note(String),
    Blank,
    Buttons,
}

const IMPORT: usize = 0;
const BACK: usize = 1;

pub struct ImportView {
    found: Vec<Found>,
    /// Files that were found but could not be read, said on the first page.
    problems: Vec<String>,
    /// The source being previewed; `None` on the list of sources.
    open: Option<usize>,
    checked: Vec<bool>,
    focus: usize,
    scroll: usize,
    button: usize,
    button_cols: Vec<(usize, usize)>,
}

impl ImportView {
    pub fn new(found: Vec<Found>, problems: Vec<String>) -> Self {
        let mut v = Self {
            found,
            problems,
            open: None,
            checked: Vec::new(),
            focus: 0,
            scroll: 0,
            button: IMPORT,
            button_cols: Vec::new(),
        };
        v.focus_first();
        v
    }

    /// Adds sessions read from a file and opens their preview. They
    /// replace what an earlier pick of the same file found.
    pub fn add(&mut self, found: Found) {
        self.found
            .retain(|f| !(f.client == found.client && f.origin == found.origin));
        self.found.push(found);
        self.open_source(self.found.len() - 1);
    }

    fn open_source(&mut self, i: usize) {
        let Some(f) = self.found.get(i) else {
            return;
        };
        self.checked = (0..f.sessions.len()).map(|s| !f.taken(s)).collect();
        self.open = Some(i);
        self.scroll = 0;
        self.button = IMPORT;
        self.focus_first();
    }

    fn back(&mut self) {
        let was = self.open.take();
        self.scroll = 0;
        self.focus = self
            .rows()
            .iter()
            .position(|r| Some(r) == was.map(Row::Source).as_ref())
            .unwrap_or(0);
    }

    fn focus_first(&mut self) {
        let rows = self.rows();
        self.focus = (0..rows.len())
            .find(|&i| self.focusable(rows.get(i)))
            .unwrap_or(0);
    }

    fn current(&self) -> Option<&Found> {
        self.open.and_then(|i| self.found.get(i))
    }

    fn rows(&self) -> Vec<Row> {
        let Some(f) = self.current() else {
            let mut r: Vec<Row> = (0..self.found.len()).map(Row::Source).collect();
            if self.found.is_empty() {
                r.push(Row::Note("No other SSH client has sessions here.".into()));
            }
            r.extend(self.problems.iter().cloned().map(Row::Note));
            if self.found.is_empty() || !self.problems.is_empty() {
                r.push(Row::Blank);
            }
            r.push(Row::FromFile);
            r.push(Row::Note("WinSCP.ini, MobaXterm.ini, confCons.xml,".into()));
            r.push(Row::Note("or a SecureCRT or KiTTY session file".into()));
            r.push(Row::Blank);
            r.push(Row::Note("Passwords are never imported.".into()));
            return r;
        };
        let mut r = vec![Row::All];
        for (i, s) in f.sessions.iter().enumerate() {
            r.push(Row::Session(i));
            if f.taken(i) {
                r.push(Row::Note(format!("{} is already a host", s.host.alias)));
                continue;
            }
            if s.had_password {
                r.push(Row::Note("its saved password stays behind".into()));
            }
            for n in &s.notes {
                r.push(Row::Note(n.clone()));
            }
        }
        r.push(Row::Blank);
        r.push(Row::Buttons);
        r.push(Row::Note(format!(
            "Saved to {}, replacing an earlier {} import.",
            f.client.file_name(),
            f.client.name()
        )));
        r
    }

    fn focusable(&self, row: Option<&Row>) -> bool {
        match row {
            Some(Row::Source(_) | Row::FromFile | Row::All | Row::Buttons) => true,
            Some(Row::Session(i)) => self.current().is_some_and(|f| !f.taken(*i)),
            _ => false,
        }
    }

    fn chosen(&self) -> usize {
        self.checked.iter().filter(|&&c| c).count()
    }

    fn move_focus(&mut self, down: bool, steps: usize) {
        let rows = self.rows();
        for _ in 0..steps {
            let next = if down {
                (self.focus + 1..rows.len()).find(|&i| self.focusable(rows.get(i)))
            } else {
                (0..self.focus).rev().find(|&i| self.focusable(rows.get(i)))
            };
            match next {
                Some(i) => self.focus = i,
                None => break,
            }
        }
    }

    pub fn key(&mut self, key: SideKey, rows: usize) -> Option<ImportAction> {
        let visible = rows.saturating_sub(1).max(1);
        let action = match key {
            SideKey::Up => {
                self.move_focus(false, 1);
                None
            }
            SideKey::Down | SideKey::Tab => {
                self.move_focus(true, 1);
                None
            }
            SideKey::PageUp => {
                self.move_focus(false, visible);
                None
            }
            SideKey::PageDown => {
                self.move_focus(true, visible);
                None
            }
            SideKey::Home => {
                self.move_focus(false, usize::MAX);
                None
            }
            SideKey::End => {
                self.move_focus(true, usize::MAX);
                None
            }
            SideKey::Left | SideKey::Right
                if self.rows().get(self.focus) == Some(&Row::Buttons) =>
            {
                self.button = if key == SideKey::Left { IMPORT } else { BACK };
                None
            }
            SideKey::Escape | SideKey::Backspace if self.open.is_some() => {
                self.back();
                None
            }
            SideKey::Escape => Some(ImportAction::Close),
            SideKey::Enter { .. } | SideKey::Char(' ') => self.activate(),
            _ => None,
        };
        self.clamp(rows);
        action
    }

    fn activate(&mut self) -> Option<ImportAction> {
        match self.rows().get(self.focus)? {
            Row::Source(i) => {
                self.open_source(*i);
                None
            }
            Row::FromFile => Some(ImportAction::FromFile),
            Row::All => {
                let f = self.current()?;
                let on = self.chosen() < f.importable();
                let taken: Vec<bool> = (0..f.sessions.len()).map(|i| f.taken(i)).collect();
                for (c, t) in self.checked.iter_mut().zip(taken) {
                    *c = on && !t;
                }
                None
            }
            Row::Session(i) => {
                if let Some(c) = self.checked.get_mut(*i) {
                    *c = !*c;
                }
                None
            }
            Row::Buttons if self.button == BACK => {
                self.back();
                None
            }
            Row::Buttons => {
                let f = self.current()?;
                let sessions: Vec<Session> = f
                    .sessions
                    .iter()
                    .zip(&self.checked)
                    .filter(|(_, c)| **c)
                    .map(|(s, _)| s.clone())
                    .collect();
                let client = f.client;
                (!sessions.is_empty()).then_some(ImportAction::Import { client, sessions })
            }
            Row::Note(_) | Row::Blank => None,
        }
    }

    /// A click on grid row `y`, column `x`.
    pub fn click(&mut self, x: usize, y: usize, rows: usize) -> Option<ImportAction> {
        let i = y.checked_sub(1)? + self.scroll;
        let all = self.rows();
        let row = all.get(i)?;
        if !self.focusable(Some(row)) {
            return None;
        }
        self.focus = i;
        if *row == Row::Buttons {
            self.button = self
                .button_cols
                .iter()
                .position(|&(a, b)| (a..b).contains(&x))?;
        }
        let action = self.activate();
        self.clamp(rows);
        action
    }

    pub fn scroll_by(&mut self, lines: i32, rows: usize) {
        let visible = rows.saturating_sub(1).max(1);
        let max = self.rows().len().saturating_sub(visible);
        self.scroll = if lines > 0 {
            self.scroll.saturating_sub(lines as usize)
        } else {
            (self.scroll + lines.unsigned_abs() as usize).min(max)
        };
    }

    fn clamp(&mut self, rows: usize) {
        let n = self.rows().len();
        self.focus = self.focus.min(n.saturating_sub(1));
        let visible = rows.saturating_sub(1).max(1);
        if self.focus < self.scroll {
            self.scroll = self.focus;
        }
        // Keep the line under the buttons in view too.
        let bottom = if self.rows().get(self.focus) == Some(&Row::Buttons) {
            (self.focus + 1).min(n.saturating_sub(1))
        } else {
            self.focus
        };
        if bottom >= self.scroll + visible {
            self.scroll = bottom + 1 - visible;
        }
    }

    /// Draws the page into `grid`.
    pub fn draw(&mut self, grid: &mut Grid, palette: &Palette, focused: bool) {
        let width = grid.cols();
        let rows = grid.rows();
        self.clamp(rows);
        let bg = palette.strip;
        let title = match self.current() {
            Some(f) => format!(" Import from {}", f.client.name()),
            None => " Import sessions".to_owned(),
        };
        grid.set_row(
            0,
            Line::new(width, bg)
                .put(&title, palette.text, Flags::BOLD)
                .cells(bg),
        );
        let all = self.rows();
        self.button_cols.clear();
        for y in 0..rows.saturating_sub(1) {
            let i = self.scroll + y;
            let here = focused && i == self.focus;
            let row_bg = if here { palette.active } else { bg };
            let mut line = Line::new(width, row_bg);
            match all.get(i) {
                Some(Row::Source(s)) => {
                    if let Some(f) = self.found.get(*s) {
                        let n = f.sessions.len();
                        let what = format!("{n} session{}", if n == 1 { "" } else { "s" });
                        let origin = if f.origin == "registry" {
                            String::new()
                        } else {
                            format!(", {}", f.origin)
                        };
                        let name = format!(" ▸ {}", f.client.name());
                        line.put(&name, palette.text, Flags::BOLD);
                        let rest = width.saturating_sub(line.col + 2);
                        line.put(
                            &format!("  {}", fit(&format!("{what}{origin}"), rest)),
                            palette.dim,
                            Flags::empty(),
                        );
                    }
                }
                Some(Row::FromFile) => {
                    line.put("   ", palette.text, Flags::empty());
                    line.put("From a file…", palette.text, Flags::UNDERLINE);
                }
                Some(Row::All) => {
                    let total = self.current().map_or(0, Found::importable);
                    let n = self.chosen();
                    let mark = match n {
                        0 => "[ ]",
                        n if n == total => "[x]",
                        _ => "[-]",
                    };
                    line.put(
                        &format!(" {mark} All ({n} of {total})"),
                        palette.text,
                        Flags::BOLD,
                    );
                }
                Some(Row::Session(s)) => {
                    if let Some(f) = self.current() {
                        let session = f.sessions.get(*s);
                        let taken = f.taken(*s);
                        let on = self.checked.get(*s).copied().unwrap_or(false);
                        let mark = if taken {
                            "   "
                        } else if on {
                            "[x]"
                        } else {
                            "[ ]"
                        };
                        let fg = if taken { palette.dim } else { palette.text };
                        let name = session.map(|s| s.name.as_str()).unwrap_or_default();
                        line.put(&format!(" {mark} "), fg, Flags::empty());
                        let room = width.saturating_sub(line.col + 1);
                        line.put(&fit(name, room), fg, Flags::empty());
                        if let Some(s) = session {
                            let mut to = s.host.hostname.clone().unwrap_or_default();
                            if let Some(u) = &s.host.user {
                                to = format!("{u}@{to}");
                            }
                            if let Some(p) = s.host.port {
                                to.push_str(&format!(":{p}"));
                            }
                            match s.kind {
                                SessionKind::Ssh => {}
                                SessionKind::Rdp => to.push_str(" RDP"),
                                SessionKind::Vnc => to.push_str(" VNC"),
                                SessionKind::Serial => to.push_str(" serial"),
                                SessionKind::Telnet => to.push_str(" Telnet"),
                            }
                            let room = width.saturating_sub(line.col + 2);
                            if room > 4 {
                                line.put(
                                    &format!("  {}", fit(&to, room)),
                                    palette.dim,
                                    Flags::empty(),
                                );
                            }
                        }
                    }
                }
                Some(Row::Note(text)) => {
                    let room = width.saturating_sub(6);
                    line.put(
                        &format!("     {}", fit(text, room)),
                        palette.dim,
                        Flags::ITALIC,
                    );
                }
                Some(Row::Buttons) => {
                    line.put(" ", palette.text, Flags::empty());
                    let labels = [format!("Import {}", self.chosen()), "Back".to_owned()];
                    for (b, label) in labels.iter().enumerate() {
                        let on = here && b == self.button;
                        let start = line.col;
                        let flags = if on {
                            Flags::BOLD.union(Flags::INVERSE)
                        } else {
                            Flags::BOLD
                        };
                        line.put(&format!(" {label} "), palette.text, flags);
                        self.button_cols.push((start, line.col));
                        line.put(" ", palette.text, Flags::empty());
                    }
                }
                Some(Row::Blank) | None => {}
            }
            grid.set_row(1 + y, line.cells(row_bg));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use ferossh_config::import::Value;
    use ferossh_config::winscp;
    use ferossh_render::Theme;

    fn site(name: &str, host: &str, password: bool) -> Session {
        let mut s = winscp::session(name, |k| match k {
            "HostName" => Some(Value::Text(host.into())),
            "Password" if password => Some(Value::Number(1)),
            _ => None,
        })
        .unwrap();
        s.host.user = Some("me".into());
        s
    }

    fn view() -> ImportView {
        ImportView::new(
            vec![Found {
                client: Client::WinScp,
                origin: "registry".into(),
                sessions: vec![
                    site("Work/web", "web.lan", true),
                    site("db", "db.lan", false),
                    site("files", "files.lan", false),
                ],
                taken: vec![false, true, false],
            }],
            Vec::new(),
        )
    }

    fn texts(v: &mut ImportView) -> Vec<String> {
        let mut grid = Grid::new(48, 16);
        v.draw(&mut grid, &Palette::of(&Theme::default()), true);
        (0..16)
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
    fn lists_sources_then_previews_with_boxes() {
        let mut v = view();
        let t = texts(&mut v);
        assert_eq!(t[0], " Import sessions");
        assert_eq!(t[1], " ▸ WinSCP  3 sessions");
        assert_eq!(t[2], "   From a file…");
        assert_eq!(v.key(SideKey::Enter { split: false }, 16), None);
        let t = texts(&mut v);
        assert_eq!(t[0], " Import from WinSCP");
        assert_eq!(t[1], " [x] All (2 of 2)");
        assert_eq!(t[2], " [x] Work/web  me@web.lan");
        assert_eq!(t[3], "     its saved password stays behind");
        assert_eq!(t[4], "     db  me@db.lan");
        assert_eq!(t[5], "     db is already a host");
        assert_eq!(t[6], " [x] files  me@files.lan");
        assert_eq!(t[8], "  Import 2   Back");
        // Down skips the taken session.
        v.key(SideKey::Down, 16);
        v.key(SideKey::Down, 16);
        v.key(SideKey::Char(' '), 16);
        let t = texts(&mut v);
        assert_eq!(t[1], " [-] All (1 of 2)");
        assert_eq!(t[6], " [ ] files  me@files.lan");
        v.key(SideKey::Down, 16);
        match v.key(SideKey::Enter { split: false }, 16) {
            Some(ImportAction::Import { client, sessions }) => {
                assert_eq!(client, Client::WinScp);
                let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
                assert_eq!(names, ["Work/web"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn all_toggles_and_escape_goes_back() {
        let mut v = view();
        v.key(SideKey::Enter { split: false }, 16);
        v.key(SideKey::Char(' '), 16);
        assert_eq!(texts(&mut v)[1], " [ ] All (0 of 2)");
        v.key(SideKey::End, 16);
        // Nothing chosen: Import does nothing.
        assert_eq!(v.key(SideKey::Enter { split: false }, 16), None);
        v.key(SideKey::Right, 16);
        v.key(SideKey::Enter { split: false }, 16);
        assert_eq!(texts(&mut v)[0], " Import sessions");
        v.key(SideKey::Enter { split: false }, 16);
        v.key(SideKey::Escape, 16);
        assert_eq!(texts(&mut v)[0], " Import sessions");
        assert_eq!(v.key(SideKey::Escape, 16), Some(ImportAction::Close));
        v.key(SideKey::Down, 16);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, 16),
            Some(ImportAction::FromFile)
        );
    }

    #[test]
    fn a_file_opens_its_preview_and_clicks_work() {
        let mut v = ImportView::new(Vec::new(), vec!["confCons.xml is encrypted".into()]);
        let t = texts(&mut v);
        assert_eq!(t[1], "     No other SSH client has sessions here.");
        assert_eq!(t[2], "     confCons.xml is encrypted");
        assert_eq!(t[4], "   From a file…");
        let mut desk = site("desk", "desk.lan", false);
        desk.kind = SessionKind::Vnc;
        desk.host.port = Some(5901);
        v.add(Found {
            client: Client::Kitty,
            origin: r"D:\kitty\Sessions".into(),
            sessions: vec![site("box", "box.lan", false), desk],
            taken: vec![false, false],
        });
        let t = texts(&mut v);
        assert_eq!(t[0], " Import from KiTTY");
        assert_eq!(t[3], " [x] desk  me@desk.lan:5901 VNC");
        // Click the box, then Import.
        v.click(2, 2, 16);
        assert_eq!(texts(&mut v)[2], " [ ] box  me@box.lan");
        v.click(2, 2, 16);
        let t = texts(&mut v);
        let y = t.iter().position(|l| l.contains("Import 2")).unwrap();
        assert!(matches!(
            v.click(3, y, 16),
            Some(ImportAction::Import { .. })
        ));
        let back = t[y].find("Back").unwrap();
        v.click(back, y, 16);
        assert_eq!(texts(&mut v)[1], r" ▸ KiTTY  2 sessions, D:\kitty\Sessions");
    }
}
