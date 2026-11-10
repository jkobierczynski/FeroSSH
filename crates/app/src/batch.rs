//! Snippet runs on several hosts, and the live report of them.
//!
//! A [`Report`] has a row per host. For a snippet typed with broadcast
//! input on, the rows are the panes it went to. For a run on hosts picked
//! in the snippets view (F5), the report is also the run's queue: at most
//! [`Run::parallel`] hosts connect at once, each in a session of its own
//! that no tab shows (Enter on its row shows it in a tab), reusing a
//! connection another pane already has to the same host. A host that asks
//! for anything at its login (an unknown host key, a password without one
//! given, a one-time code) fails with what it asked, and stays open so it
//! can be shown and answered by hand. Each host gets [`HOST_TIMEOUT`] from
//! the moment it starts connecting.
//!
//! [`Picker`] is the page that picks the hosts.

use std::time::{Duration, Instant};

use ferossh_config::login::Place;
use ferossh_config::trigger::OutputText;
use zeroize::Zeroizing;

use crate::layout::PaneId;

/// Hosts connecting at once, unless the user picks another number.
pub const DEFAULT_PARALLEL: usize = 4;
pub const MAX_PARALLEL: usize = 16;
/// The longest one host may take, login included.
pub const HOST_TIMEOUT: Duration = Duration::from_secs(120);

/// Where one host's run is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowState {
    /// Waiting for a free slot.
    Queued,
    /// Logging in.
    Connecting,
    /// Running; waiting for this, if anything.
    Running(Option<String>),
    Done,
    Failed(String),
    /// The user typed into the pane, or cancelled the run.
    Stopped,
}

impl RowState {
    /// Whether nothing more happens to the row.
    pub fn over(&self) -> bool {
        matches!(
            self,
            RowState::Done | RowState::Failed(_) | RowState::Stopped
        )
    }

    fn label(&self) -> String {
        match self {
            RowState::Queued => "queued".to_owned(),
            RowState::Connecting => "connecting".to_owned(),
            RowState::Running(_) => "running".to_owned(),
            RowState::Done => "done".to_owned(),
            RowState::Failed(why) => format!("failed: {why}"),
            RowState::Stopped => "stopped".to_owned(),
        }
    }
}

/// One host's row in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    /// Its pane, once it has one.
    pub pane: Option<PaneId>,
    /// The host's session name.
    pub label: String,
    pub state: RowState,
    /// What its `@capture` steps kept, in order.
    pub values: Vec<String>,
    /// When it started connecting, for the timeout.
    started: Option<Instant>,
}

impl ReportRow {
    pub fn new(label: &str, pane: Option<PaneId>, state: RowState) -> Self {
        Self {
            pane,
            label: label.to_owned(),
            state,
            values: Vec::new(),
            started: None,
        }
    }
}

/// The last line of text a run's session showed, without colours, cut
/// short: why its login failed, for the report.
pub fn last_line(text: &str) -> Option<String> {
    let mut plain = String::new();
    OutputText::default().feed(text.as_bytes(), &mut plain);
    let line = plain
        .split(['\r', '\n'])
        .map(|l| {
            l.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
        })
        .rfind(|l| !l.is_empty())?;
    let mut out: String = line.chars().take(100).collect();
    if line.chars().count() > 100 {
        out.push('…');
    }
    Some(out)
}

/// A password typed for a run: kept in memory only, wiped when dropped,
/// and never printed.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Password(pub Zeroizing<String>);

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(..)")
    }
}

/// What a run on picked hosts types, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The snippet's lines, arguments and variables not yet filled in:
    /// the variables differ per host.
    pub lines: Vec<String>,
    /// The arguments' values, by number.
    pub args: Vec<(u8, String)>,
    pub parallel: usize,
    /// Typed for the hosts that ask for a password and have no password
    /// source; kept in memory for this run only.
    pub password: Option<Password>,
}

/// A live report of a snippet run on several hosts, or one with
/// `@capture` steps: a row per host, updated as each runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub name: String,
    pub rows: Vec<ReportRow>,
    /// The selected row, which Enter shows.
    pub selected: usize,
    /// For a run on picked hosts: what it runs. `None` for panes that
    /// were already open.
    pub run: Option<Run>,
}

/// Quotes a CSV field when it needs it.
fn csv_field(text: &str) -> String {
    let text = text.replace(['\r', '\n'], " ");
    if text.contains([',', '"', ';']) || text.starts_with([' ', '=', '+', '-', '@']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

impl Report {
    pub fn new(name: &str, rows: Vec<ReportRow>, run: Option<Run>) -> Self {
        Self {
            name: name.to_owned(),
            rows,
            selected: 0,
            run,
        }
    }

    pub fn row_mut(&mut self, pane: PaneId) -> Option<&mut ReportRow> {
        self.rows.iter_mut().find(|r| r.pane == Some(pane))
    }

    /// Rows logging in or running.
    pub fn active(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r.state, RowState::Connecting | RowState::Running(_)))
            .count()
    }

    /// Whether every row is over.
    pub fn over(&self) -> bool {
        self.rows.iter().all(|r| r.state.over())
    }

    /// The queued rows that may start now, marked as connecting.
    pub fn starting(&mut self, now: Instant) -> Vec<usize> {
        let Some(parallel) = self.run.as_ref().map(|r| r.parallel.clamp(1, MAX_PARALLEL)) else {
            return Vec::new();
        };
        let mut free = parallel.saturating_sub(self.active());
        let mut out = Vec::new();
        for (i, r) in self.rows.iter_mut().enumerate() {
            if free == 0 {
                break;
            }
            if r.state == RowState::Queued {
                r.state = RowState::Connecting;
                r.started = Some(now);
                free -= 1;
                out.push(i);
            }
        }
        out
    }

    /// Rows past [`HOST_TIMEOUT`], now failed; their panes.
    pub fn timed_out(&mut self, now: Instant) -> Vec<PaneId> {
        let mut out = Vec::new();
        for r in &mut self.rows {
            if matches!(r.state, RowState::Connecting | RowState::Running(_))
                && r.started
                    .is_some_and(|s| now.duration_since(s) >= HOST_TIMEOUT)
            {
                r.state = RowState::Failed(format!(
                    "took longer than {} minutes",
                    HOST_TIMEOUT.as_secs() / 60
                ));
                out.extend(r.pane);
            }
        }
        out
    }

    /// The next deadline of a running row.
    pub fn deadline(&self) -> Option<Instant> {
        self.rows
            .iter()
            .filter(|r| matches!(r.state, RowState::Connecting | RowState::Running(_)))
            .filter_map(|r| r.started.map(|s| s + HOST_TIMEOUT))
            .min()
    }

    /// Stops the run: queued rows never start, running ones stop. The
    /// panes of the rows that were running.
    pub fn cancel(&mut self) -> Vec<PaneId> {
        let mut out = Vec::new();
        for r in &mut self.rows {
            if !r.state.over() {
                if r.state != RowState::Queued {
                    out.extend(r.pane);
                }
                r.state = RowState::Stopped;
            }
        }
        out
    }

    /// Queues the failed and stopped rows again; the panes they had.
    pub fn retry(&mut self) -> Vec<PaneId> {
        if self.run.is_none() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for r in &mut self.rows {
            if matches!(r.state, RowState::Failed(_) | RowState::Stopped) {
                out.extend(r.pane.take());
                r.state = RowState::Queued;
                r.values.clear();
                r.started = None;
            }
        }
        out
    }

    /// The report as tab-separated text, for a spreadsheet.
    pub fn table(&self) -> String {
        let width = self.rows.iter().map(|r| r.values.len()).max().unwrap_or(0);
        let mut out = String::from("Host\tStatus");
        for i in 1..=width {
            out.push_str(&format!("\tValue {i}"));
        }
        out.push_str("\r\n");
        let cell = |t: &str| t.replace(['\t', '\r', '\n'], " ");
        for r in &self.rows {
            out.push_str(&format!("{}\t{}", cell(&r.label), cell(&r.state.label())));
            for v in &r.values {
                out.push('\t');
                out.push_str(&cell(v));
            }
            out.push_str("\r\n");
        }
        out
    }

    /// The report as CSV. Fields a spreadsheet would read as a formula
    /// are quoted.
    pub fn csv(&self) -> String {
        let width = self.rows.iter().map(|r| r.values.len()).max().unwrap_or(0);
        let mut out = String::from("Host,Status");
        for i in 1..=width {
            out.push_str(&format!(",Value {i}"));
        }
        out.push_str("\r\n");
        for r in &self.rows {
            out.push_str(&csv_field(&r.label));
            out.push(',');
            out.push_str(&csv_field(&r.state.label()));
            for v in &r.values {
                out.push(',');
                out.push_str(&csv_field(v));
            }
            out.push_str("\r\n");
        }
        out
    }
}

/// A host the picker offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickHost {
    pub place: Place,
    /// `user@host`, shown beside the name.
    pub detail: String,
    pub picked: bool,
}

/// One line of the picker's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickLine {
    /// A folder's heading (`None`: hosts in no folder).
    Folder(usize),
    Host(usize),
}

/// Which field of the picker has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickField {
    List,
    Parallel,
    Password,
}

/// The page that picks the hosts to run a snippet on.
#[derive(Debug, Clone)]
pub struct Picker {
    pub name: String,
    pub lines: Vec<String>,
    pub hosts: Vec<PickHost>,
    /// The folder names, in order; hosts in no folder come last.
    pub folders: Vec<Option<String>>,
    pub filter: String,
    pub selected: usize,
    pub scroll: usize,
    pub field: PickField,
    pub parallel: String,
    pub password: Password,
    pub error: Option<String>,
}

impl Picker {
    /// Hosts picked at first are the ones the snippet is kept to, when it
    /// is kept to some.
    pub fn new(name: &str, lines: &[String], hosts: Vec<PickHost>, parallel: usize) -> Self {
        let mut folders: Vec<Option<String>> = Vec::new();
        for h in &hosts {
            if h.place.folder.is_some() && !folders.contains(&h.place.folder) {
                folders.push(h.place.folder.clone());
            }
        }
        folders.push(None);
        Self {
            name: name.to_owned(),
            lines: lines.to_vec(),
            hosts,
            folders,
            filter: String::new(),
            selected: 0,
            scroll: 0,
            field: PickField::List,
            parallel: parallel.to_string(),
            password: Password::default(),
            error: None,
        }
    }

    fn shows(&self, h: &PickHost) -> bool {
        let hay = format!(
            "{} {} {}",
            h.place.session,
            h.detail,
            h.place.folder.as_deref().unwrap_or_default()
        )
        .to_lowercase();
        self.filter
            .split_whitespace()
            .all(|w| hay.contains(&w.to_lowercase()))
    }

    /// The list as shown: each folder's heading, then its hosts.
    pub fn lines(&self) -> Vec<PickLine> {
        let mut out = Vec::new();
        for (f, folder) in self.folders.iter().enumerate() {
            let hosts: Vec<usize> = self
                .hosts
                .iter()
                .enumerate()
                .filter(|(_, h)| h.place.folder == *folder && self.shows(h))
                .map(|(i, _)| i)
                .collect();
            if hosts.is_empty() {
                continue;
            }
            if folder.is_some() {
                out.push(PickLine::Folder(f));
            }
            out.extend(hosts.into_iter().map(PickLine::Host));
        }
        out
    }

    pub fn picked(&self) -> usize {
        self.hosts.iter().filter(|h| h.picked).count()
    }

    /// Picks or drops the selected host, or all the shown hosts of the
    /// selected folder.
    pub fn toggle(&mut self) {
        let lines = self.lines();
        match lines.get(self.selected) {
            Some(PickLine::Host(i)) => {
                if let Some(h) = self.hosts.get_mut(*i) {
                    h.picked = !h.picked;
                }
            }
            Some(PickLine::Folder(f)) => {
                let folder = self.folders.get(*f).cloned().flatten();
                let members: Vec<usize> = lines
                    .iter()
                    .filter_map(|l| match l {
                        PickLine::Host(i) => Some(*i),
                        PickLine::Folder(_) => None,
                    })
                    .filter(|&i| self.hosts.get(i).is_some_and(|h| h.place.folder == folder))
                    .collect();
                let all = members
                    .iter()
                    .all(|&i| self.hosts.get(i).is_some_and(|h| h.picked));
                for i in members {
                    if let Some(h) = self.hosts.get_mut(i) {
                        h.picked = !all;
                    }
                }
            }
            None => {}
        }
        self.error = None;
    }

    /// The hosts to run on and the number at once, or why not.
    pub fn start(&mut self) -> Option<(Vec<Place>, usize)> {
        let parallel = match self.parallel.trim().parse::<usize>() {
            Ok(n) if (1..=MAX_PARALLEL).contains(&n) => n,
            _ => {
                self.error = Some(format!("At once takes 1 to {MAX_PARALLEL} hosts."));
                self.field = PickField::Parallel;
                return None;
            }
        };
        let hosts: Vec<Place> = self
            .hosts
            .iter()
            .filter(|h| h.picked)
            .map(|h| h.place.clone())
            .collect();
        if hosts.is_empty() {
            self.error = Some("Pick at least one host: Space picks it.".into());
            self.field = PickField::List;
            return None;
        }
        Some((hosts, parallel))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn run(parallel: usize) -> Option<Run> {
        Some(Run {
            lines: vec!["show version".into()],
            args: Vec::new(),
            parallel,
            password: None,
        })
    }

    fn queued(n: usize) -> Vec<ReportRow> {
        (0..n)
            .map(|i| ReportRow::new(&format!("sw{i}"), None, RowState::Queued))
            .collect()
    }

    #[test]
    fn hosts_start_a_few_at_a_time() {
        let now = Instant::now();
        let mut r = Report::new("Version", queued(5), run(2));
        assert_eq!(r.starting(now), [0, 1]);
        assert!(r.starting(now).is_empty(), "both slots are taken");
        r.rows[0].pane = Some(10);
        r.rows[0].state = RowState::Done;
        assert_eq!(r.starting(now), [2]);
        r.rows[1].state = RowState::Failed("no".into());
        r.rows[2].state = RowState::Running(None);
        assert_eq!(r.starting(now), [3]);
        assert!(!r.over());
        // Panes that were already open never queue.
        let mut open = Report::new("x", queued(2), None);
        assert!(open.starting(now).is_empty());
    }

    #[test]
    fn slow_hosts_time_out_and_runs_cancel_and_retry() {
        let now = Instant::now();
        let mut r = Report::new("Version", queued(3), run(2));
        r.starting(now);
        r.rows[0].pane = Some(1);
        r.rows[1].pane = Some(2);
        assert_eq!(r.deadline(), Some(now + HOST_TIMEOUT));
        assert!(r.timed_out(now + HOST_TIMEOUT / 2).is_empty());
        r.rows[1].state = RowState::Done;
        assert_eq!(r.timed_out(now + HOST_TIMEOUT), [1]);
        assert!(matches!(&r.rows[0].state, RowState::Failed(w) if w.contains("2 minutes")));
        r.starting(now + HOST_TIMEOUT);
        r.rows[2].pane = Some(3);
        assert_eq!(r.cancel(), [3]);
        assert!(r.over());
        assert_eq!(r.retry(), [1, 3], "done rows stay as they are");
        assert_eq!(r.rows[1].state, RowState::Done);
        assert_eq!(r.rows[0].state, RowState::Queued);
        assert_eq!(r.rows[0].pane, None);
    }

    #[test]
    fn reports_export_as_table_and_csv() {
        let mut rows = queued(2);
        rows[0].state = RowState::Done;
        rows[0].values = vec!["15.2(4)E10".into(), "a,b".into()];
        rows[1].state = RowState::Failed("asked for a password".into());
        rows[1].label = "=cmd|' /C calc'!A0".into();
        let r = Report::new("Version", rows, None);
        assert_eq!(
            r.table(),
            "Host\tStatus\tValue 1\tValue 2\r\nsw0\tdone\t15.2(4)E10\ta,b\r\n\
             =cmd|' /C calc'!A0\tfailed: asked for a password\r\n"
        );
        assert_eq!(
            r.csv(),
            "Host,Status,Value 1,Value 2\r\nsw0,done,15.2(4)E10,\"a,b\"\r\n\
             \"=cmd|' /C calc'!A0\",failed: asked for a password\r\n"
        );
    }

    #[test]
    fn the_last_line_says_why() {
        assert_eq!(
            last_line("\x1b[0;31mConnecting...\r\nConnection refused\x1b[0m\r\n\r\n").as_deref(),
            Some("Connection refused")
        );
        assert_eq!(
            last_line("\x1b[0;90m[Disconnected: too many authentication failures]\x1b[0m\r\n")
                .as_deref(),
            Some("Disconnected: too many authentication failures")
        );
        assert_eq!(last_line("\r\n \x1b[0m"), None);
        assert_eq!(
            last_line(&"x".repeat(300)).map(|l| l.chars().count()),
            Some(101)
        );
    }

    fn host(name: &str, folder: Option<&str>, picked: bool) -> PickHost {
        PickHost {
            place: Place {
                session: name.into(),
                host: format!("{name}.lab"),
                folder: folder.map(str::to_owned),
            },
            detail: format!("admin@{name}.lab"),
            picked,
        }
    }

    #[test]
    fn the_picker_groups_by_folder_and_picks_whole_folders() {
        let mut p = Picker::new(
            "Version",
            &["show version".into()],
            vec![
                host("sw1", Some("Network"), true),
                host("web", None, false),
                host("sw2", Some("Network"), false),
            ],
            4,
        );
        assert_eq!(
            p.lines(),
            [
                PickLine::Folder(0),
                PickLine::Host(0),
                PickLine::Host(2),
                PickLine::Host(1)
            ]
        );
        p.toggle();
        assert_eq!(p.picked(), 2, "the folder: all of it");
        p.toggle();
        assert_eq!(p.picked(), 0);
        p.selected = 3;
        p.toggle();
        assert_eq!(p.picked(), 1);
        p.filter = "SW2".into();
        assert_eq!(p.lines(), [PickLine::Folder(0), PickLine::Host(2)]);
        p.parallel = "0".into();
        assert_eq!(p.start(), None);
        assert_eq!(p.field, PickField::Parallel);
        p.parallel = "8".into();
        let (hosts, n) = p.start().unwrap();
        assert_eq!((hosts.len(), hosts[0].session.as_str(), n), (1, "web", 8));
        for h in &mut p.hosts {
            h.picked = false;
        }
        assert_eq!(p.start(), None);
        assert!(p.error.as_deref().unwrap().contains("Pick at least one"));
    }
}
