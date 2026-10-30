//! The side pane's snippets view: named command lines to type into the
//! focused pane (and every pane broadcast input reaches) without typing
//! them out. Ctrl+Shift+Space opens it; typing filters the list, Enter
//! types the snippet with Enter, Shift+Enter without the last Enter.
//! Insert adds a snippet, F2 edits the selected one, Delete removes it;
//! Edit > Save selection as snippet starts one from text in a pane, and
//! Paste works in the editor, a multi-line paste giving several lines.
//!
//! A snippet may take arguments, `$1` to `$9` or `${1:default}` (see
//! [`ferossh_config::login::args`]); typing one first asks for their
//! values here, prefilled with the defaults. Variables such as `${host}`
//! fill themselves in, and `@` lines are steps: `@expect (config)#` waits
//! for the host (see [`ferossh_config::login`]).
//!
//! A snippet can be only for some hosts or session folders: the ones for
//! the focused pane's host come first, the others follow, dimmed. It can
//! have a hotkey (Ctrl+Shift+F1 to F12), and a `When` text that makes it
//! an auto-response, typed by itself when the host prints that text.
//!
//! F5 runs the selected snippet on saved hosts picked on a page of its
//! own (see [`crate::batch`]); the report of a run stays until Escape
//! closes it, and Enter on a row shows that host's session in a tab.
//!
//! Drawn into a [`Grid`] like the connection manager; keys come in as
//! [`SideKey`]s and what the user chose goes out as a [`SnipAction`].

use ferossh_config::login::{self, Arg, Place, Snippet, Step};
use ferossh_config::record::Recording;
use ferossh_render::{Flags, Grid, Rgb};

use crate::batch::{Password, PickField, PickLine, Picker, Report, RowState, Run};
use crate::layout::PaneId;
use crate::session_form::Line;
use crate::sessions::SideKey;
use crate::tabstrip::{Palette, fit};

/// Lines above the list: the title and the filter.
const HEADER: usize = 2;
/// Lines under the list: the key help.
const FOOTER: usize = 1;
/// Longest name or line one can type.
const MAX_TEXT: usize = 400;
/// Lines in one snippet.
const MAX_LINES: usize = 50;
/// The fields above a snippet's lines: name, hosts, folders, hotkey and
/// auto-response text.
const HEAD: usize = 5;
const HEAD_LABELS: [&str; HEAD] = ["Name", "Hosts", "Folders", "Hotkey", "When"];
const HEAD_HINTS: [&str; HEAD] = [
    "e.g. Show interfaces",
    "every host, or patterns: sw-* core-?",
    "every folder, or names: Network, Lab",
    "none, or F1 to F12 (Ctrl+Shift+F...)",
    "typed by hand; or host text, e.g. --More--",
];

/// Variables' values for a pane, by name (see
/// [`ferossh_config::login::VARIABLES`]); a missing one is empty.
pub type Vars = Vec<(&'static str, String)>;

fn var(vars: &Vars, name: &str) -> String {
    vars.iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// How a step shows in the "what will be typed" preview.
fn preview(step: &Step) -> String {
    match step {
        Step::Send(t) => t.clone(),
        Step::Type(t) => format!("{t}   (no Enter)"),
        other => format!("@{other}"),
    }
}

/// What the window should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnipAction {
    /// Type snippet `name`'s `lines` into the focused pane, asking for
    /// its arguments first; `enter` presses Enter after the last one too.
    Send {
        name: String,
        lines: Vec<String>,
        enter: bool,
    },
    /// Type snippet `name` as `steps`: arguments and variables are filled
    /// in.
    Type { name: String, steps: Vec<Step> },
    /// Store `snippet` in place of snippet `index`, or as a new one.
    Save {
        index: Option<usize>,
        snippet: Snippet,
    },
    /// Remove snippet `index` (the window asks first).
    Delete(usize),
    /// Put this text (the report, tab-separated) on the clipboard.
    Copy(String),
    /// Pick the hosts to run snippet `name` on (F5).
    PickHosts { name: String, lines: Vec<String> },
    /// Run snippet `name` on `hosts`, each in a session of its own.
    RunOn {
        name: String,
        hosts: Vec<Place>,
        run: Run,
    },
    /// Show this row's session in a tab.
    ShowRow(PaneId),
    /// Save the report as CSV.
    SaveCsv(String),
    /// Run the failed and stopped hosts again.
    Retry,
    /// Stop the run: queued hosts never start.
    Cancel,
    /// The report closed: its sessions that no tab shows can go.
    CloseReport(Report),
    /// Back to the terminal.
    Leave,
}

/// A snippet being made or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    /// The snippet it replaces.
    index: Option<usize>,
    /// Name, hosts, folders, hotkey and `When` text, as typed.
    head: [String; HEAD],
    /// Always ends with an empty line to type a new one in.
    lines: Vec<String>,
    /// 0.. the head fields, then the lines, then Save and Cancel.
    focus: usize,
    cursor: usize,
    error: Option<String>,
    /// A note above the fields, such as what a recording left out.
    note: Option<String>,
}

impl Draft {
    fn new(index: Option<usize>, snippet: &Snippet) -> Self {
        let mut d = Self {
            index,
            head: [
                snippet.name.clone(),
                snippet.hosts.join(" "),
                snippet.folders.join(", "),
                snippet.hotkey.map(|n| format!("F{n}")).unwrap_or_default(),
                snippet.when.clone(),
            ],
            lines: snippet.lines.clone(),
            focus: 0,
            cursor: snippet.name.chars().count(),
            error: None,
            note: None,
        };
        d.tidy();
        d
    }

    fn name(&self) -> &str {
        &self.head[0]
    }

    fn tidy(&mut self) {
        if self.lines.last().is_none_or(|l| !l.is_empty()) && self.lines.len() < MAX_LINES {
            self.lines.push(String::new());
        }
    }

    /// Fields, then the two buttons.
    fn count(&self) -> usize {
        HEAD + self.lines.len() + 2
    }

    /// Whether field `i` is one of the lines.
    fn is_line(&self, i: usize) -> bool {
        (HEAD..HEAD + self.lines.len()).contains(&i)
    }

    fn text_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            i if i < HEAD => self.head.get_mut(i),
            i => self.lines.get_mut(i - HEAD),
        }
    }

    fn text(&self, i: usize) -> Option<&String> {
        match i {
            i if i < HEAD => self.head.get(i),
            i => self.lines.get(i - HEAD),
        }
    }

    fn set_focus(&mut self, i: usize) {
        self.focus = i.min(self.count() - 1);
        self.cursor = self.text(self.focus).map_or(0, |t| t.chars().count());
    }

    fn fail(&mut self, why: String, focus: usize) -> Option<SnipAction> {
        self.error = Some(why);
        self.set_focus(focus);
        None
    }

    fn save(&mut self) -> Option<SnipAction> {
        let name = self.name().trim().to_owned();
        let mut lines = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            let l = l.trim_end();
            if l.trim().is_empty() {
                continue;
            }
            if let Some(Err(e)) = login::line_step(l) {
                return self.fail(format!("Line {}: {e}", i + 1), HEAD + i);
            }
            lines.push(l.to_owned());
        }
        if name.is_empty() {
            return self.fail("Give the snippet a name.".into(), 0);
        }
        if lines.is_empty() {
            return self.fail("Type at least one command line.".into(), HEAD);
        }
        let hotkey = match login::parse_hotkey(&self.head[3]) {
            Ok(k) => k,
            Err(e) => return self.fail(e, 3),
        };
        let snippet = Snippet {
            name,
            lines,
            hosts: self.head[1]
                .split([' ', ','])
                .filter(|h| !h.is_empty())
                .map(str::to_owned)
                .collect(),
            folders: self.head[2]
                .split(',')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect(),
            hotkey,
            when: self.head[4].trim().to_owned(),
        };
        if let Some(why) = snippet.auto_problem() {
            return self.fail(why, 4);
        }
        Some(SnipAction::Save {
            index: self.index,
            snippet,
        })
    }
}

/// A snippet waiting for its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asking {
    name: String,
    lines: Vec<String>,
    enter: bool,
    vars: Vars,
    args: Vec<Arg>,
    /// As typed, one per argument; they start as the defaults.
    values: Vec<String>,
    focus: usize,
    cursor: usize,
    /// For a run on picked hosts: the hosts, how many at once and the
    /// run's password.
    on: Option<(Vec<Place>, usize, Option<Password>)>,
}

impl Asking {
    fn set_focus(&mut self, i: usize) {
        self.focus = i.min(self.values.len().saturating_sub(1));
        self.cursor = self.values.get(self.focus).map_or(0, |v| v.chars().count());
    }

    /// The steps with the values filled in.
    fn filled(&self) -> Vec<Step> {
        login::expand(
            &self.lines,
            self.enter,
            |n| {
                self.args
                    .iter()
                    .position(|a| a.number == n)
                    .and_then(|i| self.values.get(i))
                    .cloned()
                    .unwrap_or_default()
            },
            |name| var(&self.vars, name),
        )
    }

    fn done(&mut self) -> SnipAction {
        if let Some((hosts, parallel, password)) = self.on.take() {
            return SnipAction::RunOn {
                name: self.name.clone(),
                hosts,
                run: Run {
                    lines: self.lines.clone(),
                    args: self
                        .args
                        .iter()
                        .map(|a| a.number)
                        .zip(self.values.iter().cloned())
                        .collect(),
                    parallel,
                    password,
                },
            };
        }
        SnipAction::Type {
            name: self.name.clone(),
            steps: self.filled(),
        }
    }
}

/// Inserts `c` into `text` at character `at`; returns whether it fit.
fn insert(text: &mut String, at: usize, c: char) -> bool {
    if text.chars().count() >= MAX_TEXT {
        return false;
    }
    let byte = text.char_indices().nth(at).map_or(text.len(), |(b, _)| b);
    text.insert(byte, c);
    true
}

/// Removes the character before (`back`) or at character `at`; returns
/// whether one went.
fn remove(text: &mut String, at: usize, back: bool) -> bool {
    let gone = if back { at.checked_sub(1) } else { Some(at) };
    if let Some(i) = gone
        && let Some((b, _)) = text.char_indices().nth(i)
    {
        text.remove(b);
        return true;
    }
    false
}

/// Moves a text cursor for an editing key; `false` if the key is not one.
fn edit_key(key: SideKey, text: &mut String, cursor: &mut usize) -> bool {
    let len = text.chars().count();
    match key {
        SideKey::Home => *cursor = 0,
        SideKey::End => *cursor = len,
        SideKey::Left => *cursor = cursor.saturating_sub(1),
        SideKey::Right => *cursor = (*cursor + 1).min(len),
        SideKey::Char(c) if !c.is_control() => {
            if insert(text, *cursor, c) {
                *cursor += 1;
            }
        }
        SideKey::Backspace => {
            if remove(text, *cursor, true) {
                *cursor -= 1;
            }
        }
        SideKey::Delete => {
            remove(text, *cursor, false);
        }
        _ => return false,
    }
    true
}

/// Pasted text as lines: control characters dropped, trailing spaces and
/// empty lines at the ends trimmed.
fn paste_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| {
            l.chars()
                .map(|c| if c == '\t' { ' ' } else { c })
                .filter(|c| !c.is_control())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

#[derive(Debug, Default)]
pub struct SnippetsView {
    /// Where the focused pane is connected: its snippets come first.
    place: Option<Place>,
    filter: String,
    /// The selected row among the matching snippets.
    selected: usize,
    scroll: usize,
    draft: Option<Draft>,
    asking: Option<Asking>,
    report: Option<Report>,
    /// The first report row shown.
    report_scroll: usize,
    picker: Option<Picker>,
}

/// Lines of the picker above its list: title, help, filter, at once,
/// password and a gap.
pub const PICK_HEAD: usize = 6;

impl SnippetsView {
    /// The indexes of the snippets the filter keeps: the ones for the
    /// focused pane's host first.
    fn shown(&self, all: &[Snippet]) -> Vec<usize> {
        let mut out: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, s)| s.matches(&self.filter))
            .map(|(i, _)| i)
            .collect();
        out.sort_by_key(|&i| all.get(i).is_some_and(|s| !s.applies(self.place.as_ref())));
        out
    }

    /// Where the focused pane is connected (`None`: no host).
    pub fn set_place(&mut self, place: Option<Place>) {
        self.place = place;
    }

    fn visible(rows: usize) -> usize {
        rows.saturating_sub(HEADER + FOOTER).max(1)
    }

    /// Whether a snippet is being edited.
    pub fn editing(&self) -> bool {
        self.draft.is_some()
    }

    /// Shows `report` in place of the list until Escape.
    pub fn show_report(&mut self, report: Report) {
        self.draft = None;
        self.asking = None;
        self.picker = None;
        self.report = Some(report);
        self.report_scroll = 0;
    }

    /// Shows the page that picks the hosts to run a snippet on.
    pub fn show_picker(&mut self, picker: Picker) {
        self.draft = None;
        self.asking = None;
        self.picker = Some(picker);
    }

    /// Whether the host picker is shown.
    pub fn picking(&self) -> bool {
        self.picker.is_some()
    }

    /// The report shown.
    pub fn report(&self) -> Option<&Report> {
        self.report.as_ref()
    }

    /// The report shown, to update as the runs go.
    pub fn report_mut(&mut self) -> Option<&mut Report> {
        self.report.as_mut()
    }

    /// Whether a report is shown.
    pub fn reporting(&self) -> bool {
        self.report.is_some()
    }

    /// Whether a snippet's arguments are being asked for.
    pub fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// Starts afresh with an empty filter (when the view is opened).
    pub fn reset(&mut self) {
        self.filter.clear();
        self.selected = 0;
        self.scroll = 0;
        self.draft = None;
        self.asking = None;
        self.picker = None;
        // A run on picked hosts stays until its report is closed.
        if self.report.as_ref().is_some_and(|r| r.run.is_none()) {
            self.report = None;
        }
    }

    /// What typing snippet `name` does: [`SnipAction::Type`] at once when
    /// it takes no arguments, else `None` while the view asks for them.
    /// `vars` are the variables' values in the pane it goes to.
    pub fn send(
        &mut self,
        name: &str,
        lines: &[String],
        enter: bool,
        vars: Vars,
    ) -> Option<SnipAction> {
        let args = login::args(lines);
        if args.is_empty() {
            return Some(SnipAction::Type {
                name: name.to_owned(),
                steps: login::expand(lines, enter, |_| String::new(), |n| var(&vars, n)),
            });
        }
        let mut a = Asking {
            name: name.to_owned(),
            lines: lines.to_vec(),
            enter,
            vars,
            values: args.iter().map(|a| a.default.clone()).collect(),
            args,
            focus: 0,
            cursor: 0,
            on: None,
        };
        a.set_focus(0);
        self.draft = None;
        self.asking = Some(a);
        None
    }

    /// A new snippet made of `text`, such as the selection in a pane.
    pub fn new_from(&mut self, text: &str) {
        self.asking = None;
        let mut d = Draft::new(
            None,
            &Snippet {
                lines: paste_lines(text).into_iter().take(MAX_LINES).collect(),
                ..Snippet::default()
            },
        );
        d.set_focus(0);
        self.draft = Some(d);
    }

    /// A new snippet made of a recording of keystrokes.
    pub fn new_recorded(&mut self, rec: Recording) {
        self.asking = None;
        let mut d = Draft::new(
            None,
            &Snippet {
                lines: rec.lines,
                ..Snippet::default()
            },
        );
        let mut note = String::from("Recorded. Check the lines, then name it.");
        if rec.hidden > 0 {
            note.push_str(" Lines the host did not echo became @password.");
        }
        if rec.skipped {
            note.push_str(" Arrow and function keys were left out.");
        }
        d.note = Some(note);
        d.set_focus(0);
        self.draft = Some(d);
    }

    /// Pastes `text` at the cursor: into an argument, the snippet being
    /// edited (one line after another), or the filter.
    pub fn paste(&mut self, text: &str) {
        let lines = paste_lines(text);
        if let Some(a) = &mut self.asking {
            let at = a.focus;
            if let Some(v) = a.values.get_mut(at) {
                for c in lines.join(" ").chars() {
                    if insert(v, a.cursor, c) {
                        a.cursor += 1;
                    }
                }
            }
            return;
        }
        let Some(d) = &mut self.draft else {
            for c in lines.join(" ").chars() {
                if self.filter.chars().count() < 60 {
                    self.filter.push(c);
                }
            }
            self.selected = 0;
            return;
        };
        if !d.is_line(d.focus) {
            let mut at = d.cursor;
            if let Some(t) = d.text_mut() {
                for c in lines.join(" ").chars() {
                    if insert(t, at, c) {
                        at += 1;
                    }
                }
                d.cursor = at;
            }
            return;
        }
        // The first pasted line goes in at the cursor, the rest become
        // lines of their own, with what followed the cursor after them.
        let i = d.focus - HEAD;
        let Some(current) = d.lines.get(i).cloned() else {
            return;
        };
        let split = current
            .char_indices()
            .nth(d.cursor)
            .map_or(current.len(), |(b, _)| b);
        let (head, tail) = current.split_at(split);
        let mut new: Vec<String> = lines;
        if new.is_empty() {
            return;
        }
        if let Some(first) = new.first_mut() {
            first.insert_str(0, head);
        }
        let last = new.len() - 1;
        let cursor = new.get(last).map_or(0, |l| l.chars().count());
        if let Some(l) = new.get_mut(last) {
            l.push_str(tail);
        }
        let room = MAX_LINES.saturating_sub(d.lines.len() - 1);
        new.truncate(room.max(1));
        let added = new.len();
        d.lines.splice(i..=i, new);
        d.error = None;
        d.tidy();
        d.set_focus(HEAD + i + added - 1);
        if added == last + 1 {
            d.cursor = cursor;
        }
    }

    /// The edit is over: saved, or the snippet is gone.
    pub fn saved(&mut self, all: &[Snippet], select: Option<usize>) {
        self.draft = None;
        if let Some(i) = select {
            self.filter.clear();
            self.selected = i;
        }
        self.selected = self.selected.min(all.len().saturating_sub(1));
    }

    fn clamp(&mut self, n: usize, visible: usize) {
        self.selected = self.selected.min(n.saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
    }

    pub fn key(&mut self, key: SideKey, all: &[Snippet], rows: usize) -> Option<SnipAction> {
        if self.report.is_some() {
            return self.report_key(key, rows);
        }
        if self.picker.is_some() {
            return self.pick_key(key, rows);
        }
        if self.asking.is_some() {
            return self.ask_key(key);
        }
        if self.draft.is_some() {
            return self.draft_key(key);
        }
        let shown = self.shown(all);
        let visible = Self::visible(rows);
        let here = shown.get(self.selected).copied();
        match key {
            SideKey::Up => self.selected = self.selected.saturating_sub(1),
            SideKey::Down => self.selected += 1,
            SideKey::PageUp => self.selected = self.selected.saturating_sub(visible),
            SideKey::PageDown => self.selected += visible,
            SideKey::Home => self.selected = 0,
            SideKey::End => self.selected = shown.len().saturating_sub(1),
            SideKey::Enter { split } => {
                let s = all.get(here?)?;
                return Some(SnipAction::Send {
                    name: s.name.clone(),
                    lines: s.lines.clone(),
                    enter: !split,
                });
            }
            SideKey::Escape if self.filter.is_empty() => return Some(SnipAction::Leave),
            SideKey::Escape => self.filter.clear(),
            SideKey::Backspace => {
                self.filter.pop();
            }
            SideKey::Char(c) if !c.is_control() && self.filter.chars().count() < 60 => {
                self.filter.push(c);
                self.selected = 0;
            }
            SideKey::New => {
                // What was typed as a filter names the new snippet.
                let name = std::mem::take(&mut self.filter);
                self.draft = Some(Draft::new(
                    None,
                    &Snippet {
                        name,
                        ..Snippet::default()
                    },
                ));
                if let Some(d) = &mut self.draft {
                    d.set_focus(if d.name().is_empty() { 0 } else { HEAD });
                }
            }
            SideKey::Edit => {
                let i = here?;
                self.draft = Some(Draft::new(Some(i), all.get(i)?));
            }
            SideKey::Delete => return Some(SnipAction::Delete(here?)),
            SideKey::Refresh => {
                let s = all.get(here?)?;
                return Some(SnipAction::PickHosts {
                    name: s.name.clone(),
                    lines: s.lines.clone(),
                });
            }
            _ => {}
        }
        self.clamp(self.shown(all).len(), visible);
        None
    }

    fn report_key(&mut self, key: SideKey, rows: usize) -> Option<SnipAction> {
        let r = self.report.as_mut()?;
        let n = r.rows.len();
        let visible = rows.saturating_sub(HEADER).max(1);
        match key {
            SideKey::Escape if r.run.is_some() && !r.over() => return Some(SnipAction::Cancel),
            SideKey::Escape => return self.report.take().map(SnipAction::CloseReport),
            SideKey::Up => r.selected = r.selected.saturating_sub(1),
            SideKey::Down => r.selected += 1,
            SideKey::PageUp => r.selected = r.selected.saturating_sub(visible),
            SideKey::PageDown => r.selected += visible,
            SideKey::Home => r.selected = 0,
            SideKey::End => r.selected = n.saturating_sub(1),
            SideKey::Enter { .. } => {
                return r.rows.get(r.selected)?.pane.map(SnipAction::ShowRow);
            }
            SideKey::Char('c' | 'C') => return Some(SnipAction::Copy(r.table())),
            SideKey::Char('s' | 'S') => return Some(SnipAction::SaveCsv(r.csv())),
            SideKey::Char('r' | 'R')
                if r.run.is_some()
                    && r.rows
                        .iter()
                        .any(|r| matches!(r.state, RowState::Failed(_) | RowState::Stopped)) =>
            {
                return Some(SnipAction::Retry);
            }
            _ => {}
        }
        r.selected = r.selected.min(n.saturating_sub(1));
        if r.selected < self.report_scroll {
            self.report_scroll = r.selected;
        }
        if r.selected >= self.report_scroll + visible {
            self.report_scroll = r.selected + 1 - visible;
        }
        None
    }

    fn pick_key(&mut self, key: SideKey, rows: usize) -> Option<SnipAction> {
        let p = self.picker.as_mut()?;
        let n = p.lines().len();
        let visible = rows.saturating_sub(PICK_HEAD).max(1);
        let next = |f: PickField| match f {
            PickField::List => PickField::Parallel,
            PickField::Parallel => PickField::Password,
            PickField::Password => PickField::List,
        };
        match (p.field, key) {
            (_, SideKey::Escape) => {
                self.picker = None;
                return None;
            }
            (f, SideKey::Tab) => p.field = next(f),
            (_, SideKey::Enter { .. }) => {
                let (hosts, parallel) = p.start()?;
                let password = (!p.password.0.is_empty()).then(|| p.password.clone());
                let p = self.picker.take()?;
                let args = login::args(&p.lines);
                if args.is_empty() {
                    return Some(SnipAction::RunOn {
                        name: p.name,
                        hosts,
                        run: Run {
                            lines: p.lines,
                            args: Vec::new(),
                            parallel,
                            password,
                        },
                    });
                }
                // The variables differ per host: shown by name.
                let vars = login::VARIABLES
                    .iter()
                    .map(|v| (*v, format!("${{{v}}}")))
                    .collect();
                let mut a = Asking {
                    name: p.name,
                    lines: p.lines,
                    enter: true,
                    vars,
                    values: args.iter().map(|a| a.default.clone()).collect(),
                    args,
                    focus: 0,
                    cursor: 0,
                    on: Some((hosts, parallel, password)),
                };
                a.set_focus(0);
                self.asking = Some(a);
                return None;
            }
            (PickField::List, SideKey::Up) => p.selected = p.selected.saturating_sub(1),
            (PickField::List, SideKey::Down) => p.selected += 1,
            (PickField::List, SideKey::PageUp) => {
                p.selected = p.selected.saturating_sub(visible);
            }
            (PickField::List, SideKey::PageDown) => p.selected += visible,
            (PickField::List, SideKey::Home) => p.selected = 0,
            (PickField::List, SideKey::End) => p.selected = n.saturating_sub(1),
            (PickField::List, SideKey::Char(' ')) => p.toggle(),
            (PickField::List, SideKey::Char(c))
                if !c.is_control() && p.filter.chars().count() < 60 =>
            {
                p.filter.push(c);
                p.selected = 0;
            }
            (PickField::List, SideKey::Backspace) => {
                p.filter.pop();
                p.selected = 0;
            }
            (_, SideKey::Up) => {
                p.field = next(next(p.field));
            }
            (_, SideKey::Down) => p.field = next(p.field),
            (PickField::Parallel, SideKey::Char(c))
                if c.is_ascii_digit() && p.parallel.len() < 2 =>
            {
                p.parallel.push(c);
                p.error = None;
            }
            (PickField::Parallel, SideKey::Backspace) => {
                p.parallel.pop();
            }
            (PickField::Password, SideKey::Char(c))
                if !c.is_control() && p.password.0.chars().count() < MAX_TEXT =>
            {
                p.password.0.push(c);
            }
            (PickField::Password, SideKey::Backspace) => {
                p.password.0.pop();
            }
            _ => {}
        }
        let n = p.lines().len();
        p.selected = p.selected.min(n.saturating_sub(1));
        if p.selected < p.scroll {
            p.scroll = p.selected;
        }
        if p.selected >= p.scroll + visible {
            p.scroll = p.selected + 1 - visible;
        }
        None
    }

    fn ask_key(&mut self, key: SideKey) -> Option<SnipAction> {
        let a = self.asking.as_mut()?;
        let n = a.values.len();
        match key {
            SideKey::Escape => {
                self.asking = None;
                return Some(SnipAction::Leave);
            }
            SideKey::Up => a.set_focus(a.focus.saturating_sub(1)),
            SideKey::Down | SideKey::Tab => a.set_focus((a.focus + 1) % n),
            // Enter goes to the next argument; on the last, or with Shift
            // on any, the snippet is typed.
            SideKey::Enter { split } if split || a.focus + 1 >= n => {
                let done = a.done();
                self.asking = None;
                return Some(done);
            }
            SideKey::Enter { .. } => a.set_focus(a.focus + 1),
            key => {
                let at = a.focus;
                if let Some(v) = a.values.get_mut(at) {
                    edit_key(key, v, &mut a.cursor);
                }
            }
        }
        None
    }

    fn draft_key(&mut self, key: SideKey) -> Option<SnipAction> {
        let d = self.draft.as_mut()?;
        let n = d.count();
        let save = n - 2;
        let on_button = d.focus >= save;
        match key {
            SideKey::Escape => {
                self.draft = None;
                return None;
            }
            SideKey::Up => d.set_focus(d.focus.saturating_sub(1)),
            SideKey::Down | SideKey::Tab => d.set_focus((d.focus + 1) % n),
            SideKey::Home if on_button => d.set_focus(0),
            SideKey::End if on_button => d.set_focus(n - 1),
            SideKey::Home => d.cursor = 0,
            SideKey::End => d.cursor = d.text(d.focus).map_or(0, |t| t.chars().count()),
            SideKey::Left if on_button => d.set_focus(save),
            SideKey::Right if on_button => d.set_focus(save + 1),
            SideKey::Left => d.cursor = d.cursor.saturating_sub(1),
            SideKey::Right => {
                let len = d.text(d.focus).map_or(0, |t| t.chars().count());
                d.cursor = (d.cursor + 1).min(len);
            }
            SideKey::Enter { .. } if d.focus == save + 1 => {
                self.draft = None;
                return None;
            }
            SideKey::Enter { .. } if d.focus == save => return d.save(),
            // Enter on a line goes to the next, where a new one waits.
            SideKey::Enter { .. } => d.set_focus(d.focus + 1),
            SideKey::Char(' ') if on_button => {
                return self.draft_key(SideKey::Enter { split: false });
            }
            SideKey::Char(c) if !c.is_control() && !on_button => {
                let at = d.cursor;
                if let Some(t) = d.text_mut()
                    && t.chars().count() < MAX_TEXT
                {
                    let byte = t.char_indices().nth(at).map_or(t.len(), |(b, _)| b);
                    t.insert(byte, c);
                    d.cursor += 1;
                }
                d.error = None;
                d.tidy();
            }
            SideKey::Backspace | SideKey::Delete if !on_button => {
                let at = d.cursor;
                let back = key == SideKey::Backspace;
                if let Some(t) = d.text_mut() {
                    let gone = if back { at.checked_sub(1) } else { Some(at) };
                    if let Some(i) = gone
                        && let Some((b, _)) = t.char_indices().nth(i)
                    {
                        t.remove(b);
                        if back {
                            d.cursor -= 1;
                        }
                    }
                }
            }
            _ => {}
        }
        None
    }

    /// A click on grid row `y` selects the snippet or field there.
    pub fn click(&mut self, y: usize, all: &[Snippet], rows: usize) {
        if let Some(r) = &mut self.report {
            if let Some(i) = y.checked_sub(HEADER) {
                r.selected = (self.report_scroll + i).min(r.rows.len().saturating_sub(1));
            }
            return;
        }
        if let Some(p) = &mut self.picker {
            match y {
                2 => p.field = PickField::List,
                3 => p.field = PickField::Parallel,
                4 => p.field = PickField::Password,
                y if y >= PICK_HEAD => {
                    p.field = PickField::List;
                    p.selected = (p.scroll + y - PICK_HEAD).min(p.lines().len().saturating_sub(1));
                }
                _ => {}
            }
            return;
        }
        if let Some(a) = &mut self.asking {
            if let Some(i) = y.checked_sub(HEADER + 1) {
                a.set_focus(i);
            }
            return;
        }
        if let Some(d) = &mut self.draft {
            if let Some(i) = y.checked_sub(HEADER)
                && i < d.count()
            {
                d.set_focus(i);
            }
            return;
        }
        if let Some(i) = y.checked_sub(HEADER) {
            self.selected = self.scroll + i;
        }
        self.clamp(self.shown(all).len(), Self::visible(rows));
    }

    /// Wheel scrolling by `lines` (positive: up).
    pub fn scroll_by(&mut self, by: i32, all: &[Snippet], rows: usize) {
        if self.report.is_some() || self.picker.is_some() {
            let key = if by > 0 { SideKey::Up } else { SideKey::Down };
            for _ in 0..by.unsigned_abs() {
                if self.report.is_some() {
                    self.report_key(key, rows);
                } else if self
                    .picker
                    .as_ref()
                    .is_some_and(|p| p.field == PickField::List)
                {
                    self.pick_key(key, rows);
                }
            }
            return;
        }
        if self.draft.is_some() {
            return;
        }
        let max = self.shown(all).len().saturating_sub(Self::visible(rows));
        self.scroll = if by > 0 {
            self.scroll.saturating_sub(by as usize)
        } else {
            (self.scroll + by.unsigned_abs() as usize).min(max)
        };
    }

    pub fn draw(&mut self, grid: &mut Grid, palette: &Palette, focused: bool, all: &[Snippet]) {
        let width = grid.cols();
        let rows = grid.rows();
        let bg = palette.strip;
        let red = Rgb::new(0xd0, 0x6a, 0x6a);
        let row = |grid: &mut Grid, y: usize, line: &mut Line, bg: Rgb| {
            grid.set_row(y, line.cells(bg));
        };
        if let Some(r) = &self.report {
            let done = r.rows.iter().filter(|r| r.state.over()).count();
            let mut line = Line::new(width, bg);
            line.put(
                &fit(
                    &format!(" {} · {done} of {} over", r.name, r.rows.len()),
                    width,
                ),
                palette.text,
                Flags::BOLD,
            );
            row(grid, 0, &mut line, bg);
            let mut line = Line::new(width, bg);
            let help = match (&r.run, r.over()) {
                (Some(_), false) => " Enter shows · C copies · S saves CSV · Esc stops the run",
                (Some(_), true) => " Enter shows · C copies · S saves CSV · R retries · Esc closes",
                (None, _) => " Enter shows · C copies as a table · S saves CSV · Esc closes",
            };
            line.put(&fit(help, width), palette.dim, Flags::ITALIC);
            row(grid, 1, &mut line, bg);
            let label_w = r
                .rows
                .iter()
                .map(|r| r.label.chars().count())
                .max()
                .unwrap_or(0)
                .min(width / 3);
            let mut y = HEADER;
            for (i, item) in r.rows.iter().enumerate().skip(self.report_scroll) {
                if y >= rows {
                    break;
                }
                let row_bg = if focused && i == r.selected {
                    palette.active
                } else {
                    bg
                };
                let mut line = Line::new(width, row_bg);
                let (mark, colour) = match &item.state {
                    RowState::Queued => ("·", palette.dim),
                    RowState::Connecting | RowState::Running(_) => ("…", palette.dim),
                    RowState::Done => ("✓", palette.accent),
                    RowState::Failed(_) => ("✗", red),
                    RowState::Stopped => ("–", palette.dim),
                };
                line.put(&format!(" {mark} "), colour, Flags::BOLD);
                let label: String = fit(&item.label, label_w);
                line.put(
                    &format!("{label:<label_w$}  "),
                    palette.text,
                    Flags::empty(),
                );
                let left = width.saturating_sub(line.col);
                let (text, colour) = match &item.state {
                    RowState::Failed(why) if item.values.is_empty() => (why.clone(), red),
                    RowState::Running(Some(w)) if item.values.is_empty() => {
                        (format!("waiting for {w}"), palette.dim)
                    }
                    RowState::Queued => ("queued".to_owned(), palette.dim),
                    RowState::Connecting => ("logging in".to_owned(), palette.dim),
                    _ => (item.values.join(" · "), palette.text),
                };
                line.put(&fit(&text, left), colour, Flags::empty());
                row(grid, y, &mut line, row_bg);
                y += 1;
            }
            for y in y..rows {
                row(grid, y, &mut Line::new(width, bg), bg);
            }
            return;
        }
        if let Some(p) = &self.picker {
            let mut line = Line::new(width, bg);
            line.put(
                &fit(&format!(" Run {} on: {} picked", p.name, p.picked()), width),
                palette.text,
                Flags::BOLD,
            );
            row(grid, 0, &mut line, bg);
            let mut line = Line::new(width, bg);
            match &p.error {
                Some(e) => line.put(&fit(&format!(" {e}"), width), red, Flags::empty()),
                None => line.put(
                    &fit(
                        " Space picks · Tab: next field · Enter runs · Esc: back",
                        width,
                    ),
                    palette.dim,
                    Flags::ITALIC,
                ),
            };
            row(grid, 1, &mut line, bg);
            let password = "•".repeat(p.password.0.chars().count());
            let fields = [
                (
                    PickField::List,
                    "Filter  ",
                    p.filter.as_str(),
                    "type to filter",
                ),
                (
                    PickField::Parallel,
                    "At once ",
                    p.parallel.as_str(),
                    "1 to 16",
                ),
                (
                    PickField::Password,
                    "Password",
                    password.as_str(),
                    "for hosts with none saved; never stored",
                ),
            ];
            for (y, (field, label, value, hint)) in fields.into_iter().enumerate() {
                let here = focused && p.field == field && field != PickField::List;
                let row_bg = if here { palette.active } else { bg };
                let mut line = Line::new(width, row_bg);
                line.put(&format!(" {label} "), palette.dim, Flags::empty());
                let room = width.saturating_sub(line.col);
                if value.is_empty() {
                    line.put(&fit(hint, room), palette.dim, Flags::ITALIC);
                } else {
                    line.put(&fit(value, room), palette.text, Flags::empty());
                }
                if here || (field == PickField::List && focused && p.field == field) {
                    line.put("▏", palette.accent, Flags::empty());
                }
                row(grid, y + HEADER, &mut line, row_bg);
            }
            row(grid, PICK_HEAD - 1, &mut Line::new(width, bg), bg);
            let lines = p.lines();
            let mut y = PICK_HEAD;
            for (i, l) in lines.iter().enumerate().skip(p.scroll) {
                if y >= rows {
                    break;
                }
                let row_bg = if focused && p.field == PickField::List && i == p.selected {
                    palette.active
                } else {
                    bg
                };
                let mut line = Line::new(width, row_bg);
                match *l {
                    PickLine::Folder(f) => {
                        let folder = p.folders.get(f).cloned().flatten().unwrap_or_default();
                        let (all, picked) = p
                            .hosts
                            .iter()
                            .filter(|h| h.place.folder.as_deref() == Some(folder.as_str()))
                            .fold((0, 0), |(a, k), h| (a + 1, k + usize::from(h.picked)));
                        line.put(
                            &fit(&format!(" {folder}  {picked}/{all}"), width),
                            palette.text,
                            Flags::BOLD,
                        );
                    }
                    PickLine::Host(h) => {
                        let Some(h) = p.hosts.get(h) else { continue };
                        let mark = if h.picked { "[x]" } else { "[ ]" };
                        line.put(
                            &format!("  {mark} "),
                            if h.picked {
                                palette.accent
                            } else {
                                palette.dim
                            },
                            Flags::BOLD,
                        );
                        let room = width.saturating_sub(line.col);
                        line.put(&fit(&h.place.session, room), palette.text, Flags::empty());
                        let room = width.saturating_sub(line.col + 2);
                        if room > 4 {
                            line.put(
                                &fit(&format!("  {}", h.detail), room + 2),
                                palette.dim,
                                Flags::empty(),
                            );
                        }
                    }
                }
                row(grid, y, &mut line, row_bg);
                y += 1;
            }
            if lines.is_empty() && y < rows {
                let mut line = Line::new(width, bg);
                line.put(
                    &fit(" No saved session matches.", width),
                    palette.dim,
                    Flags::ITALIC,
                );
                row(grid, y, &mut line, bg);
                y += 1;
            }
            for y in y..rows {
                row(grid, y, &mut Line::new(width, bg), bg);
            }
            return;
        }
        if let Some(a) = &self.asking {
            let mut line = Line::new(width, bg);
            line.put(
                &fit(&format!(" Run {}", a.name), width),
                palette.text,
                Flags::BOLD,
            );
            row(grid, 0, &mut line, bg);
            let mut line = Line::new(width, bg);
            let help = if a.enter {
                " Enter: next, types on the last · Esc: cancel"
            } else {
                " Enter: next, inserts on the last · Esc: cancel"
            };
            line.put(&fit(help, width), palette.dim, Flags::ITALIC);
            row(grid, 1, &mut line, bg);
            let mut y = HEADER;
            for (i, (arg, value)) in a.args.iter().zip(&a.values).enumerate() {
                if y >= rows {
                    break;
                }
                let here = focused && i == a.focus;
                let row_bg = if here { palette.active } else { bg };
                let mut line = Line::new(width, row_bg);
                line.put(
                    &format!(" ${:<3} ", arg.number),
                    palette.dim,
                    Flags::empty(),
                );
                let room = width.saturating_sub(line.col + 1);
                if here {
                    let chars: Vec<char> = value.chars().collect();
                    let at = a.cursor.min(chars.len());
                    let start = at.saturating_sub(room.saturating_sub(2));
                    let before: String = chars.get(start..at).unwrap_or_default().iter().collect();
                    let after: String = chars.get(at..).unwrap_or_default().iter().collect();
                    line.put(&before, palette.text, Flags::empty());
                    line.put("▏", palette.accent, Flags::empty());
                    let left = width.saturating_sub(line.col);
                    line.put(&fit(&after, left), palette.text, Flags::empty());
                } else if value.is_empty() {
                    line.put(&fit("empty", room), palette.dim, Flags::ITALIC);
                } else {
                    line.put(&fit(value, room), palette.text, Flags::empty());
                }
                row(grid, y, &mut line, row_bg);
                y += 1;
            }
            // What will be typed.
            if y < rows {
                row(grid, y, &mut Line::new(width, bg), bg);
                y += 1;
            }
            for step in a.filled() {
                if y >= rows {
                    break;
                }
                let mut line = Line::new(width, bg);
                line.put(
                    &fit(&format!(" › {}", preview(&step)), width),
                    palette.dim,
                    Flags::empty(),
                );
                row(grid, y, &mut line, bg);
                y += 1;
            }
            for y in y..rows {
                row(grid, y, &mut Line::new(width, bg), bg);
            }
            return;
        }
        let title = if self.draft.as_ref().is_some_and(|d| d.index.is_some()) {
            " Edit snippet"
        } else if self.draft.is_some() {
            " New snippet"
        } else {
            " Snippets"
        };
        let mut line = Line::new(width, bg);
        line.put(title, palette.text, Flags::BOLD);
        row(grid, 0, &mut line, bg);
        if let Some(d) = &self.draft {
            let mut y = 1;
            let mut line = Line::new(width, bg);
            match (&d.error, &d.note) {
                (Some(e), _) => line.put(&fit(&format!(" {e}"), width), red, Flags::empty()),
                (None, Some(n)) => line.put(
                    &fit(&format!(" {n}"), width),
                    palette.accent,
                    Flags::empty(),
                ),
                (None, None) => line.put(
                    &fit(
                        " Enter: next line · @expect, @pause, @key lines · Esc: cancel",
                        width,
                    ),
                    palette.dim,
                    Flags::ITALIC,
                ),
            };
            row(grid, y, &mut line, bg);
            y += 1;
            let save = d.count() - 2;
            for i in 0..save {
                if y >= rows {
                    break;
                }
                let here = focused && d.focus == i;
                let row_bg = if here { palette.active } else { bg };
                let mut line = Line::new(width, row_bg);
                let label = match HEAD_LABELS.get(i) {
                    Some(l) => (*l).to_owned(),
                    None => format!("{}", i - HEAD + 1),
                };
                line.put(&format!(" {label:>7} "), palette.dim, Flags::empty());
                let value = d.text(i).cloned().unwrap_or_default();
                let room = width.saturating_sub(line.col + 1);
                if here {
                    let chars: Vec<char> = value.chars().collect();
                    let at = d.cursor.min(chars.len());
                    let start = at.saturating_sub(room.saturating_sub(2));
                    let before: String = chars.get(start..at).unwrap_or_default().iter().collect();
                    let after: String = chars.get(at..).unwrap_or_default().iter().collect();
                    line.put(&before, palette.text, Flags::empty());
                    line.put("▏", palette.accent, Flags::empty());
                    let left = width.saturating_sub(line.col);
                    line.put(&fit(&after, left), palette.text, Flags::empty());
                } else if value.is_empty() {
                    let hint = HEAD_HINTS.get(i).copied().unwrap_or("command line");
                    line.put(&fit(hint, room), palette.dim, Flags::ITALIC);
                } else {
                    line.put(&fit(&value, room), palette.text, Flags::empty());
                }
                row(grid, y, &mut line, row_bg);
                y += 1;
            }
            if y < rows {
                let here = focused && d.focus >= save;
                let row_bg = if here { palette.active } else { bg };
                let mut line = Line::new(width, row_bg);
                line.put(" ", palette.text, Flags::empty());
                for (b, label) in ["Save", "Cancel"].into_iter().enumerate() {
                    let on = focused && d.focus == save + b;
                    let flags = if on {
                        Flags::BOLD.union(Flags::INVERSE)
                    } else {
                        Flags::BOLD
                    };
                    line.put(&format!(" {label} "), palette.text, flags);
                    line.put(" ", palette.text, Flags::empty());
                }
                row(grid, y, &mut line, row_bg);
                y += 1;
            }
            for y in y..rows {
                row(grid, y, &mut Line::new(width, bg), bg);
            }
            return;
        }
        let shown = self.shown(all);
        let visible = Self::visible(rows);
        self.clamp(shown.len(), visible);
        let mut line = Line::new(width, bg);
        if self.filter.is_empty() {
            let hint = if focused {
                " Type to filter▏"
            } else {
                " Ctrl+Shift+Space to pick"
            };
            line.put(hint, palette.dim, Flags::ITALIC);
        } else {
            line.put(" Filter ", palette.dim, Flags::empty());
            line.put(&self.filter, palette.text, Flags::empty());
            if focused {
                line.put("▏", palette.accent, Flags::empty());
            }
        }
        row(grid, 1, &mut line, bg);
        for y in 0..visible {
            let at = HEADER + y;
            if at >= rows.saturating_sub(FOOTER) {
                break;
            }
            let i = self.scroll + y;
            let here = focused && i == self.selected;
            let row_bg = if here { palette.active } else { bg };
            let mut line = Line::new(width, row_bg);
            match shown.get(i).and_then(|&n| all.get(n)) {
                Some(s) => {
                    // Snippets for other hosts are dimmed.
                    let here_host = s.applies(self.place.as_ref());
                    line.put(
                        &fit(&format!(" {}", s.name), width),
                        if here_host { palette.text } else { palette.dim },
                        Flags::empty(),
                    );
                    let mut tags = String::new();
                    if let Some(n) = s.hotkey {
                        tags.push_str(&format!(" F{n}"));
                    }
                    if !s.when.is_empty() {
                        tags.push_str(&format!(" «{}»", s.when));
                    }
                    let left = width.saturating_sub(line.col);
                    line.put(&fit(&tags, left), palette.accent, Flags::empty());
                    let first = s.lines.first().map(String::as_str).unwrap_or_default();
                    let more = if s.lines.len() > 1 {
                        format!(" (+{})", s.lines.len() - 1)
                    } else {
                        String::new()
                    };
                    let left = width.saturating_sub(line.col);
                    line.put(
                        &fit(&format!("  {first}{more}"), left),
                        palette.dim,
                        Flags::empty(),
                    );
                }
                None if i == 0 && all.is_empty() => {
                    line.put(
                        " No snippets yet. Insert adds one.",
                        palette.dim,
                        Flags::ITALIC,
                    );
                }
                None if i == 0 => {
                    line.put(" Nothing matches.", palette.dim, Flags::ITALIC);
                }
                None => {}
            }
            row(grid, at, &mut line, row_bg);
        }
        if rows > HEADER {
            let mut line = Line::new(width, bg);
            line.put(
                &fit(
                    " Enter types · Shift+Enter: no Enter · F5 on hosts · Ins new · F2 edit · Del",
                    width,
                ),
                palette.dim,
                Flags::empty(),
            );
            row(grid, rows - 1, &mut line, bg);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::batch::{PickHost, ReportRow};
    use ferossh_render::Theme;

    fn snippets() -> Vec<Snippet> {
        vec![
            Snippet {
                name: "Show interfaces".into(),
                lines: vec!["show ip interface brief".into()],
                ..Snippet::default()
            },
            Snippet {
                name: "Save config".into(),
                lines: vec!["copy running-config startup-config".into(), String::new()],
                ..Snippet::default()
            },
        ]
    }

    fn typed(v: &mut SnippetsView, all: &[Snippet], text: &str) {
        for c in text.chars() {
            v.key(SideKey::Char(c), all, 20);
        }
    }

    fn texts(v: &mut SnippetsView, all: &[Snippet]) -> Vec<String> {
        let mut grid = Grid::new(60, 12);
        v.draw(&mut grid, &Palette::of(&Theme::default()), true, all);
        (0..12)
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
    fn filter_and_send() {
        let all = snippets();
        let mut v = SnippetsView::default();
        let t = texts(&mut v, &all);
        assert!(t[2].contains("Show interfaces") && t[2].contains("show ip"));
        assert!(
            t[3].contains("Save config") && t[3].ends_with("(+1)"),
            "{t:#?}"
        );
        typed(&mut v, &all, "save");
        let t = texts(&mut v, &all);
        assert!(t[1].contains("Filter save"));
        assert!(t[2].contains("Save config"));
        assert!(!t.iter().any(|l| l.contains("Show interfaces")));
        assert_eq!(
            v.key(SideKey::Enter { split: true }, &all, 20),
            Some(SnipAction::Send {
                name: "Save config".into(),
                lines: all[1].lines.clone(),
                enter: false
            })
        );
        v.key(SideKey::Escape, &all, 20);
        v.key(SideKey::Down, &all, 20);
        v.key(SideKey::Down, &all, 20);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::Send {
                name: "Save config".into(),
                lines: all[1].lines.clone(),
                enter: true
            }),
            "the selection stops at the last"
        );
        assert_eq!(
            v.key(SideKey::Delete, &all, 20),
            Some(SnipAction::Delete(1))
        );
        assert_eq!(v.key(SideKey::Escape, &all, 20), Some(SnipAction::Leave));
        typed(&mut v, &all, "vlan");
        assert_eq!(v.key(SideKey::Enter { split: false }, &all, 20), None);
        assert!(texts(&mut v, &all)[2].contains("Nothing matches"));
    }

    #[test]
    fn new_snippets_take_the_filter_as_name() {
        let all = snippets();
        let mut v = SnippetsView::default();
        typed(&mut v, &all, "Show vlans");
        v.key(SideKey::New, &all, 20);
        assert!(v.editing());
        typed(&mut v, &all, "terminal length 0");
        v.key(SideKey::Enter { split: false }, &all, 20);
        typed(&mut v, &all, "show vlan brief");
        let t = texts(&mut v, &all);
        assert!(t[0].contains("New snippet"));
        assert!(t[2].contains("Name Show vlans"), "{t:#?}");
        assert!(t[9].contains("command line"), "a new line waits: {t:#?}");
        // Down past the empty line to Save.
        v.key(SideKey::Down, &all, 20);
        v.key(SideKey::Down, &all, 20);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::Save {
                index: None,
                snippet: Snippet {
                    name: "Show vlans".into(),
                    lines: vec!["terminal length 0".into(), "show vlan brief".into()],
                    ..Snippet::default()
                }
            })
        );
    }

    #[test]
    fn editing_checks_and_cancels() {
        let all = snippets();
        let mut v = SnippetsView::default();
        v.key(SideKey::Edit, &all, 20);
        let t = texts(&mut v, &all);
        assert!(t[0].contains("Edit snippet"));
        for _ in 0..20 {
            v.key(SideKey::Backspace, &all, 20);
        }
        // Name, hosts, folders, hotkey, when, line 1, the empty line,
        // then Save.
        for _ in 0..7 {
            v.key(SideKey::Tab, &all, 20);
        }
        assert_eq!(v.key(SideKey::Enter { split: false }, &all, 20), None);
        assert!(texts(&mut v, &all)[1].contains("Give the snippet a name."));
        typed(&mut v, &all, "Brief");
        for _ in 0..7 {
            v.key(SideKey::Tab, &all, 20);
        }
        assert_eq!(
            v.key(SideKey::Char(' '), &all, 20),
            Some(SnipAction::Save {
                index: Some(0),
                snippet: Snippet {
                    name: "Brief".into(),
                    lines: all[0].lines.clone(),
                    ..Snippet::default()
                }
            })
        );
        v.key(SideKey::Escape, &all, 20);
        assert!(!v.editing());
    }

    #[test]
    fn arguments_are_asked_for_then_typed() {
        let all = snippets();
        let mut v = SnippetsView::default();
        let lines: Vec<String> = vec![
            "interface $1".into(),
            "description ${2:uplink}".into(),
            "awk '{print $$1}'".into(),
        ];
        let vars = || vec![("host", "sw1".to_owned())];
        assert_eq!(
            v.send("plain", &["echo $$HOME ${host}".into()], true, vars()),
            Some(SnipAction::Type {
                name: "plain".into(),
                steps: vec![Step::Send("echo $HOME sw1".into())],
            })
        );
        assert_eq!(v.send("Port", &lines, false, vars()), None);
        assert!(v.asking());
        typed(&mut v, &all, "Gi0/2");
        let t = texts(&mut v, &all);
        assert!(t[0].contains("Run Port"), "{t:#?}");
        assert!(t[2].contains("$1") && t[2].contains("Gi0/2"), "{t:#?}");
        assert!(t[3].contains("$2") && t[3].contains("uplink"), "{t:#?}");
        assert!(t.iter().any(|l| l.contains("› interface Gi0/2")), "{t:#?}");
        assert_eq!(v.key(SideKey::Enter { split: false }, &all, 20), None);
        v.key(SideKey::Backspace, &all, 20);
        v.key(SideKey::Backspace, &all, 20);
        v.paste("nk to core\n");
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::Type {
                name: "Port".into(),
                steps: vec![
                    Step::Send("interface Gi0/2".into()),
                    Step::Send("description uplink to core".into()),
                    Step::Type("awk '{print $1}'".into()),
                ],
            })
        );
        assert!(!v.asking());
        // Esc gives up; Shift+Enter types at once.
        v.send("Port", &lines, true, vars());
        assert_eq!(v.key(SideKey::Escape, &all, 20), Some(SnipAction::Leave));
        v.send("Port", &lines, true, vars());
        assert!(matches!(
            v.key(SideKey::Enter { split: true }, &all, 20),
            Some(SnipAction::Type { steps, .. }) if matches!(steps.last(), Some(Step::Send(_)))
        ));
    }

    #[test]
    fn selections_and_pastes_become_lines() {
        let all = snippets();
        let mut v = SnippetsView::default();
        v.new_from("\n  show version\t \r\nshow clock\n\n");
        assert!(v.editing());
        typed(&mut v, &all, "Basics");
        for _ in 0..HEAD {
            v.key(SideKey::Down, &all, 20);
        }
        v.key(SideKey::End, &all, 20);
        // Into the middle of line 1: what followed the cursor moves down.
        v.key(SideKey::Left, &all, 20);
        v.paste("x\ny\nz");
        typed(&mut v, &all, "!");
        // Line 3, line 4, the empty line, Save.
        for _ in 0..3 {
            v.key(SideKey::Down, &all, 20);
        }
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::Save {
                index: None,
                snippet: Snippet {
                    name: "Basics".into(),
                    // Leading spaces stay, trailing ones go.
                    lines: vec![
                        "  show versiox".into(),
                        "y".into(),
                        "z!n".into(),
                        "show clock".into()
                    ],
                    ..Snippet::default()
                }
            })
        );
    }

    #[test]
    fn snippets_for_this_host_come_first() {
        let mut all = snippets();
        all[1].hosts = vec!["sw-*".into()];
        all[1].hotkey = Some(3);
        all[0].folders = vec!["Servers".into()];
        let mut v = SnippetsView::default();
        let t = texts(&mut v, &all);
        assert!(t[2].contains("Show interfaces"), "{t:#?}");
        v.set_place(Some(Place {
            session: "sw-core".into(),
            host: "10.0.0.2".into(),
            folder: None,
        }));
        let t = texts(&mut v, &all);
        assert!(
            t[2].contains("Save config") && t[2].contains("F3"),
            "{t:#?}"
        );
        assert!(t[3].contains("Show interfaces"), "{t:#?}");
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::Send {
                name: "Save config".into(),
                lines: all[1].lines.clone(),
                enter: true
            })
        );
    }

    #[test]
    fn the_editor_checks_steps_hotkeys_and_auto_responses() {
        let all = snippets();
        let mut v = SnippetsView::default();
        v.key(SideKey::New, &all, 20);
        typed(&mut v, &all, "Pager");
        v.key(SideKey::Down, &all, 20);
        typed(&mut v, &all, "sw-*, core-?");
        v.key(SideKey::Down, &all, 20);
        typed(&mut v, &all, "Network , Lab");
        v.key(SideKey::Down, &all, 20);
        typed(&mut v, &all, "F13");
        v.key(SideKey::Down, &all, 20);
        typed(&mut v, &all, "--More--");
        v.key(SideKey::Down, &all, 20);
        typed(&mut v, &all, "@kye space");
        let save = |v: &mut SnippetsView| {
            // From the line, past the empty one, to Save.
            for _ in 0..2 {
                v.key(SideKey::Down, &all, 20);
            }
            v.key(SideKey::Enter { split: false }, &all, 20)
        };
        assert_eq!(save(&mut v), None);
        assert!(texts(&mut v, &all)[1].contains("Line 1: Start a step"));
        // The focus went to the bad line: fix it.
        for _ in 0..10 {
            v.key(SideKey::Backspace, &all, 20);
        }
        typed(&mut v, &all, "@key space");
        assert_eq!(save(&mut v), None);
        assert!(texts(&mut v, &all)[1].contains("F1 to F12"));
        for _ in 0..3 {
            v.key(SideKey::Backspace, &all, 20);
        }
        typed(&mut v, &all, "f3");
        for _ in 0..2 {
            v.key(SideKey::Down, &all, 20);
        }
        let got = save(&mut v);
        assert_eq!(
            got,
            Some(SnipAction::Save {
                index: None,
                snippet: Snippet {
                    name: "Pager".into(),
                    lines: vec!["@key space".into()],
                    hosts: vec!["sw-*".into(), "core-?".into()],
                    folders: vec!["Network".into(), "Lab".into()],
                    hotkey: Some(3),
                    when: "--More--".into(),
                }
            })
        );
        // An auto-response may not type secrets.
        let mut v = SnippetsView::default();
        v.key(SideKey::New, &all, 20);
        typed(&mut v, &all, "Fish");
        for _ in 0..4 {
            v.key(SideKey::Down, &all, 20);
        }
        typed(&mut v, &all, "Password:");
        v.key(SideKey::Down, &all, 20);
        typed(&mut v, &all, "@password");
        assert_eq!(save(&mut v), None);
        assert!(texts(&mut v, &all)[1].contains("auto-response"));
    }

    #[test]
    fn recordings_open_with_a_note() {
        let all = snippets();
        let mut v = SnippetsView::default();
        v.new_recorded(Recording {
            lines: vec!["enable".into(), "@password".into()],
            skipped: true,
            hidden: 1,
        });
        let t = texts(&mut v, &all);
        assert!(t[1].contains("Recorded"), "{t:#?}");
        assert!(
            t[7].contains("enable") && t[8].contains("@password"),
            "{t:#?}"
        );
    }

    #[test]
    fn reports_show_each_pane_and_copy_as_a_table() {
        let all = snippets();
        let mut v = SnippetsView::default();
        let sw1 = ReportRow::new("sw1", Some(1), RowState::Running(Some("Version *,".into())));
        let sw2 = ReportRow::new(
            "sw2",
            Some(2),
            RowState::Failed("\"#\" did not show up".into()),
        );
        v.show_report(Report::new("Version", vec![sw1, sw2], None));
        let t = texts(&mut v, &all);
        assert!(t[0].contains("Version · 1 of 2 over"), "{t:#?}");
        assert!(
            t[2].contains("sw1") && t[2].contains("waiting for Version *,"),
            "{t:#?}"
        );
        assert!(
            t[3].contains("✗ sw2") && t[3].contains("did not show up"),
            "{t:#?}"
        );
        let r = v.report_mut().unwrap().row_mut(1).unwrap();
        r.values.push("15.2(4)E10".into());
        r.state = RowState::Done;
        let t = texts(&mut v, &all);
        assert!(
            t[0].contains("2 of 2 over") && t[2].contains("✓ sw1  15.2(4)E10"),
            "{t:#?}"
        );
        assert_eq!(
            v.key(SideKey::Char('c'), &all, 20),
            Some(SnipAction::Copy(
                "Host\tStatus\tValue 1\r\nsw1\tdone\t15.2(4)E10\r\nsw2\tfailed: \"#\" did not show up\r\n"
                    .into()
            ))
        );
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::ShowRow(1))
        );
        v.key(SideKey::Down, &all, 20);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::ShowRow(2))
        );
        assert!(
            matches!(v.key(SideKey::Char('s'), &all, 20), Some(SnipAction::SaveCsv(c)) if c.starts_with("Host,Status"))
        );
        assert_eq!(
            v.key(SideKey::Char('r'), &all, 20),
            None,
            "panes that were open never retry"
        );
        assert!(matches!(
            v.key(SideKey::Escape, &all, 20),
            Some(SnipAction::CloseReport(_))
        ));
        assert!(!v.reporting());
    }

    fn picker() -> Picker {
        let host = |name: &str, folder: Option<&str>| PickHost {
            place: Place {
                session: name.into(),
                host: format!("{name}.lab"),
                folder: folder.map(str::to_owned),
            },
            detail: format!("admin@{name}.lab"),
            picked: false,
        };
        Picker::new(
            "Version",
            &[
                "show version | i ${1:Version}".into(),
                "@capture Version *,".into(),
            ],
            vec![
                host("sw1", Some("Network")),
                host("sw2", Some("Network")),
                host("web", None),
            ],
            4,
        )
    }

    #[test]
    fn hosts_are_picked_then_arguments_asked_then_run() {
        let all = snippets();
        let mut v = SnippetsView::default();
        assert_eq!(
            v.key(SideKey::Refresh, &all, 20),
            Some(SnipAction::PickHosts {
                name: all[0].name.clone(),
                lines: all[0].lines.clone()
            })
        );
        v.show_picker(picker());
        let t = texts(&mut v, &all);
        assert!(t[0].contains("Run Version on: 0 picked"), "{t:#?}");
        assert!(
            t[6].contains("Network  0/2") && t[7].contains("[ ] sw1"),
            "{t:#?}"
        );
        // Enter with nothing picked says so.
        assert_eq!(v.key(SideKey::Enter { split: false }, &all, 20), None);
        assert!(texts(&mut v, &all)[1].contains("Pick at least one"));
        v.key(SideKey::Char(' '), &all, 20);
        let t = texts(&mut v, &all);
        assert!(
            t[6].contains("2/2") && t[8].contains("[x] sw2") && t[9].contains("[ ] web"),
            "{t:#?}"
        );
        v.key(SideKey::Tab, &all, 20);
        v.key(SideKey::Backspace, &all, 20);
        v.key(SideKey::Char('2'), &all, 20);
        v.key(SideKey::Tab, &all, 20);
        for c in "s3cret".chars() {
            v.key(SideKey::Char(c), &all, 20);
        }
        let t = texts(&mut v, &all);
        assert!(
            t[3].contains("At once  2") && t[4].contains("••••••"),
            "{t:#?}"
        );
        assert!(!t.iter().any(|l| l.contains("s3cret")));
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            None,
            "asks for $1"
        );
        assert!(v.asking() && !v.picking());
        let t = texts(&mut v, &all);
        assert!(
            t.iter().any(|l| l.contains("show version | i Version")),
            "{t:#?}"
        );
        let Some(SnipAction::RunOn { name, hosts, run }) =
            v.key(SideKey::Enter { split: false }, &all, 20)
        else {
            panic!("no run")
        };
        assert_eq!(name, "Version");
        assert_eq!(
            hosts.iter().map(|h| h.session.as_str()).collect::<Vec<_>>(),
            ["sw1", "sw2"]
        );
        assert_eq!(run.parallel, 2);
        assert_eq!(run.args, [(1, "Version".to_owned())]);
        assert_eq!(
            run.password.map(|p| p.0.to_string()).as_deref(),
            Some("s3cret")
        );
        assert!(!format!("{:?}", v).contains("s3cret"));
    }

    #[test]
    fn a_run_report_stops_then_retries_and_outlives_reopening() {
        let all = snippets();
        let mut v = SnippetsView::default();
        let rows = vec![
            ReportRow::new("sw1", Some(5), RowState::Running(None)),
            ReportRow::new("sw2", None, RowState::Queued),
        ];
        let run = Run {
            lines: vec!["show clock".into()],
            args: Vec::new(),
            parallel: 1,
            password: None,
        };
        v.show_report(Report::new("Clock", rows, Some(run)));
        let t = texts(&mut v, &all);
        assert!(
            t[1].contains("Esc stops the run") && t[3].contains("queued"),
            "{t:#?}"
        );
        assert_eq!(v.key(SideKey::Escape, &all, 20), Some(SnipAction::Cancel));
        v.reset();
        assert!(v.reporting(), "a run's report stays until closed");
        v.report_mut().unwrap().cancel();
        assert_eq!(v.key(SideKey::Char('r'), &all, 20), Some(SnipAction::Retry));
        v.key(SideKey::Down, &all, 20);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            None,
            "no session yet"
        );
        assert!(
            matches!(v.key(SideKey::Escape, &all, 20), Some(SnipAction::CloseReport(r)) if r.name == "Clock")
        );
    }
}
