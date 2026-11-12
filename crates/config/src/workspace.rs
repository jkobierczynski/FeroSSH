//! Workspaces: saved sets of tabs and split panes, each pane on a
//! connection, in `%APPDATA%\FeroSSH\workspaces`, one file per workspace.
//!
//! A workspace names its connections; it never copies a host's address,
//! user, keys or passwords. Editing a connection changes every workspace
//! that uses it, and a workspace file holds nothing secret, so it can be
//! copied to another PC, kept in git or synced.
//!
//! A file lists its tabs; each tab's split tree is written top-down, a
//! `Split` followed by its two halves (left or top first), and each pane's
//! settings follow its `Pane` line. Indentation is only for reading:
//!
//! ```text
//! Active 1
//! Tab
//!     Split right 60
//!         Pane connection web1
//!             Directory /var/log
//!             Name logs
//!             Focus
//!         Split down 50
//!             Pane connection db1
//!             Pane local
//! Tab
//!     Pane prompt
//! ```
//!
//! `Split right` puts the halves side by side, `Split down` one above the
//! other; the number is the first half's share in percent (10 to 90). A
//! pane opens `connection NAME` (a saved connection, a `Host` from
//! ssh_config, or anything the connect prompt takes), a `local` shell, or
//! the connect `prompt`. A pane's `Directory` is where its shell starts,
//! `Name` the session's name, `Theme`, `Font` and `FontSize` its look
//! where it differs from its host's, and `Focus` marks the tab's focused
//! pane. `Active` is the tab shown first, counted from 1.
//!
//! Unknown keywords are skipped, so a newer FeroSSH's workspace still
//! opens here.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use crate::snippet_store::file_part;

/// What a workspace file's name ends in.
pub const EXTENSION: &str = "workspace";
/// Tabs in one workspace at most.
pub const MAX_TABS: usize = 50;
/// Panes in one workspace at most, over all its tabs.
pub const MAX_PANES: usize = 200;
/// Splits within splits at most.
pub const MAX_DEPTH: usize = 16;
/// The biggest workspace file read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// The longest value kept (a connection, directory or name).
const MAX_VALUE: usize = 512;
/// Workspace files listed at most.
const MAX_LISTED: usize = 500;

/// What a pane opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    /// A connection by name, or anything the connect prompt takes.
    Connection(String),
    /// A local shell.
    Local,
    /// The connect prompt.
    Prompt,
}

/// One pane of a workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneSpec {
    pub open: Open,
    /// Where its shell starts.
    pub dir: Option<String>,
    /// The session's name (Window > Rename session).
    pub name: Option<String>,
    /// A colour theme other than its host's.
    pub theme: Option<String>,
    /// A font family other than its host's.
    pub font: Option<String>,
    /// A font size in points other than its host's.
    pub font_size: Option<f32>,
}

impl PaneSpec {
    pub fn new(open: Open) -> Self {
        Self {
            open,
            dir: None,
            name: None,
            theme: None,
            font: None,
            font_size: None,
        }
    }
}

/// A tab's split tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    Pane(PaneSpec),
    Split {
        /// One above the other; else side by side.
        down: bool,
        /// The first half's share, 0.1 to 0.9.
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    /// The panes, first to last (left to right, top to bottom).
    pub fn panes(&self) -> Vec<&PaneSpec> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a PaneSpec>) {
        match self {
            Layout::Pane(p) => out.push(p),
            Layout::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    fn depth(&self) -> usize {
        match self {
            Layout::Pane(_) => 0,
            Layout::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
        }
    }
}

/// One tab: its split tree and which pane (counted in [`Layout::panes`]
/// order) has the keyboard.
#[derive(Clone, Debug, PartialEq)]
pub struct TabSpec {
    pub root: Layout,
    pub focus: usize,
}

/// A workspace: its tabs and the one shown first.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Workspace {
    pub tabs: Vec<TabSpec>,
    pub active: usize,
}

impl Workspace {
    /// The panes in all tabs.
    pub fn pane_count(&self) -> usize {
        self.tabs.iter().map(|t| t.root.panes().len()).sum()
    }
}

/// A split ratio as written: percent, 10 to 90.
fn percent(ratio: f32) -> u32 {
    let r = if ratio.is_finite() { ratio } else { 0.5 };
    (r.clamp(0.1, 0.9) * 100.0).round() as u32
}

/// A value safe to keep: no control characters, trimmed, at most
/// [`MAX_VALUE`] characters. `None` when nothing is left.
fn clean(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() || v.chars().any(char::is_control) || v.chars().count() > MAX_VALUE {
        return None;
    }
    Some(v.to_owned())
}

/// The workspace as its file's text.
pub fn to_text(ws: &Workspace) -> String {
    let mut out = String::from(
        "# A FeroSSH workspace: tabs and split panes, each pane on a connection.\n\
         # It names connections and holds no passwords. Copy it into another\n\
         # FeroSSH's workspaces folder to bring it along.\n",
    );
    let _ = writeln!(
        out,
        "Active {}",
        ws.active.min(ws.tabs.len().saturating_sub(1)) + 1
    );
    for tab in &ws.tabs {
        out.push_str("Tab\n");
        let mut index = 0;
        write_layout(&mut out, &tab.root, 1, tab.focus, &mut index);
    }
    out
}

fn write_layout(out: &mut String, l: &Layout, level: usize, focus: usize, index: &mut usize) {
    let pad = "    ".repeat(level);
    match l {
        Layout::Split {
            down,
            ratio,
            first,
            second,
        } => {
            let dir = if *down { "down" } else { "right" };
            let _ = writeln!(out, "{pad}Split {dir} {}", percent(*ratio));
            write_layout(out, first, level + 1, focus, index);
            write_layout(out, second, level + 1, focus, index);
        }
        Layout::Pane(p) => {
            match &p.open {
                Open::Connection(c) => match clean(c) {
                    Some(c) => {
                        let _ = writeln!(out, "{pad}Pane connection {c}");
                    }
                    None => {
                        let _ = writeln!(out, "{pad}Pane prompt");
                    }
                },
                Open::Local => {
                    let _ = writeln!(out, "{pad}Pane local");
                }
                Open::Prompt => {
                    let _ = writeln!(out, "{pad}Pane prompt");
                }
            }
            let fields = [
                ("Directory", p.dir.as_deref()),
                ("Name", p.name.as_deref()),
                ("Theme", p.theme.as_deref()),
                ("Font", p.font.as_deref()),
            ];
            for (key, value) in fields {
                if let Some(v) = value.and_then(clean) {
                    let _ = writeln!(out, "{pad}    {key} {v}");
                }
            }
            if let Some(size) = p.font_size.filter(|s| s.is_finite() && *s > 0.0) {
                let _ = writeln!(out, "{pad}    FontSize {size}");
            }
            if *index == focus {
                let _ = writeln!(out, "{pad}    Focus");
            }
            *index += 1;
        }
    }
}

/// A line of the file, as read.
enum Item {
    Split { down: bool, ratio: f32 },
    Pane(PaneSpec, bool),
}

/// Reads a workspace file's text. Every tab needs a complete split tree;
/// a mistake says which line.
pub fn parse(text: &str) -> Result<Workspace, String> {
    let mut active = 1usize;
    let mut tabs: Vec<Vec<Item>> = Vec::new();
    let mut panes = 0usize;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = n + 1;
        let (key, rest) = line
            .split_once(char::is_whitespace)
            .map_or((line, ""), |(k, r)| (k, r.trim()));
        let key = key.to_ascii_lowercase();
        match key.as_str() {
            "active" => {
                active = rest
                    .parse()
                    .map_err(|_| format!("Line {at}: Active needs a tab number."))?;
            }
            "tab" => {
                if tabs.len() >= MAX_TABS {
                    return Err(format!("A workspace has at most {MAX_TABS} tabs."));
                }
                tabs.push(Vec::new());
            }
            "split" => {
                let tab = tabs
                    .last_mut()
                    .ok_or_else(|| format!("Line {at}: Split comes before any Tab."))?;
                let mut words = rest.split_whitespace();
                let down = match words.next().map(str::to_ascii_lowercase).as_deref() {
                    Some("right") => false,
                    Some("down") => true,
                    _ => return Err(format!("Line {at}: Split needs right or down.")),
                };
                let ratio = match words.next() {
                    None => 0.5,
                    Some(w) => {
                        let p: f32 = w
                            .trim_end_matches('%')
                            .parse()
                            .map_err(|_| format!("Line {at}: {w} is not a percentage."))?;
                        if !p.is_finite() {
                            return Err(format!("Line {at}: {w} is not a percentage."));
                        }
                        (p.round().clamp(10.0, 90.0)) / 100.0
                    }
                };
                tab.push(Item::Split { down, ratio });
            }
            "pane" => {
                let tab = tabs
                    .last_mut()
                    .ok_or_else(|| format!("Line {at}: Pane comes before any Tab."))?;
                panes += 1;
                if panes > MAX_PANES {
                    return Err(format!("A workspace has at most {MAX_PANES} panes."));
                }
                let (kind, value) = rest
                    .split_once(char::is_whitespace)
                    .map_or((rest, ""), |(k, v)| (k, v.trim()));
                let open = match kind.to_ascii_lowercase().as_str() {
                    "connection" => Open::Connection(clean(value).ok_or_else(|| {
                        format!("Line {at}: Pane connection needs a connection's name.")
                    })?),
                    "local" => Open::Local,
                    "prompt" | "" => Open::Prompt,
                    other => {
                        return Err(format!(
                            "Line {at}: a pane opens a connection, local or prompt, not {other}."
                        ));
                    }
                };
                tab.push(Item::Pane(PaneSpec::new(open), false));
            }
            "directory" | "name" | "theme" | "font" | "fontsize" | "focus" => {
                let Some(Item::Pane(p, focus)) = tabs.last_mut().and_then(|t| t.last_mut()) else {
                    return Err(format!("Line {at}: {key} belongs right after a Pane."));
                };
                match key.as_str() {
                    "focus" => *focus = true,
                    "fontsize" => {
                        let size: f32 = rest
                            .parse()
                            .map_err(|_| format!("Line {at}: {rest} is not a font size."))?;
                        if !size.is_finite() || size <= 0.0 {
                            return Err(format!("Line {at}: {rest} is not a font size."));
                        }
                        p.font_size = Some(size);
                    }
                    _ => {
                        let v = clean(rest)
                            .ok_or_else(|| format!("Line {at}: {key} needs a value."))?;
                        match key.as_str() {
                            "directory" => p.dir = Some(v),
                            "name" => p.name = Some(v),
                            "theme" => p.theme = Some(v),
                            _ => p.font = Some(v),
                        }
                    }
                }
            }
            // A newer FeroSSH's setting.
            _ => {}
        }
    }
    let mut out = Workspace::default();
    for (i, items) in tabs.into_iter().enumerate() {
        let n = i + 1;
        if items.is_empty() {
            return Err(format!("Tab {n} has no panes."));
        }
        let mut at = 0;
        let mut focus = 0;
        let mut index = 0;
        let root = build(&items, &mut at, 0, &mut index, &mut focus)
            .ok_or_else(|| format!("Tab {n} has a Split without both halves."))?;
        if at != items.len() {
            return Err(format!(
                "Tab {n} has more panes than its splits hold: start another Tab or add a Split."
            ));
        }
        out.tabs.push(TabSpec { root, focus });
    }
    if out.tabs.is_empty() {
        return Err("The workspace has no tabs.".into());
    }
    out.active = active.clamp(1, out.tabs.len()) - 1;
    Ok(out)
}

fn build(
    items: &[Item],
    at: &mut usize,
    depth: usize,
    index: &mut usize,
    focus: &mut usize,
) -> Option<Layout> {
    let item = items.get(*at)?;
    *at += 1;
    match item {
        Item::Pane(p, f) => {
            if *f {
                *focus = *index;
            }
            *index += 1;
            Some(Layout::Pane(p.clone()))
        }
        Item::Split { down, ratio } => {
            if depth >= MAX_DEPTH {
                return None;
            }
            let first = build(items, at, depth + 1, index, focus)?;
            let second = build(items, at, depth + 1, index, focus)?;
            Some(Layout::Split {
                down: *down,
                ratio: *ratio,
                first: Box::new(first),
                second: Box::new(second),
            })
        }
    }
}

/// Whether `ws` stays within the limits [`parse`] keeps, so it reads back.
pub fn fits(ws: &Workspace) -> bool {
    !ws.tabs.is_empty()
        && ws.tabs.len() <= MAX_TABS
        && ws.pane_count() <= MAX_PANES
        && ws.tabs.iter().all(|t| t.root.depth() <= MAX_DEPTH)
}

/// The workspaces in `dir` by name, sorted ignoring case. A missing
/// folder has none.
pub fn list(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (stem, ext) = name.rsplit_once('.')?;
            (ext.eq_ignore_ascii_case(EXTENSION) && !stem.is_empty()).then(|| stem.to_owned())
        })
        .take(MAX_LISTED)
        .collect();
    out.sort_by_cached_key(|n| n.to_lowercase());
    out
}

/// The file workspace `name` is kept in, in `dir`; `None` for a name
/// that leaves no file name.
pub fn path_for(dir: &Path, name: &str) -> Option<PathBuf> {
    let part = file_part(name);
    (!part.is_empty()).then(|| dir.join(format!("{part}.{EXTENSION}")))
}

/// The file of workspace `name` exactly as [`list`] gave it, or `None`
/// when that file is not there: never another file (deleting uses it).
pub fn listed(dir: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name == "." || name == ".." {
        return None;
    }
    let exact = dir.join(format!("{name}.{EXTENSION}"));
    exact.is_file().then_some(exact)
}

/// The file of workspace `name` as [`list`] gave it: that exact file
/// when it is there, else the one [`path_for`] names.
pub fn existing(dir: &Path, name: &str) -> Option<PathBuf> {
    listed(dir, name).or_else(|| path_for(dir, name))
}

/// The workspace in file `path`.
pub fn read(path: &Path) -> Result<Workspace, String> {
    let shown = path.display();
    match std::fs::metadata(path) {
        Ok(m) if m.len() > MAX_FILE_BYTES => {
            return Err(format!("{shown} is too big for a workspace."));
        }
        Ok(_) => {}
        Err(e) => return Err(format!("Could not read {shown}: {e}")),
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read {shown}: {e}"))?;
    parse(&String::from_utf8_lossy(&bytes)).map_err(|e| format!("{shown}: {e}"))
}

/// Writes `ws` to `path` through a temporary file of its own, so a crash
/// never leaves half a file and two windows saving at once never mix
/// theirs. A workspace too big to read back is refused, leaving the file
/// as it was, and so is one with a connection it could not keep (too
/// long, or with control characters), rather than saving it as a prompt.
pub fn write(path: &Path, ws: &Workspace) -> io::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU32, Ordering};
    static SAVES: AtomicU32 = AtomicU32::new(0);
    if let Some(c) = ws
        .tabs
        .iter()
        .flat_map(|t| t.root.panes())
        .find_map(|p| match &p.open {
            Open::Connection(c) if clean(c).is_none() => Some(c),
            _ => None,
        })
    {
        let shown: String = c
            .trim()
            .chars()
            .take(40)
            .filter(|ch| !ch.is_control())
            .collect();
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the connection {shown}... is too long to keep (at most {MAX_VALUE} characters)"
            ),
        ));
    }
    let text = to_text(ws);
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the workspace is too big to save: use shorter directories or fewer panes",
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n = SAVES.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("workspace-{}-{n}.tmp", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn pane(open: Open) -> Layout {
        Layout::Pane(PaneSpec::new(open))
    }

    fn sample() -> Workspace {
        let mut logs = PaneSpec::new(Open::Connection("web1".into()));
        logs.dir = Some("/var/log".into());
        logs.name = Some("logs".into());
        logs.theme = Some("Dracula".into());
        logs.font = Some("Cascadia Mono".into());
        logs.font_size = Some(11.5);
        Workspace {
            tabs: vec![
                TabSpec {
                    root: Layout::Split {
                        down: false,
                        ratio: 0.6,
                        first: Box::new(Layout::Pane(logs)),
                        second: Box::new(Layout::Split {
                            down: true,
                            ratio: 0.5,
                            first: Box::new(pane(Open::Connection("admin@db1:2222".into()))),
                            second: Box::new(pane(Open::Local)),
                        }),
                    },
                    focus: 2,
                },
                TabSpec {
                    root: pane(Open::Prompt),
                    focus: 0,
                },
            ],
            active: 1,
        }
    }

    #[test]
    fn a_workspace_reads_back_as_written() {
        let ws = sample();
        let text = to_text(&ws);
        assert_eq!(parse(&text).unwrap(), ws);
        assert!(text.contains("Split right 60"), "{text}");
        assert!(text.contains("Pane connection admin@db1:2222"), "{text}");
        assert!(text.contains("Active 2"), "{text}");
    }

    #[test]
    fn the_documented_example_reads() {
        let text = "Active 1\nTab\n    Split right 60\n        Pane connection web1\n\
                    \x20           Directory /var/log\n            Focus\n\
                    \x20       Split down 50\n            Pane connection db1\n\
                    \x20           Pane local\nTab\n    Pane prompt\n";
        let ws = parse(text).unwrap();
        assert_eq!(ws.tabs.len(), 2);
        let panes = ws.tabs[0].root.panes();
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[0].dir.as_deref(), Some("/var/log"));
        assert_eq!(panes[2].open, Open::Local);
        assert_eq!(ws.tabs[0].focus, 0);
    }

    #[test]
    fn broken_trees_say_which_tab() {
        assert!(
            parse("Tab\nSplit right\nPane local\n")
                .unwrap_err()
                .contains("Tab 1")
        );
        assert!(
            parse("Tab\nPane local\nPane local\n")
                .unwrap_err()
                .contains("Tab 1")
        );
        assert!(parse("Pane local\n").unwrap_err().contains("Line 1"));
        assert!(parse("Tab\nSplit sideways\nPane local\nPane local\n").is_err());
        assert!(parse("# nothing\n").is_err());
        assert!(parse("Tab\n").is_err());
    }

    #[test]
    fn values_with_control_characters_are_refused() {
        assert!(parse("Tab\nPane connection a\u{1b}[2J\n").is_err());
        assert!(parse("Tab\nPane local\nDirectory /tmp\r\u{7}\n").is_err());
        // Never written either.
        let mut p = PaneSpec::new(Open::Connection("x\ny".into()));
        p.dir = Some("a\u{1b}b".into());
        let ws = Workspace {
            tabs: vec![TabSpec {
                root: Layout::Pane(p),
                focus: 0,
            }],
            active: 0,
        };
        let back = parse(&to_text(&ws)).unwrap();
        assert_eq!(back.tabs[0].root.panes()[0].open, Open::Prompt);
        assert_eq!(back.tabs[0].root.panes()[0].dir, None);
    }

    #[test]
    fn unknown_keywords_are_skipped_and_ratios_clamped() {
        let ws = parse("Tab\nScript later\nSplit down 99\nPane local\nColour red\nPane prompt\n")
            .unwrap();
        let Layout::Split { down, ratio, .. } = &ws.tabs[0].root else {
            unreachable!("not a split");
        };
        assert!(*down);
        assert!((*ratio - 0.9).abs() < 1e-6);
        assert!(parse("Tab\nSplit down NaN\nPane local\nPane local\n").is_err());
    }

    #[test]
    fn limits_hold() {
        let deep = format!("Tab\n{}Pane local\n", "Split right\n".repeat(MAX_DEPTH + 1))
            + &"Pane local\n".repeat(MAX_DEPTH + 1);
        assert!(parse(&deep).is_err());
        let many = "Tab\nPane local\n".repeat(MAX_TABS + 1);
        assert!(parse(&many).is_err());
        assert!(parse("Active 7\nTab\nPane local\n").unwrap().active == 0);
    }

    #[test]
    fn files_are_listed_by_name_and_names_made_safe() {
        let dir = std::env::temp_dir().join(format!("ferossh-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(list(&dir).is_empty());
        let path = path_for(&dir, "Web: prod/1").unwrap();
        assert_eq!(path.file_name().unwrap(), "Web_ prod_1.workspace");
        write(&path, &sample()).unwrap();
        write(&path_for(&dir, "alpha").unwrap(), &sample()).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        assert_eq!(
            list(&dir),
            vec!["alpha".to_owned(), "Web_ prod_1".to_owned()]
        );
        assert_eq!(read(&path).unwrap(), sample());
        assert!(path_for(&dir, "..").is_none());
        // A listed name opens its own file, even one path_for would change.
        std::fs::write(dir.join(".prod.workspace"), to_text(&sample())).unwrap();
        assert!(list(&dir).contains(&".prod".to_owned()));
        assert_eq!(
            existing(&dir, ".prod").unwrap().file_name().unwrap(),
            ".prod.workspace"
        );
        // Deleting a listed name never reaches another file.
        assert!(listed(&dir, ".gone").is_none());
        assert!(listed(&dir, "../alpha").is_none());
        assert!(listed(&dir, "alpha").is_some());
        // Too big to read back: refused, the file stays.
        let mut big = sample();
        let mut p = PaneSpec::new(Open::Local);
        p.dir = Some("d".repeat(512));
        big.tabs = (0..MAX_TABS)
            .map(|_| TabSpec {
                root: Layout::Split {
                    down: false,
                    ratio: 0.5,
                    first: Box::new(Layout::Pane(p.clone())),
                    second: Box::new(Layout::Split {
                        down: true,
                        ratio: 0.5,
                        first: Box::new(Layout::Pane(p.clone())),
                        second: Box::new(Layout::Pane(p.clone())),
                    }),
                },
                focus: 0,
            })
            .collect();
        assert!(fits(&big));
        assert!(write(&path, &big).is_err());
        assert_eq!(read(&path).unwrap(), sample());
        let mut long = sample();
        long.tabs[0].root = Layout::Pane(PaneSpec::new(Open::Connection(format!(
            "sftp://web1/{}",
            "d".repeat(MAX_VALUE)
        ))));
        assert!(write(&path, &long).is_err());
        assert_eq!(read(&path).unwrap(), sample());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
