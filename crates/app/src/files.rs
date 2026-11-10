//! What the file transfer pane shows, kept apart from the Windows controls
//! (`files_panel`) so it can be tested anywhere: the rows of the Local
//! and Remote lists, the queue's rows, the Local list's folder, and where
//! the two lists and the gap with the arrow buttons go.
//!
//! The remote listing and the queue live in the host's [`Remote`]; the
//! local folder is read straight from disk here (the UI process does disk
//! I/O; the sandboxed workers never see local files).

use std::path::{Path, PathBuf};

use ferossh_config::Bookmark;
use ferossh_ipc::DirEntry;
use ferossh_sftp::{JobId, Kind, Listing, Remote, State, names};

use crate::layout::Rect;
use crate::local_dir::{LocalDir, Up};

/// Whether the Remote list follows the terminal's directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follow {
    Off,
    /// Following, and the focused pane's shell has reported its directory.
    On,
    /// Following is on, but the focused pane's shell has not reported its
    /// directory, so there is nothing to follow.
    Unreported,
}

/// One row of the Local or Remote list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    /// As shown; ".." for the row that goes up.
    pub name: String,
    /// What a remote row is called on the server, or a local row's path;
    /// empty for "..".
    pub key: Vec<u8>,
    pub local: Option<PathBuf>,
    pub up: bool,
    /// A folder, or a link (which opens like one).
    pub dir: bool,
    pub size: Option<u64>,
    /// Seconds since 1970.
    pub modified: Option<i64>,
    /// `drwxr-xr-x` and so on; empty for local rows.
    pub rights: String,
}

impl FileRow {
    fn up() -> Self {
        Self {
            name: "..".into(),
            key: Vec::new(),
            local: None,
            up: true,
            dir: true,
            size: None,
            modified: None,
            rights: String::new(),
        }
    }
}

/// Whether `name` matches the filter `find`: any part of the name, in
/// any case; with `*` or `?`, the whole name as a wildcard pattern.
pub fn matches(name: &str, find: &str) -> bool {
    if find.is_empty() {
        return true;
    }
    if find.contains(['*', '?']) {
        ferossh_config::match_pattern(name, find)
    } else {
        name.to_lowercase().contains(&find.to_lowercase())
    }
}

/// `ls -l`'s first column for Unix mode bits.
pub fn rights(mode: Option<u32>) -> String {
    let Some(m) = mode else {
        return String::new();
    };
    let kind = match m & 0o170_000 {
        0o040_000 => 'd',
        0o120_000 => 'l',
        0o020_000 => 'c',
        0o060_000 => 'b',
        0o010_000 => 'p',
        0o140_000 => 's',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(kind);
    for shift in [6, 3, 0] {
        let bits = (m >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

fn remote_row(e: &DirEntry) -> FileRow {
    FileRow {
        name: names::display(&e.name),
        key: e.name.clone(),
        local: None,
        up: false,
        dir: e.attrs.is_dir() || e.attrs.is_symlink(),
        size: (!e.attrs.is_dir()).then_some(e.attrs.size).flatten(),
        modified: e.attrs.mtime.map(i64::from),
        rights: rights(e.attrs.permissions),
    }
}

/// The Remote list's rows: ".." (unless at the root or filtering), then
/// the entries whose names match `find`, folders first.
pub fn remote_rows(listing: &Listing, find: &str) -> Vec<FileRow> {
    let path = &listing.path;
    let mut rows = Vec::new();
    if find.is_empty() && !path.is_empty() && path.as_slice() != b"/" {
        rows.push(FileRow::up());
    }
    let mut entries: Vec<FileRow> = listing
        .entries
        .iter()
        .map(remote_row)
        .filter(|r| matches(&r.name, find))
        .collect();
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows.extend(entries);
    rows
}

/// The Local list's rows, the same way.
pub fn local_rows(dir: &LocalDir, find: &str) -> Vec<FileRow> {
    let mut rows = Vec::new();
    if find.is_empty() && dir.up.is_some() {
        rows.push(FileRow::up());
    }
    rows.extend(
        dir.entries
            .iter()
            .filter(|e| matches(&e.name, find))
            .map(|e| FileRow {
                name: e.name.clone(),
                key: e.name.as_bytes().to_vec(),
                local: Some(e.path.clone()),
                up: false,
                dir: e.dir,
                size: (!e.dir).then_some(e.size),
                modified: e.modified,
                rights: String::new(),
            }),
    );
    rows
}

/// `rows` with only the names matching `find`, and no ".." while
/// filtering.
pub fn filtered(rows: &[FileRow], find: &str) -> Vec<FileRow> {
    rows.iter()
        .filter(|r| {
            if r.up {
                find.is_empty()
            } else {
                matches(&r.name, find)
            }
        })
        .cloned()
        .collect()
}

/// The entries of the listed remote folder called `keys`.
pub fn entries_named(listing: &Listing, keys: &[Vec<u8>]) -> Vec<DirEntry> {
    listing
        .entries
        .iter()
        .filter(|e| keys.contains(&e.name))
        .cloned()
        .collect()
}

/// One row of the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueRow {
    pub id: JobId,
    /// Indented under its folder's row.
    pub name: String,
    pub upload: bool,
    pub state: State,
    /// Done of 100, when the size is known.
    pub percent: Option<u64>,
    pub status: String,
}

fn percent(done: u64, size: Option<u64>) -> Option<u64> {
    let size = size.filter(|&s| s > 0)?;
    Some((done.min(size) * 100) / size)
}

/// The status column for a job.
fn status(job: &ferossh_sftp::Job) -> String {
    let pct = percent(job.done, job.size);
    match job.state {
        State::Queued => "queued".into(),
        State::Asking => "exists".into(),
        State::Running => {
            let mut s = match pct {
                Some(p) => format!("{p}%"),
                None => names::size(job.done),
            };
            if let Some(rate) = job.rate().filter(|r| *r > 0.0) {
                s.push_str(&format!(" · {}/s", names::size(rate as u64)));
            }
            s
        }
        State::Paused => match pct {
            Some(p) => format!("paused {p}%"),
            None => "paused".into(),
        },
        State::Waiting => "waiting for the connection".into(),
        State::Done => job.note.clone().unwrap_or_else(|| "done".into()),
        State::Failed => match &job.note {
            Some(n) => format!("failed: {n}"),
            None => "failed".into(),
        },
        State::Cancelled => "cancelled".into(),
        State::Skipped => "skipped".into(),
    }
}

/// The queue's rows: every top-level job, and the files of a folder that
/// are moving, stuck or asking.
pub fn queue_rows(remote: &Remote) -> Vec<QueueRow> {
    remote
        .jobs()
        .iter()
        .filter(|j| {
            j.parent.is_none()
                || matches!(
                    j.state,
                    State::Running | State::Asking | State::Failed | State::Paused
                )
        })
        .map(|j| QueueRow {
            id: j.id,
            name: if j.parent.is_some() {
                format!("    {}", j.name)
            } else {
                j.name.clone()
            },
            upload: matches!(j.kind, Kind::Upload | Kind::SendFolder),
            state: j.state,
            percent: if j.state == State::Done {
                Some(100)
            } else {
                percent(j.done, j.size)
            },
            status: status(j),
        })
        .collect()
}

/// Finished downloads, so one that lands shows up in the Local list.
pub fn landed(remote: &Remote) -> usize {
    remote
        .jobs()
        .iter()
        .filter(|j| {
            j.parent.is_none()
                && j.state == State::Done
                && matches!(j.kind, Kind::Download | Kind::FetchFolder)
        })
        .count()
}

/// The Local list's folder.
#[derive(Debug, Default)]
pub struct LocalPane {
    pub listing: LocalDir,
    /// The download folder the window last gave: when it changes (the
    /// folder picker, Settings) the list moves there.
    given: Option<PathBuf>,
    landed: usize,
}

impl LocalPane {
    /// Follows the download folder `base`, and reads the folder again
    /// when a download has landed (`landed` counts them). True when the
    /// listing changed.
    pub fn sync(&mut self, base: &Path, landed: usize) -> bool {
        let mut changed = false;
        if self.given.as_deref() != Some(base) {
            self.given = Some(base.to_path_buf());
            if self.listing.dir.as_deref() != Some(base) {
                self.listing = LocalDir::read(base);
                changed = true;
            }
        } else if landed != self.landed {
            self.reload();
            changed = true;
        }
        self.landed = landed;
        changed
    }

    /// Reads the listed folder again.
    pub fn reload(&mut self) {
        self.listing = match &self.listing.dir {
            Some(d) => LocalDir::read(d),
            None => LocalDir::drives(),
        };
    }

    /// Lists folder `dir`. A folder that cannot be read is not entered.
    pub fn enter(&mut self, dir: &Path) -> Result<(), String> {
        let listing = LocalDir::read(dir);
        if let Some(e) = listing.error {
            return Err(e);
        }
        self.listing = listing;
        Ok(())
    }

    /// One folder up, or to the drives above a drive's root; the folder
    /// left, to select it.
    pub fn up(&mut self) -> Result<Option<PathBuf>, String> {
        let here = self.listing.dir.clone();
        match self.listing.up.clone() {
            Some(Up::Folder(p)) => self.enter(&p).map(|()| here),
            Some(Up::Drives) => {
                self.listing = LocalDir::drives();
                Ok(here)
            }
            None => Ok(None),
        }
    }

    /// The listed folder (`None` for the drives).
    pub fn dir(&self) -> Option<&Path> {
        self.listing.dir.as_deref()
    }
}

/// How the two lists are arranged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrange {
    /// One above the other, else side by side.
    pub stacked: bool,
    /// Remote first (left or top), else Local first.
    pub swapped: bool,
    /// The first list's share of the room, in percent.
    pub split: u8,
}

impl Default for Arrange {
    fn default() -> Self {
        Self {
            stacked: false,
            swapped: false,
            split: 50,
        }
    }
}

impl Arrange {
    /// The `TransferLayout` setting: `side` or `stacked`, then `swapped`
    /// and the split, such as `side swapped 40`.
    pub fn parse(text: &str) -> Self {
        let mut a = Self::default();
        for word in text.split_whitespace() {
            match word.to_ascii_lowercase().as_str() {
                "stacked" => a.stacked = true,
                "side" => a.stacked = false,
                "swapped" => a.swapped = true,
                n => {
                    if let Ok(n) = n.parse::<u8>() {
                        a.split = n.clamp(MIN_SPLIT, 100 - MIN_SPLIT);
                    }
                }
            }
        }
        a
    }

    pub fn to_setting(self) -> String {
        format!(
            "{}{} {}",
            if self.stacked { "stacked" } else { "side" },
            if self.swapped { " swapped" } else { "" },
            self.split
        )
    }

    /// The arrow that moves files from the Local list to the Remote list,
    /// and the one back.
    pub fn arrows(self) -> (&'static str, &'static str) {
        match (self.stacked, self.swapped) {
            (false, false) => ("→", "←"),
            (false, true) => ("←", "→"),
            (true, false) => ("↓", "↑"),
            (true, true) => ("↑", "↓"),
        }
    }
}

/// The smallest share either list gets.
pub const MIN_SPLIT: u8 = 15;

/// Where the parts go in `area`: the Local list's block, the gap with
/// the arrow buttons (`gap` wide or tall), and the Remote list's block.
pub fn arrange(a: Arrange, area: Rect, gap: i32) -> (Rect, Rect, Rect) {
    let along = if a.stacked { area.h } else { area.w };
    let room = (along - gap).max(0);
    let first = room * i32::from(a.split) / 100;
    let second = room - first;
    let (r1, g, r2) = if a.stacked {
        (
            Rect::new(area.x, area.y, area.w, first),
            Rect::new(area.x, area.y + first, area.w, gap.min(along)),
            Rect::new(area.x, area.y + first + gap, area.w, second),
        )
    } else {
        (
            Rect::new(area.x, area.y, first, area.h),
            Rect::new(area.x + first, area.y, gap.min(along), area.h),
            Rect::new(area.x + first + gap, area.y, second, area.h),
        )
    };
    if a.swapped { (r2, g, r1) } else { (r1, g, r2) }
}

/// The split for a drag that put the gap's middle at `pos` (along the
/// arrangement's axis, in `area`).
pub fn split_at(a: Arrange, area: Rect, gap: i32, pos: i32) -> u8 {
    let (start, along) = if a.stacked {
        (area.y, area.h)
    } else {
        (area.x, area.w)
    };
    let room = (along - gap).max(1);
    let first = (pos - start - gap / 2).clamp(0, room);
    // The first (left or top) block's share, whichever list it shows.
    let pct = (first * 100 / room) as u8;
    pct.clamp(MIN_SPLIT, 100 - MIN_SPLIT)
}

/// The bookmarks that go with connection `alias` (those for any host, and
/// those for this one), with their indexes in `all`, sorted by name.
pub fn bookmarks_for<'a>(all: &'a [Bookmark], alias: Option<&str>) -> Vec<(usize, &'a Bookmark)> {
    let mut shown: Vec<(usize, &Bookmark)> = all
        .iter()
        .enumerate()
        .filter(|(_, b)| match (&b.host, alias) {
            (None, _) => true,
            (Some(h), Some(a)) => h.eq_ignore_ascii_case(a),
            (Some(_), None) => false,
        })
        .collect();
    shown.sort_by_key(|(_, b)| b.name.to_lowercase());
    shown
}

/// A bookmark name as kept: trimmed, without quotes or control
/// characters (it is written quoted on one line); `None` when empty.
pub fn bookmark_name(text: &str) -> Option<String> {
    let name: String = text
        .chars()
        .filter(|c| *c != '"' && !c.is_control())
        .take(60)
        .collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// `all` with `new` added, or put in place of bookmark `replacing`; one
/// with the same name for the same host goes.
pub fn put_bookmark(all: &[Bookmark], new: Bookmark, replacing: Option<usize>) -> Vec<Bookmark> {
    let mut out: Vec<Bookmark> = all
        .iter()
        .enumerate()
        .filter(|(i, b)| Some(*i) != replacing && !b.same_name(&new))
        .map(|(_, b)| b.clone())
        .collect();
    out.push(new);
    out
}

/// A name for a new bookmark: the last part of the remote folder, else
/// of the local one.
pub fn bookmark_suggestion(local: Option<&Path>, remote: Option<&[u8]>) -> String {
    let remote = remote
        .map(names::file_name)
        .filter(|n| !n.is_empty() && *n != b"/")
        .map(|n| String::from_utf8_lossy(n).into_owned());
    let local = local.and_then(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| Some(p.display().to_string()))
    });
    remote.or(local).unwrap_or_else(|| "Bookmark".to_owned())
}

/// The local folder Sync browsing opens for remote folder `remote` inside
/// `here`: the remote name mapped the way a download names it, so it is
/// always one plain name below `here`. `None` for a name that must not
/// reach the local file system at all, so a server-chosen name such as
/// `\\host\share`, `C:\` or `..` is never probed.
pub fn sync_local(here: &Path, remote: &[u8]) -> Option<PathBuf> {
    use std::path::Component;
    let name = names::local_name(remote)?;
    let mut parts = Path::new(&name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(one)), None) if one == name.as_str() => Some(here.join(one)),
        _ => None,
    }
}

/// Sync browsing moves both lists or neither: the remote folder is asked
/// for first, and the Local list moves only once it has listed. If the
/// Local list then cannot move, the Remote list goes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPending {
    /// The transfer queue it was asked on.
    pub key: Option<String>,
    /// The remote folder asked for.
    pub target: Vec<u8>,
    /// The remote folder it left, to go back to.
    pub back: Vec<u8>,
    pub local: SyncLocal,
}

/// What the Local list does once the remote folder has listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncLocal {
    Enter(PathBuf),
    Up,
}

/// Where a pending Sync move stands after a new view of the Remote list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncStep {
    /// Nothing pending, or dropped: another queue or connection.
    None,
    /// Still listing: keep waiting.
    Wait(SyncPending),
    /// The remote folder listed: move the Local list (and if that fails,
    /// browse `back`).
    Move(SyncPending),
    /// The remote folder did not open: the Local list stays.
    Failed,
}

/// Resolves `pending` against the Remote list now shown: queue `key`,
/// `alias_changed` since the last view, still `loading`, at `remote_dir`.
pub fn sync_step(
    pending: Option<SyncPending>,
    key: Option<&str>,
    alias_changed: bool,
    loading: bool,
    remote_dir: &[u8],
) -> SyncStep {
    match pending {
        None => SyncStep::None,
        Some(p) if p.key.as_deref() != key || alias_changed => SyncStep::None,
        Some(p) if loading => SyncStep::Wait(p),
        Some(p) if remote_dir == p.target.as_slice() => SyncStep::Move(p),
        Some(_) => SyncStep::Failed,
    }
}

/// A Sync move whose Local half failed: the Remote list is going back to
/// `path` on queue `key`, for connection `alias`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncBack {
    pub key: Option<String>,
    pub alias: Option<String>,
    pub path: Vec<u8>,
}

impl SyncBack {
    /// Whether the go-back still belongs to the Remote list showing queue
    /// `key` for connection `alias`: another connection on the same queue
    /// has its own folder.
    pub fn applies(&self, key: Option<&str>, alias: Option<&str>) -> bool {
        self.key.as_deref() == key && self.alias.as_deref() == alias
    }
}

/// Where a Remote go-back stands after a new view of the Remote list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackStep {
    /// Still listing: keep waiting.
    Wait,
    /// Another queue or connection now: nothing more to say.
    Dropped,
    /// Settled: the line to show under the lists.
    Done(String),
}

/// Resolves go-back `back` against the Remote list now shown (see
/// [`sync_step`]); `remote_error` is the Remote list's own problem line.
pub fn back_step(
    back: &SyncBack,
    key: Option<&str>,
    alias_changed: bool,
    loading: bool,
    remote_dir: &[u8],
    remote_error: Option<&str>,
) -> BackStep {
    if back.key.as_deref() != key || alias_changed {
        return BackStep::Dropped;
    }
    if loading {
        return BackStep::Wait;
    }
    if remote_dir == back.path.as_slice() {
        return BackStep::Done(
            "Sync browsing: the Local folder did not open, so Remote went back to where it was."
                .into(),
        );
    }
    BackStep::Done(match remote_error {
        Some(e) => format!(
            "Sync browsing: the Local folder did not open, and Remote could not go back: {e}"
        ),
        None => {
            "Sync browsing: the Local folder did not open, and Remote could not go back.".into()
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_ipc::FileAttrs;

    #[test]
    fn sync_go_back_reports_how_it_went() {
        let back = SyncBack {
            key: Some("k".into()),
            alias: Some("a".into()),
            path: b"/srv".to_vec(),
        };
        assert!(back.applies(Some("k"), Some("a")));
        assert!(
            !back.applies(Some("k"), Some("b")),
            "another connection on the queue"
        );
        assert!(!back.applies(Some("other"), Some("a")));
        let step =
            |key, alias, loading, dir: &[u8], err| back_step(&back, key, alias, loading, dir, err);
        assert_eq!(
            step(Some("k"), false, true, b"/srv/a", None),
            BackStep::Wait
        );
        assert!(matches!(
            step(Some("k"), false, false, b"/srv", None),
            BackStep::Done(t) if t.contains("went back")
        ));
        assert!(matches!(
            step(Some("k"), false, false, b"/srv/a", Some("Permission denied")),
            BackStep::Done(t) if t.contains("could not go back: Permission denied")
        ));
        assert!(matches!(
            step(Some("k"), false, false, b"/srv/a", None),
            BackStep::Done(t) if t.contains("could not go back")
        ));
        assert_eq!(
            step(Some("other"), false, false, b"/srv", None),
            BackStep::Dropped
        );
        assert_eq!(
            step(Some("k"), true, false, b"/srv", None),
            BackStep::Dropped
        );
    }

    #[test]
    fn sync_moves_local_only_once_remote_listed() {
        let p = SyncPending {
            key: Some("k".into()),
            target: b"/srv/a".to_vec(),
            back: b"/srv".to_vec(),
            local: SyncLocal::Up,
        };
        let step =
            |key, alias, loading, dir: &[u8]| sync_step(Some(p.clone()), key, alias, loading, dir);
        assert_eq!(
            step(Some("k"), false, true, b"/srv"),
            SyncStep::Wait(p.clone())
        );
        assert_eq!(
            step(Some("k"), false, false, b"/srv/a"),
            SyncStep::Move(p.clone())
        );
        // The move carries the folder to go back to if Local cannot move.
        if let SyncStep::Move(m) = step(Some("k"), false, false, b"/srv/a") {
            assert_eq!(m.back, b"/srv".to_vec());
        }
        assert_eq!(step(Some("k"), false, false, b"/srv"), SyncStep::Failed);
        assert_eq!(step(Some("other"), false, false, b"/srv/a"), SyncStep::None);
        assert_eq!(step(Some("k"), true, false, b"/srv/a"), SyncStep::None);
        assert_eq!(
            sync_step(None, Some("k"), false, false, b"/"),
            SyncStep::None
        );
    }

    fn entry(name: &str, mode: u32, size: u64) -> DirEntry {
        DirEntry {
            name: name.as_bytes().to_vec(),
            attrs: FileAttrs {
                size: Some(size),
                permissions: Some(mode),
                mtime: Some(1_762_700_000),
            },
        }
    }

    fn listing(path: &str) -> Listing {
        let mut l = Listing::default();
        l.path = path.as_bytes().to_vec();
        l.entries = vec![
            entry("zeta.txt", 0o100_644, 10),
            entry("Alpha", 0o040_755, 4096),
            entry("link", 0o120_777, 7),
            entry("beta.log", 0o100_600, 2048),
        ];
        l
    }

    #[test]
    fn remote_rows_put_folders_first_then_names() {
        let rows = remote_rows(&listing("/etc"), "");
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["..", "Alpha", "link", "beta.log", "zeta.txt"]);
        assert!(rows[0].up && rows[1].dir && rows[2].dir && !rows[3].dir);
        assert_eq!(rows[1].size, None);
        assert_eq!(rows[3].size, Some(2048));
        assert_eq!(rows[1].rights, "drwxr-xr-x");
        assert_eq!(rows[3].rights, "-rw-------");
        assert_eq!(rows[2].rights, "lrwxrwxrwx");
        assert_eq!(rows[4].key, b"zeta.txt");
    }

    #[test]
    fn the_root_has_no_up_row_and_a_filter_hides_it() {
        assert!(!remote_rows(&listing("/"), "")[0].up);
        let rows = remote_rows(&listing("/etc"), "TXT");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "zeta.txt");
        let rows = remote_rows(&listing("/etc"), "*.log");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "beta.log");
    }

    #[test]
    fn filtering_shown_rows_drops_the_up_row() {
        let rows = remote_rows(&listing("/etc"), "");
        assert_eq!(filtered(&rows, ""), rows);
        let got = filtered(&rows, "a");
        let names: Vec<_> = got.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "beta.log", "zeta.txt"]);
    }

    #[test]
    fn entries_are_found_by_name() {
        let l = listing("/etc");
        let got = entries_named(&l, &[b"beta.log".to_vec(), b"Alpha".to_vec()]);
        let names: Vec<_> = got.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, [b"Alpha".to_vec(), b"beta.log".to_vec()]);
    }

    #[test]
    fn local_rows_list_a_folder_and_follow_the_download_folder() {
        let dir = std::env::temp_dir().join(format!("ferossh-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"12345").unwrap();
        let mut pane = LocalPane::default();
        assert!(pane.sync(&dir, 0));
        assert!(!pane.sync(&dir, 0));
        let rows = local_rows(&pane.listing, "");
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["..", "sub", "a.txt"]);
        assert_eq!(rows[2].size, Some(5));
        assert!(rows[2].modified.is_some());
        assert_eq!(rows[2].local.as_deref(), Some(dir.join("a.txt").as_path()));
        // A download landed: read again.
        std::fs::write(dir.join("b.txt"), b"1").unwrap();
        assert!(pane.sync(&dir, 1));
        assert_eq!(local_rows(&pane.listing, "b.").len(), 1);
        pane.enter(&dir.join("sub")).unwrap();
        assert_eq!(pane.dir(), Some(dir.join("sub").as_path()));
        assert_eq!(pane.up().unwrap(), Some(dir.join("sub")));
        assert_eq!(pane.dir(), Some(dir.as_path()));
        assert!(pane.enter(&dir.join("missing")).is_err());
        assert_eq!(pane.dir(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arrange_splits_the_room_and_swaps() {
        let area = Rect::new(0, 0, 410, 300);
        let a = Arrange::default();
        let (l, g, r) = arrange(a, area, 10);
        assert_eq!((l.x, l.w, g.x, g.w, r.x, r.w), (0, 200, 200, 10, 210, 200));
        let swapped = Arrange {
            swapped: true,
            split: 25,
            ..a
        };
        let (l, _, r) = arrange(swapped, area, 10);
        // Remote first, with a quarter of the room.
        assert_eq!((r.x, r.w, l.x, l.w), (0, 100, 110, 300));
        let stacked = Arrange { stacked: true, ..a };
        let (l, g, r) = arrange(stacked, Rect::new(0, 0, 400, 210), 10);
        assert_eq!((l.y, l.h, g.y, r.y, r.h), (0, 100, 100, 110, 100));
        assert_eq!(stacked.arrows(), ("↓", "↑"));
        assert_eq!(swapped.arrows(), ("←", "→"));
    }

    #[test]
    fn dragging_the_gap_sets_the_split_within_limits() {
        let area = Rect::new(0, 0, 410, 300);
        let a = Arrange::default();
        assert_eq!(split_at(a, area, 10, 105), 25);
        assert_eq!(split_at(a, area, 10, 0), MIN_SPLIT);
        assert_eq!(split_at(a, area, 10, 400), 100 - MIN_SPLIT);
        let swapped = Arrange { swapped: true, ..a };
        // The split is the left block's share, whichever list it shows.
        assert_eq!(split_at(swapped, area, 10, 105), 25);
        let (_, gap, _) = arrange(
            Arrange {
                split: 25,
                ..swapped
            },
            area,
            10,
        );
        assert_eq!(gap.x + gap.w / 2, 105);
    }

    #[test]
    fn the_layout_setting_round_trips() {
        let a = Arrange {
            stacked: true,
            swapped: true,
            split: 40,
        };
        assert_eq!(a.to_setting(), "stacked swapped 40");
        assert_eq!(Arrange::parse(&a.to_setting()), a);
        assert_eq!(Arrange::parse("side 3").split, MIN_SPLIT);
        assert_eq!(Arrange::parse("nonsense"), Arrange::default());
    }

    #[test]
    fn rights_read_like_ls() {
        assert_eq!(rights(Some(0o100_755)), "-rwxr-xr-x");
        assert_eq!(rights(None), "");
    }
    fn mark(name: &str, host: Option<&str>) -> Bookmark {
        Bookmark {
            name: name.into(),
            local: None,
            remote: Some("/srv".into()),
            host: host.map(Into::into),
        }
    }

    #[test]
    fn bookmarks_show_for_any_host_or_this_one() {
        let all = [
            mark("web", Some("prod")),
            mark("Logs", None),
            mark("db", Some("other")),
        ];
        let at = |alias| -> Vec<usize> { bookmarks_for(&all, alias).iter().map(|b| b.0).collect() };
        assert_eq!(at(Some("PROD")), vec![1, 0]);
        assert_eq!(at(None), vec![1]);
    }

    #[test]
    fn a_bookmark_replaces_one_with_its_name() {
        let all = [mark("web", Some("prod")), mark("web", None)];
        let out = put_bookmark(&all, mark("WEB", Some("prod")), None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].name, "WEB");
        // Renaming bookmark 1 drops the old one.
        let out = put_bookmark(&all, mark("site", None), Some(1));
        assert_eq!(
            out.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            ["web", "site"]
        );
        assert_eq!(bookmark_name("  \"my\" site\n "), Some("my site".into()));
        assert_eq!(bookmark_name(" \" "), None);
    }

    #[test]
    fn a_new_bookmark_is_named_after_its_folder() {
        assert_eq!(bookmark_suggestion(None, Some(b"/var/www/")), "www");
        assert_eq!(
            bookmark_suggestion(Some(Path::new("/home/me/Sites")), Some(b"/")),
            "Sites"
        );
        assert_eq!(bookmark_suggestion(None, None), "Bookmark");
    }

    #[test]
    fn sync_browsing_never_leaves_the_local_folder() {
        let here = Path::new("base");
        assert_eq!(sync_local(here, b"logs"), Some(here.join("logs")));
        // Mapped like a download, so the folder it would have landed in.
        assert_eq!(sync_local(here, b"a:b"), Some(here.join("a_b")));
        for bad in [
            &b"\\\\host\\share"[..],
            b"C:\\",
            b"C:",
            b"\\x",
            b"..",
            b".",
            b"",
            b"a/b",
        ] {
            let got = sync_local(here, bad);
            assert!(
                got.as_ref().is_none_or(|p| p.parent() == Some(here)),
                "{bad:?} -> {got:?}"
            );
        }
        assert_eq!(sync_local(here, b".."), None);
        assert_eq!(sync_local(here, b"a/b"), None);
    }
}
