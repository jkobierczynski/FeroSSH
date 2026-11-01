//! The file transfer view: the side pane's listing of the focused pane's
//! remote directory, then the Local PC section (the folder downloads go
//! to, and uploads come from), then the transfer queue.
//!
//! Drawn into a [`Grid`] like the connection manager. The listing and the
//! queue live in the host's [`Remote`]; this view only keeps the
//! selection and turns keys and clicks into calls on it. What needs the
//! window (a confirm box, the file picker) comes out as a [`FilesAction`].

use std::path::{Path, PathBuf};

use ferossh_ipc::DirEntry;
use ferossh_render::{Cell, Color, Flags, Grid, Rgb};
use ferossh_sftp::{Choice, Job, JobId, Kind, Remote, State, names};

use crate::local_dir::{LocalDir, LocalEntry, Up};
use crate::sessions::SideKey;
use crate::tabstrip::{Palette, fit};

/// Rows above the listing: the title and the path.
const HEADER: usize = 2;
const OPTIONS: &str = " [O]verwrite  [S]kip  [K]eep both";

/// What the window should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesAction {
    /// Give the keyboard back to the terminal.
    Leave,
    /// Ask before deleting this entry of the listed folder.
    Delete(DirEntry),
    /// Show the file picker and upload what is picked into this folder.
    PickUpload(Vec<u8>),
    OpenDownloads,
    ToggleFollow,
    /// The Local PC section moved to this folder: downloads go there now.
    SetLocal(PathBuf),
    /// Show the folder picker for the Local PC section.
    PickLocal,
    /// Upload these local files or folders into this remote folder.
    Upload {
        paths: Vec<PathBuf>,
        dir: Vec<u8>,
    },
}

/// Whether the listing follows the terminal's directory, as the title
/// shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follow {
    Off,
    /// Following, and the focused pane's shell has reported its directory.
    On,
    /// Following is on, but the focused pane's shell has not reported its
    /// directory (OSC 7), so there is nothing to follow.
    Unreported,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Focus {
    #[default]
    List,
    Local,
    Queue,
}

/// Where each part of the pane is, for a grid of some height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    /// Listing rows: first row and count.
    list: (usize, usize),
    /// The Local PC section's title row, then its rows: first row and count.
    local: Option<(usize, usize, usize)>,
    /// The queue's title row, then its rows: first row and count.
    queue: Option<(usize, usize, usize)>,
    /// The question's two rows.
    question: Option<usize>,
    footer: usize,
}

fn geometry(rows: usize, queued: usize, asking: bool) -> Geometry {
    let footer = rows.saturating_sub(1);
    let mut bottom = footer;
    let question = if asking && bottom >= HEADER + 3 {
        bottom -= 2;
        Some(bottom)
    } else {
        None
    };
    let queue = if queued > 0 && bottom >= HEADER + 3 {
        let room = bottom.saturating_sub(HEADER + 2);
        let n = queued.min((rows / 3).max(2)).min(room).max(1);
        bottom -= n + 1;
        Some((bottom, bottom + 1, n))
    } else {
        None
    };
    // The Local PC section, when the listing keeps three rows: a quarter
    // of the pane, whatever the folder holds, so nothing jumps.
    let local = if bottom >= HEADER + 3 + 2 {
        let room = bottom - HEADER - 3 - 1;
        let n = (rows / 4).clamp(3, 8).min(room);
        bottom -= n + 1;
        Some((bottom, bottom + 1, n))
    } else {
        None
    };
    Geometry {
        list: (HEADER, bottom.saturating_sub(HEADER).max(1)),
        local,
        queue,
        question,
        footer,
    }
}

/// The jobs the queue shows: every top-level job, and the files of a
/// folder that are moving, stuck or asking.
fn queue_rows(remote: &Remote) -> Vec<usize> {
    remote
        .jobs()
        .iter()
        .enumerate()
        .filter(|(_, j)| {
            j.parent.is_none()
                || matches!(
                    j.state,
                    State::Running | State::Asking | State::Failed | State::Paused
                )
        })
        .map(|(i, _)| i)
        .collect()
}

fn has_up(remote: &Remote) -> bool {
    let path = &remote.listing.path;
    !path.is_empty() && path.as_slice() != b"/"
}

/// What a listing row is.
enum Item<'a> {
    Up,
    Entry(&'a DirEntry),
}

fn item(remote: &Remote, i: usize) -> Option<Item<'_>> {
    if has_up(remote) {
        if i == 0 {
            return Some(Item::Up);
        }
        return remote.listing.entries.get(i - 1).map(Item::Entry);
    }
    remote.listing.entries.get(i).map(Item::Entry)
}

fn item_count(remote: &Remote) -> usize {
    remote.listing.entries.len() + usize::from(has_up(remote))
}

/// `left`, then `right` against the right edge, in `width` cells.
fn spread(left: &str, right: &str, width: usize) -> String {
    let rw = unicode_width::UnicodeWidthStr::width(right);
    if rw + 2 >= width {
        return fit(left, width);
    }
    let left = fit(left, width - rw - 1);
    let lw = unicode_width::UnicodeWidthStr::width(left.as_str());
    format!("{left}{}{right}", " ".repeat(width - rw - lw))
}

fn percent(job: &Job) -> Option<u64> {
    let size = job.size.filter(|&s| s > 0)?;
    Some((job.done.min(size) * 100) / size)
}

/// The queue's status column for a job.
fn status(job: &Job) -> String {
    match job.state {
        State::Queued => "queued".into(),
        State::Asking => "exists".into(),
        State::Running => {
            let mut s = match percent(job) {
                Some(p) => format!("{p}%"),
                None => names::size(job.done),
            };
            if let Some(rate) = job.rate().filter(|r| *r > 0.0) {
                s.push_str(&format!(" · {}/s", names::size(rate as u64)));
            }
            s
        }
        State::Paused => match percent(job) {
            Some(p) => format!("paused {p}%"),
            None => "paused".into(),
        },
        State::Waiting => "waiting".into(),
        State::Done => job.note.clone().unwrap_or_else(|| "done".into()),
        State::Failed => "failed".into(),
        State::Cancelled => "cancelled".into(),
        State::Skipped => "skipped".into(),
    }
}

/// The last `width` cells of `text`, with "…" in front when cut.
fn tail(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if unicode_width::UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut kept = Vec::new();
    let mut used = 1;
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        kept.push(c);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

/// Finished downloads, so one that lands shows up in the Local PC listing.
fn landed(remote: &Remote) -> usize {
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

/// What a Local PC row is.
enum LocalItem<'a> {
    Up,
    Entry(&'a LocalEntry),
}

/// The Local PC section: a folder on this PC and the selection in it.
#[derive(Debug, Default)]
struct LocalView {
    listing: LocalDir,
    /// The download folder the window last gave: when it changes (the
    /// folder picker, Settings) to a folder not listed, the section lists
    /// it. Moving around here changes it after the window has heard.
    given: Option<PathBuf>,
    selected: usize,
    scroll: usize,
    landed: usize,
}

impl LocalView {
    fn count(&self) -> usize {
        self.listing.entries.len() + usize::from(self.listing.up.is_some())
    }

    fn item(&self, i: usize) -> Option<LocalItem<'_>> {
        if self.listing.up.is_some() {
            if i == 0 {
                return Some(LocalItem::Up);
            }
            return self.listing.entries.get(i - 1).map(LocalItem::Entry);
        }
        self.listing.entries.get(i).map(LocalItem::Entry)
    }

    /// Shows `listing`, with `select` selected when it is in it.
    fn show(&mut self, listing: LocalDir, select: Option<&Path>) {
        let skip = usize::from(listing.up.is_some());
        self.selected = select
            .and_then(|p| listing.entries.iter().position(|e| e.path == p))
            .map_or(0, |i| i + skip);
        self.scroll = 0;
        self.listing = listing;
    }

    /// Reads the listed folder again, keeping the selection.
    fn reload(&mut self) {
        let keep = match self.item(self.selected) {
            Some(LocalItem::Entry(e)) => Some(e.path.clone()),
            _ => None,
        };
        let listing = match &self.listing.dir {
            Some(d) => LocalDir::read(d),
            None => LocalDir::drives(),
        };
        self.show(listing, keep.as_deref());
    }

    /// Follows the download folder `base`, and reads the folder again when
    /// a download has landed.
    fn sync(&mut self, base: &Path, remote: &Remote) {
        let landed = landed(remote);
        if self.given.as_deref() != Some(base) {
            self.given = Some(base.to_path_buf());
            if self.listing.dir.as_deref() != Some(base) {
                self.show(LocalDir::read(base), None);
            }
        } else if landed != self.landed {
            self.reload();
        }
        self.landed = landed;
    }

    /// Lists folder `dir`, selecting `select`; the window makes it the
    /// download folder. A folder that cannot be read is not entered.
    fn enter(&mut self, dir: PathBuf, select: Option<&Path>) -> Result<FilesAction, String> {
        let listing = LocalDir::read(&dir);
        if let Some(e) = listing.error {
            return Err(e);
        }
        self.show(listing, select);
        Ok(FilesAction::SetLocal(dir))
    }

    /// One folder up, or to the drives above a drive's root.
    fn up(&mut self) -> Result<Option<FilesAction>, String> {
        let here = self.listing.dir.clone();
        match self.listing.up.clone() {
            Some(Up::Folder(p)) => self.enter(p, here.as_deref()).map(Some),
            Some(Up::Drives) => {
                self.show(LocalDir::drives(), here.as_deref());
                Ok(None)
            }
            None => Ok(None),
        }
    }
}

/// The selection in the file transfer view.
#[derive(Debug, Default)]
pub struct FilesView {
    focus: Focus,
    local: LocalView,
    selected: usize,
    scroll: usize,
    queue_selected: usize,
    queue_scroll: usize,
    /// The folder the selection belongs to: a new folder starts at the top.
    path: Vec<u8>,
}

impl FilesView {
    /// Starts over, for another host. The Local PC section stays where
    /// it is.
    pub fn reset(&mut self) {
        let local = std::mem::take(&mut self.local);
        *self = Self::default();
        self.local = local;
    }

    /// Fits the selections to the pane; `base` is the download folder.
    fn sync(&mut self, remote: &Remote, base: &Path, rows: usize) -> Geometry {
        self.local.sync(base, remote);
        if self.path != remote.listing.path {
            self.path.clone_from(&remote.listing.path);
            self.selected = 0;
            self.scroll = 0;
        }
        let queued = queue_rows(remote).len();
        let g = geometry(rows, queued, remote.question().is_some());
        let n = item_count(remote);
        self.selected = self.selected.min(n.saturating_sub(1));
        let visible = g.list.1.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
        let local = &mut self.local;
        local.selected = local.selected.min(local.count().saturating_sub(1));
        if let Some((_, _, ln)) = g.local {
            if local.selected < local.scroll {
                local.scroll = local.selected;
            }
            if local.selected >= local.scroll + ln {
                local.scroll = local.selected + 1 - ln;
            }
        } else if self.focus == Focus::Local {
            self.focus = Focus::List;
        }
        self.queue_selected = self.queue_selected.min(queued.saturating_sub(1));
        if let Some((_, _, qn)) = g.queue {
            if self.queue_selected < self.queue_scroll {
                self.queue_scroll = self.queue_selected;
            }
            if self.queue_selected >= self.queue_scroll + qn {
                self.queue_scroll = self.queue_selected + 1 - qn;
            }
        }
        if queued == 0 && self.focus == Focus::Queue {
            self.focus = Focus::List;
        }
        g
    }

    fn selected_job(&self, remote: &Remote) -> Option<JobId> {
        let i = *queue_rows(remote).get(self.queue_selected)?;
        remote.jobs().get(i).map(|j| j.id)
    }

    /// Opens the selected row: a folder is listed, a file downloaded.
    fn open(&mut self, remote: &mut Remote, downloads: &Path) {
        let path = remote.listing.path.clone();
        match item(remote, self.selected) {
            Some(Item::Up) => remote.browse(&names::parent(&path)),
            Some(Item::Entry(e)) if e.attrs.is_dir() || e.attrs.is_symlink() => {
                let target = names::join(&path, &e.name);
                remote.browse(&target);
            }
            Some(Item::Entry(_)) => self.download(remote, downloads),
            None => {}
        }
    }

    fn download(&mut self, remote: &mut Remote, downloads: &Path) {
        let Some(Item::Entry(entry)) = item(remote, self.selected) else {
            return;
        };
        let entry = entry.clone();
        if let Err(e) = std::fs::create_dir_all(downloads) {
            remote.notice = Some(format!("{}: {e}", downloads.display()));
            return;
        }
        if let Err(e) = remote.download(&entry, downloads) {
            remote.notice = Some(e);
        }
    }

    /// Handles a key; `rows` is the grid's height and `downloads` where
    /// downloads go.
    pub fn key(
        &mut self,
        remote: Option<&mut Remote>,
        key: SideKey,
        downloads: &Path,
        rows: usize,
    ) -> Option<FilesAction> {
        let Some(remote) = remote else {
            return match key {
                SideKey::Escape => Some(FilesAction::Leave),
                SideKey::Char('f' | 'F') => Some(FilesAction::ToggleFollow),
                SideKey::Char('o') => Some(FilesAction::OpenDownloads),
                _ => None,
            };
        };
        remote.notice = None;
        let g = self.sync(remote, downloads, rows);
        // The question takes its answer from either half.
        if let (Some((id, _)), SideKey::Char(c)) = (remote.question(), key) {
            let choice = match c.to_ascii_lowercase() {
                'o' => Some(Choice::Overwrite),
                's' => Some(Choice::Skip),
                'k' => Some(Choice::KeepBoth),
                _ => None,
            };
            if let Some(choice) = choice {
                remote.answer(id, choice, c.is_ascii_uppercase());
                self.sync(remote, downloads, rows);
                return None;
            }
        }
        let action = match self.focus {
            Focus::List => self.list_key(remote, key, downloads, g),
            Focus::Local => self.local_key(remote, key, g),
            Focus::Queue => {
                let n = g.queue.map_or(1, |q| q.2);
                self.queue_key(remote, key, n)
            }
        };
        self.sync(remote, downloads, rows);
        action
    }

    fn list_key(
        &mut self,
        remote: &mut Remote,
        key: SideKey,
        downloads: &Path,
        g: Geometry,
    ) -> Option<FilesAction> {
        let n = item_count(remote);
        let page = g.list.1.saturating_sub(1).max(1);
        match key {
            SideKey::Up => self.selected = self.selected.saturating_sub(1),
            SideKey::Down => self.selected += 1,
            SideKey::PageUp => self.selected = self.selected.saturating_sub(page),
            SideKey::PageDown => self.selected += page,
            SideKey::Home => self.selected = 0,
            SideKey::End => self.selected = n.saturating_sub(1),
            SideKey::Enter { .. } | SideKey::Right => self.open(remote, downloads),
            SideKey::Left | SideKey::Backspace => {
                if has_up(remote) {
                    let up = names::parent(&remote.listing.path);
                    remote.browse(&up);
                }
            }
            SideKey::Refresh => remote.refresh(),
            SideKey::Delete => {
                if let Some(Item::Entry(e)) = item(remote, self.selected) {
                    return Some(FilesAction::Delete(e.clone()));
                }
            }
            SideKey::Tab => {
                if g.local.is_some() {
                    self.focus = Focus::Local;
                    self.local.reload();
                } else if g.queue.is_some() {
                    self.focus = Focus::Queue;
                }
            }
            SideKey::Escape => return Some(FilesAction::Leave),
            SideKey::Char(c) => match c.to_ascii_lowercase() {
                'd' => self.download(remote, downloads),
                'b' => return Some(FilesAction::PickLocal),
                'u' => return Some(FilesAction::PickUpload(remote.listing.path.clone())),
                'f' => return Some(FilesAction::ToggleFollow),
                'o' => return Some(FilesAction::OpenDownloads),
                '~' | 'h' => {
                    if let Some(home) = remote.home.clone() {
                        remote.browse(&home);
                    }
                }
                _ => {}
            },
            SideKey::ToggleFavourite | SideKey::Edit | SideKey::New => {}
        }
        None
    }

    fn local_key(&mut self, remote: &mut Remote, key: SideKey, g: Geometry) -> Option<FilesAction> {
        let n = self.local.count();
        let page = g.local.map_or(1, |l| l.2).saturating_sub(1).max(1);
        let local = &mut self.local;
        match key {
            SideKey::Up => local.selected = local.selected.saturating_sub(1),
            SideKey::Down => local.selected += 1,
            SideKey::PageUp => local.selected = local.selected.saturating_sub(page),
            SideKey::PageDown => local.selected += page,
            SideKey::Home => local.selected = 0,
            SideKey::End => local.selected = n.saturating_sub(1),
            _ => return self.local_command(remote, key, g),
        }
        None
    }

    /// The Local PC keys that do more than move the selection.
    fn local_command(
        &mut self,
        remote: &mut Remote,
        key: SideKey,
        g: Geometry,
    ) -> Option<FilesAction> {
        let done = match key {
            SideKey::Enter { .. } | SideKey::Right => return self.local_open(remote),
            SideKey::Left | SideKey::Backspace => self.local.up(),
            SideKey::Refresh => {
                self.local.reload();
                Ok(None)
            }
            SideKey::Tab => {
                self.focus = if g.queue.is_some() {
                    Focus::Queue
                } else {
                    Focus::List
                };
                Ok(None)
            }
            SideKey::Escape => Ok(Some(FilesAction::Leave)),
            SideKey::Char(c) => match c.to_ascii_lowercase() {
                'u' => return self.local_upload(remote),
                'b' => Ok(Some(FilesAction::PickLocal)),
                'o' => Ok(Some(FilesAction::OpenDownloads)),
                'f' => Ok(Some(FilesAction::ToggleFollow)),
                _ => Ok(None),
            },
            _ => Ok(None),
        };
        done.unwrap_or_else(|e| {
            remote.notice = Some(e);
            None
        })
    }

    /// Opens the selected Local PC row: a folder or drive is listed, a
    /// file uploaded into the listed remote folder.
    fn local_open(&mut self, remote: &mut Remote) -> Option<FilesAction> {
        let done = match self.local.item(self.local.selected) {
            Some(LocalItem::Up) => self.local.up(),
            Some(LocalItem::Entry(e)) if e.dir => {
                let dir = e.path.clone();
                self.local.enter(dir, None).map(Some)
            }
            Some(LocalItem::Entry(_)) => return self.local_upload(remote),
            None => Ok(None),
        };
        done.unwrap_or_else(|e| {
            remote.notice = Some(e);
            None
        })
    }

    /// Uploads the selected Local PC file or folder into the listed remote
    /// folder.
    fn local_upload(&mut self, remote: &mut Remote) -> Option<FilesAction> {
        // Not a whole drive.
        self.local.listing.dir.as_ref()?;
        let Some(LocalItem::Entry(e)) = self.local.item(self.local.selected) else {
            return None;
        };
        if remote.listing.path.is_empty() {
            remote.notice = Some("Wait for the remote folder to be listed.".into());
            return None;
        }
        Some(FilesAction::Upload {
            paths: vec![e.path.clone()],
            dir: remote.listing.path.clone(),
        })
    }

    fn queue_key(
        &mut self,
        remote: &mut Remote,
        key: SideKey,
        visible: usize,
    ) -> Option<FilesAction> {
        let n = queue_rows(remote).len();
        let job = self.selected_job(remote);
        let state = job
            .and_then(|id| remote.jobs().iter().find(|j| j.id == id))
            .map(|j| j.state);
        let page = visible.saturating_sub(1).max(1);
        match key {
            SideKey::Up => self.queue_selected = self.queue_selected.saturating_sub(1),
            SideKey::Down => self.queue_selected += 1,
            SideKey::PageUp => self.queue_selected = self.queue_selected.saturating_sub(page),
            SideKey::PageDown => self.queue_selected += page,
            SideKey::Home => self.queue_selected = 0,
            SideKey::End => self.queue_selected = n.saturating_sub(1),
            SideKey::Tab | SideKey::Left => self.focus = Focus::List,
            SideKey::Escape => return Some(FilesAction::Leave),
            SideKey::Delete => {
                if let Some(id) = job {
                    remote.cancel(id);
                }
            }
            SideKey::Char(c) => match (c.to_ascii_lowercase(), job, state) {
                (' ' | 'p', Some(id), Some(State::Paused)) => remote.resume(id),
                (' ' | 'p', Some(id), Some(s)) if !s.finished() => remote.pause(id),
                ('x', Some(id), _) => remote.cancel(id),
                ('r', Some(id), Some(State::Failed | State::Cancelled)) => remote.retry(id),
                ('c', _, _) => remote.clear_finished(),
                _ => {}
            },
            _ => {}
        }
        None
    }

    /// A click on grid cell (`x`, `y`); `double` opens.
    pub fn click(
        &mut self,
        remote: Option<&mut Remote>,
        x: usize,
        y: usize,
        double: bool,
        downloads: &Path,
        rows: usize,
    ) -> Option<FilesAction> {
        let remote = remote?;
        let g = self.sync(remote, downloads, rows);
        if let Some(q) = g.question
            && y == q + 1
            && let Some((id, _)) = remote.question()
        {
            // The options row: which word was hit.
            let choice = if x < 13 {
                Choice::Overwrite
            } else if x < 21 {
                Choice::Skip
            } else {
                Choice::KeepBoth
            };
            remote.answer(id, choice, false);
            self.sync(remote, downloads, rows);
            return None;
        }
        if let Some((_, first, n)) = g.queue
            && (first..first + n).contains(&y)
        {
            self.focus = Focus::Queue;
            self.queue_selected = self.queue_scroll + (y - first);
            self.sync(remote, downloads, rows);
            return None;
        }
        if let Some((title, first, n)) = g.local {
            if y == title {
                // The section's title: choose another folder.
                return Some(FilesAction::PickLocal);
            }
            if (first..first + n).contains(&y) {
                let i = self.local.scroll + (y - first);
                if i >= self.local.count() {
                    return None;
                }
                self.focus = Focus::Local;
                self.local.selected = i;
                let action = if double {
                    self.local_open(remote)
                } else {
                    None
                };
                self.sync(remote, downloads, rows);
                return action;
            }
        }
        let (first, n) = g.list;
        if !(first..first + n).contains(&y) {
            return None;
        }
        let i = self.scroll + (y - first);
        if i >= item_count(remote) {
            return None;
        }
        self.focus = Focus::List;
        self.selected = i;
        if double {
            self.open(remote, downloads);
        }
        self.sync(remote, downloads, rows);
        None
    }

    /// Wheel scrolling by `lines` (positive: up), of the Local PC section
    /// when it has the keyboard, else of the listing.
    pub fn scroll_by(
        &mut self,
        remote: Option<&Remote>,
        lines: i32,
        downloads: &Path,
        rows: usize,
    ) {
        let Some(remote) = remote else {
            return;
        };
        let g = self.sync(remote, downloads, rows);
        if self.focus == Focus::Local
            && let Some((_, _, n)) = g.local
        {
            let local = &mut self.local;
            let max = local.count().saturating_sub(n);
            local.scroll = if lines > 0 {
                local.scroll.saturating_sub(lines as usize)
            } else {
                (local.scroll + lines.unsigned_abs() as usize).min(max)
            };
            local.selected = local
                .selected
                .clamp(local.scroll, local.scroll + n.saturating_sub(1));
            return;
        }
        let max = item_count(remote).saturating_sub(g.list.1.max(1));
        self.scroll = if lines > 0 {
            self.scroll.saturating_sub(lines as usize)
        } else {
            (self.scroll + lines.unsigned_abs() as usize).min(max)
        };
        // Keep the selection on screen so the next sync does not undo it.
        let visible = g.list.1.max(1);
        self.selected = self
            .selected
            .clamp(self.scroll, self.scroll + visible.saturating_sub(1));
    }

    /// Draws the pane. `follow` is whether the listing follows the
    /// terminal's directory; `downloads` is the download folder.
    pub fn draw(
        &mut self,
        grid: &mut Grid,
        palette: &Palette,
        focused: bool,
        remote: Option<&Remote>,
        follow: Follow,
        downloads: &Path,
    ) {
        let width = grid.cols();
        let rows = grid.rows();
        let bg = palette.strip;
        let accent = Rgb::new(0xe5, 0xb5, 0x3b);
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
        let blank = line("", palette.dim, bg, Flags::empty());
        let Some(remote) = remote else {
            grid.set_row(0, line(" Files", palette.text, bg, Flags::BOLD));
            grid.set_row(
                1,
                line(
                    " Connect a pane to an SSH host to see its files.",
                    palette.dim,
                    palette.active,
                    Flags::empty(),
                ),
            );
            for y in 2..rows {
                grid.set_row(y, blank.clone());
            }
            return;
        };
        let g = self.sync(remote, downloads, rows);
        let follows = match follow {
            Follow::Off => "",
            Follow::On => "follows cd ",
            Follow::Unreported => "cd not reported ",
        };
        let title = spread(&format!(" Files  {}", remote.label), follows, width);
        let mut cells = line(&title, palette.text, bg, Flags::empty());
        for c in cells.iter_mut().take(6) {
            c.flags.insert(Flags::BOLD);
        }
        for c in cells.iter_mut().skip(width.saturating_sub(follows.len())) {
            c.fg = Color::Rgb(palette.dim);
        }
        grid.set_row(0, cells);
        let path = if remote.listing.path.is_empty() {
            if remote.is_ready() {
                " Listing…".to_owned()
            } else {
                " Opening SFTP…".to_owned()
            }
        } else {
            let loading = if remote.listing.loading { " …" } else { "" };
            format!(" {}{loading}", names::display(&remote.listing.path))
        };
        grid.set_row(1, line(&path, palette.text, palette.active, Flags::BOLD));
        // The listing.
        let (first, n) = g.list;
        let list_focus = focused && self.focus == Focus::List;
        for y in 0..n {
            let i = self.scroll + y;
            let selected = i == self.selected && list_focus;
            let row_bg = if selected { palette.active } else { bg };
            let flags = if selected {
                Flags::BOLD
            } else {
                Flags::empty()
            };
            let cells = match item(remote, i) {
                Some(Item::Up) => line(" ▴ ..", palette.dim, row_bg, flags),
                Some(Item::Entry(e)) => {
                    let name = names::display(&e.name);
                    if e.attrs.is_dir() {
                        line(&format!(" ▸ {name}/"), palette.text, row_bg, flags)
                    } else if e.attrs.is_symlink() {
                        line(&format!(" ↪ {name}"), palette.text, row_bg, flags)
                    } else {
                        let size = e.attrs.size.map(names::size).unwrap_or_default();
                        let text = spread(&format!("   {name}"), &format!("{size} "), width);
                        let mut cells = line(&text, palette.text, row_bg, flags);
                        let from = width.saturating_sub(size.chars().count() + 1);
                        for c in cells.iter_mut().skip(from) {
                            c.fg = Color::Rgb(palette.dim);
                        }
                        cells
                    }
                }
                None if i == 0 => {
                    let text = match &remote.listing.error {
                        Some(e) => format!(" {e}"),
                        None if remote.listing.loading || remote.listing.path.is_empty() => {
                            String::new()
                        }
                        None => " (empty)".into(),
                    };
                    line(&text, palette.dim, bg, Flags::empty())
                }
                None => blank.clone(),
            };
            grid.set_row(first + y, cells);
        }
        if let Some(e) = &remote.listing.error
            && item_count(remote) > 0
            && n > 0
        {
            // A failed refresh keeps the old listing; say so at its foot.
            grid.set_row(
                first + n - 1,
                line(&format!(" {e}"), accent, bg, Flags::empty()),
            );
        }
        // The Local PC section.
        if let Some((title_row, lfirst, ln)) = g.local {
            let label = self.local.listing.label();
            let room = width.saturating_sub(" Local PC  ".len() + 1);
            let text = spread(" Local PC", &format!("{} ", tail(&label, room)), width);
            let mut cells = line(&text, palette.dim, palette.active, Flags::empty());
            for c in cells.iter_mut().take(9) {
                c.flags.insert(Flags::BOLD);
            }
            grid.set_row(title_row, cells);
            let local_focus = focused && self.focus == Focus::Local;
            let sep = std::path::MAIN_SEPARATOR;
            for y in 0..ln {
                let i = self.local.scroll + y;
                let selected = i == self.local.selected && local_focus;
                let row_bg = if selected { palette.active } else { bg };
                let flags = if selected {
                    Flags::BOLD
                } else {
                    Flags::empty()
                };
                let cells = match self.local.item(i) {
                    Some(LocalItem::Up) => line(" ▴ ..", palette.dim, row_bg, flags),
                    Some(LocalItem::Entry(e)) if e.dir => {
                        let name = e.name.trim_end_matches(['\\', '/']);
                        line(&format!(" ▸ {name}{sep}"), palette.text, row_bg, flags)
                    }
                    Some(LocalItem::Entry(e)) => {
                        let size = names::size(e.size);
                        let text = spread(&format!("   {}", e.name), &format!("{size} "), width);
                        let mut cells = line(&text, palette.text, row_bg, flags);
                        let from = width.saturating_sub(size.chars().count() + 1);
                        for c in cells.iter_mut().skip(from) {
                            c.fg = Color::Rgb(palette.dim);
                        }
                        cells
                    }
                    None if i == 0 || (i == 1 && self.local.listing.up.is_some()) => {
                        let text = match &self.local.listing.error {
                            Some(e) => format!(" {e}"),
                            None => " (empty)".into(),
                        };
                        line(&text, palette.dim, bg, Flags::empty())
                    }
                    None => blank.clone(),
                };
                grid.set_row(lfirst + y, cells);
            }
        }
        // The queue.
        if let Some((title_row, qfirst, qn)) = g.queue {
            let jobs = remote.jobs();
            let shown = queue_rows(remote);
            let running = jobs.iter().filter(|j| j.state == State::Running).count();
            let waiting = jobs.iter().filter(|j| j.state == State::Waiting).count();
            let mut right = String::new();
            if running > 0 {
                right = format!("{running} moving ");
            } else if waiting > 0 {
                right = "waiting for connection ".into();
            }
            if remote.verify {
                right.push_str("· verify ");
            }
            let text = spread(" Transfers", &right, width);
            grid.set_row(
                title_row,
                line(&text, palette.dim, palette.active, Flags::BOLD),
            );
            let queue_focus = focused && self.focus == Focus::Queue;
            for y in 0..qn {
                let k = self.queue_scroll + y;
                let selected = k == self.queue_selected && queue_focus;
                let row_bg = if selected { palette.active } else { bg };
                let cells = match shown.get(k).and_then(|&i| jobs.get(i)) {
                    Some(job) => {
                        let arrow = match job.kind {
                            Kind::Download | Kind::FetchFolder => "↓",
                            Kind::Upload | Kind::SendFolder => "↑",
                        };
                        let indent = if job.parent.is_some() { "  " } else { "" };
                        let st = status(job);
                        let text = spread(
                            &format!(" {indent}{arrow} {}", job.name),
                            &format!("{st} "),
                            width,
                        );
                        let fg = match job.state {
                            State::Failed | State::Asking => accent,
                            State::Done | State::Cancelled | State::Skipped => palette.dim,
                            _ => palette.text,
                        };
                        line(
                            &text,
                            fg,
                            row_bg,
                            if selected {
                                Flags::BOLD
                            } else {
                                Flags::empty()
                            },
                        )
                    }
                    None => blank.clone(),
                };
                grid.set_row(qfirst + y, cells);
            }
        }
        if let Some(q) = g.question {
            let text = remote.question().map(|(_, t)| t).unwrap_or_default();
            grid.set_row(
                q,
                line(&format!(" {text}"), accent, palette.active, Flags::BOLD),
            );
            grid.set_row(
                q + 1,
                line(
                    &format!("{OPTIONS}  (Shift: all)"),
                    palette.text,
                    palette.active,
                    Flags::empty(),
                ),
            );
        }
        // The footer: a notice, else what the keys do.
        let (text, fg) = if let Some(n) = &remote.notice {
            (format!(" {n}"), accent)
        } else if focused && self.focus == Focus::Queue {
            (
                " Space pause · R retry · X cancel · C clear · Tab files".into(),
                palette.dim,
            )
        } else if focused && self.focus == Focus::Local {
            let next = if g.queue.is_some() { "queue" } else { "files" };
            (
                format!(" Enter open · U upload · B choose folder · O Explorer · Tab {next}"),
                palette.dim,
            )
        } else if focused {
            let next = if g.local.is_some() {
                "Local PC"
            } else {
                "queue"
            };
            (
                format!(" Enter open · D download · U upload · Del delete · Tab {next}"),
                palette.dim,
            )
        } else {
            (" Ctrl+Shift+E to browse".into(), palette.dim)
        };
        if g.footer >= HEADER {
            grid.set_row(g.footer, line(&text, fg, bg, Flags::empty()));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_ipc::{FileAttrs, Message, SftpOp, SftpReply};
    use ferossh_render::Theme;

    fn entry(name: &str, dir: bool, size: u64) -> DirEntry {
        DirEntry {
            name: name.as_bytes().to_vec(),
            attrs: FileAttrs {
                size: Some(size),
                permissions: Some(if dir { 0o040755 } else { 0o100644 }),
                mtime: None,
            },
        }
    }

    /// A remote that is open and lists /home/me.
    fn remote() -> Remote {
        let mut r = Remote::new("me@host:22");
        r.attach(7, 1);
        r.on_message(Message::SftpReady {
            channel: 7,
            home: b"/home/me".to_vec(),
        });
        let list = r
            .take_outbox()
            .into_iter()
            .find_map(|m| match m {
                Message::SftpRequest {
                    id,
                    op: SftpOp::List { .. },
                    ..
                } => Some(id),
                _ => None,
            })
            .unwrap();
        r.on_message(Message::SftpReply {
            channel: 7,
            id: list,
            reply: SftpReply::Entries {
                entries: vec![entry("notes.txt", false, 1500), entry("src", true, 0)],
                last: true,
            },
        });
        r
    }

    fn texts(v: &mut FilesView, r: Option<&Remote>, rows: usize) -> Vec<String> {
        texts_in(v, r, rows, Path::new("dl"))
    }

    fn texts_in(v: &mut FilesView, r: Option<&Remote>, rows: usize, local: &Path) -> Vec<String> {
        let mut grid = Grid::new(60, rows);
        v.draw(
            &mut grid,
            &Palette::of(&Theme::default()),
            true,
            r,
            Follow::On,
            local,
        );
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
    fn lists_folders_first() {
        let mut v = FilesView::default();
        let r = remote();
        let t = texts(&mut v, Some(&r), 10);
        assert!(t[0].starts_with(" Files  me@host:22"), "{:?}", t[0]);
        assert!(t[0].ends_with("follows cd"));
        assert_eq!(t[1], " /home/me");
        assert_eq!(t[2], " ▴ ..");
        assert_eq!(t[3], " ▸ src/");
        assert!(
            t[4].starts_with("   notes.txt") && t[4].ends_with("1.5 KB"),
            "{:?}",
            t[4]
        );
        assert!(t[9].contains("Enter open"));
    }

    #[test]
    fn without_a_host() {
        let mut v = FilesView::default();
        let t = texts(&mut v, None, 5);
        assert!(t[1].contains("Connect a pane"));
        let d = Path::new("dl");
        assert_eq!(v.key(None, SideKey::Escape, d, 5), Some(FilesAction::Leave));
    }

    #[test]
    fn keys_browse_and_ask() {
        let mut v = FilesView::default();
        let mut r = remote();
        let d = Path::new("dl");
        // Down to src and in.
        v.key(Some(&mut r), SideKey::Down, d, 10);
        v.key(Some(&mut r), SideKey::Enter { split: false }, d, 10);
        let asked = r.take_outbox();
        assert!(
            asked.iter().any(|m| matches!(m, Message::SftpRequest {
                op: SftpOp::List { path }, ..
            } if path == b"/home/me/src")),
            "{asked:?}"
        );
        // Backspace goes up.
        v.key(Some(&mut r), SideKey::Backspace, d, 10);
        assert!(
            r.take_outbox()
                .iter()
                .any(|m| matches!(m, Message::SftpRequest {
            op: SftpOp::List { path }, ..
        } if path == b"/home"))
        );
        // Del on a file asks the window first.
        v.key(Some(&mut r), SideKey::Down, d, 10);
        assert_eq!(
            v.key(Some(&mut r), SideKey::Delete, d, 10),
            Some(FilesAction::Delete(entry("notes.txt", false, 1500)))
        );
        assert_eq!(
            v.key(Some(&mut r), SideKey::Char('u'), d, 10),
            Some(FilesAction::PickUpload(b"/home/me".to_vec()))
        );
        assert_eq!(
            v.key(Some(&mut r), SideKey::Escape, d, 10),
            Some(FilesAction::Leave)
        );
    }

    #[test]
    fn queue_shows_and_controls_jobs() {
        let dir = std::env::temp_dir().join(format!("ferossh-files-{}", std::process::id()));
        let mut v = FilesView::default();
        let mut r = remote();
        // Select notes.txt and download it.
        v.key(Some(&mut r), SideKey::End, &dir, 12);
        v.key(Some(&mut r), SideKey::Char('d'), &dir, 12);
        assert_eq!(r.jobs().len(), 1);
        let t = texts(&mut v, Some(&r), 12);
        let title = t.iter().position(|l| l.starts_with(" Transfers")).unwrap();
        assert!(t[title + 1].starts_with(" ↓ notes.txt"), "{t:?}");
        // Past Local PC into the queue; pause and resume.
        v.key(Some(&mut r), SideKey::Tab, &dir, 12);
        v.key(Some(&mut r), SideKey::Tab, &dir, 12);
        v.key(Some(&mut r), SideKey::Char(' '), &dir, 12);
        assert_eq!(r.jobs()[0].state, State::Paused);
        let t = texts(&mut v, Some(&r), 12);
        assert!(t[title + 1].ends_with("paused 0%"), "{:?}", t[title + 1]);
        assert!(t[11].contains("Space pause"));
        v.key(Some(&mut r), SideKey::Char('p'), &dir, 12);
        assert_ne!(r.jobs()[0].state, State::Paused);
        v.key(Some(&mut r), SideKey::Char('x'), &dir, 12);
        assert_eq!(r.jobs()[0].state, State::Cancelled);
        v.key(Some(&mut r), SideKey::Char('c'), &dir, 12);
        assert!(r.jobs().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clicks_select_and_open() {
        let mut v = FilesView::default();
        let mut r = remote();
        let d = Path::new("dl");
        assert_eq!(v.click(Some(&mut r), 3, 3, false, d, 10), None);
        assert_eq!(v.selected, 1);
        r.take_outbox();
        v.click(Some(&mut r), 3, 3, true, d, 10);
        assert!(
            r.take_outbox()
                .iter()
                .any(|m| matches!(m, Message::SftpRequest {
            op: SftpOp::List { path }, ..
        } if path == b"/home/me/src"))
        );
    }

    #[test]
    fn geometry_fits() {
        let g = geometry(20, 3, true);
        assert_eq!(g.footer, 19);
        assert_eq!(g.question, Some(17));
        assert_eq!(g.queue, Some((13, 14, 3)));
        assert_eq!(g.local, Some((7, 8, 5)));
        assert_eq!(g.list, (2, 5));
        // A tall pane gives Local PC a quarter, up to eight rows.
        let g = geometry(60, 0, false);
        assert_eq!(g.local, Some((50, 51, 8)));
        assert_eq!(g.list, (2, 48));
        // Too small for the queue and Local PC: the listing keeps what
        // there is.
        let g = geometry(4, 3, true);
        assert_eq!(g.queue, None);
        assert_eq!(g.local, None);
        assert_eq!(g.list, (2, 1));
    }

    /// A local folder with a subfolder and a file.
    fn local_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ferossh-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        std::fs::write(dir.join("report.pdf"), vec![0u8; 2048]).unwrap();
        dir
    }

    #[test]
    fn local_pc_sits_between_files_and_transfers() {
        let dir = local_dir("between");
        let mut v = FilesView::default();
        let mut r = remote();
        // Download notes.txt so the queue shows.
        v.key(Some(&mut r), SideKey::End, &dir, 20);
        v.key(Some(&mut r), SideKey::Char('d'), &dir, 20);
        let t = texts_in(&mut v, Some(&r), 20, &dir);
        let files = t
            .iter()
            .position(|l| l.starts_with("   notes.txt"))
            .unwrap();
        let local = t.iter().position(|l| l.starts_with(" Local PC")).unwrap();
        let transfers = t.iter().position(|l| l.starts_with(" Transfers")).unwrap();
        assert!(files < local && local < transfers, "{t:?}");
        assert!(
            t[local].ends_with(&format!("{}", dir.display())),
            "{:?}",
            t[local]
        );
        assert_eq!(t[local + 1], " ▴ ..");
        assert_eq!(
            t[local + 2],
            format!(" ▸ photos{}", std::path::MAIN_SEPARATOR)
        );
        assert!(t[local + 3].starts_with("   report.pdf") && t[local + 3].ends_with("2.0 KB"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_pc_browses_and_uploads() {
        let dir = local_dir("browse");
        let mut v = FilesView::default();
        let mut r = remote();
        texts_in(&mut v, Some(&r), 20, &dir);
        // Tab moves to Local PC; Down to photos; Enter goes in.
        v.key(Some(&mut r), SideKey::Tab, &dir, 20);
        assert_eq!(v.focus, Focus::Local);
        let t = texts_in(&mut v, Some(&r), 20, &dir);
        assert!(t[19].contains("B choose folder"), "{:?}", t[19]);
        v.key(Some(&mut r), SideKey::Down, &dir, 20);
        assert_eq!(
            v.key(Some(&mut r), SideKey::Enter { split: false }, &dir, 20),
            Some(FilesAction::SetLocal(dir.join("photos")))
        );
        let photos = dir.join("photos");
        let t = texts_in(&mut v, Some(&r), 20, &photos);
        let local = t.iter().position(|l| l.starts_with(" Local PC")).unwrap();
        assert!(t[local].ends_with("photos"), "{:?}", t[local]);
        assert_eq!(t[local + 2], " (empty)");
        // Backspace goes back up, with the folder we came from selected.
        assert_eq!(
            v.key(Some(&mut r), SideKey::Backspace, &photos, 20),
            Some(FilesAction::SetLocal(dir.clone()))
        );
        assert_eq!(v.local.selected, 1);
        // U (or Enter on a file) uploads into the listed remote folder.
        v.key(Some(&mut r), SideKey::Down, &dir, 20);
        let up = Some(FilesAction::Upload {
            paths: vec![dir.join("report.pdf")],
            dir: b"/home/me".to_vec(),
        });
        assert_eq!(v.key(Some(&mut r), SideKey::Char('u'), &dir, 20), up);
        assert_eq!(
            v.key(Some(&mut r), SideKey::Enter { split: false }, &dir, 20),
            up
        );
        assert_eq!(
            v.key(Some(&mut r), SideKey::Char('b'), &dir, 20),
            Some(FilesAction::PickLocal)
        );
        // Tab with no queue goes back to the files.
        v.key(Some(&mut r), SideKey::Tab, &dir, 20);
        assert_eq!(v.focus, Focus::List);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_pc_clicks_and_follows_the_download_folder() {
        let dir = local_dir("click");
        let mut v = FilesView::default();
        let mut r = remote();
        let t = texts_in(&mut v, Some(&r), 20, &dir);
        let local = t.iter().position(|l| l.starts_with(" Local PC")).unwrap();
        // The title picks another folder; a row selects; a double click
        // on a folder opens it.
        assert_eq!(
            v.click(Some(&mut r), 3, local, false, &dir, 20),
            Some(FilesAction::PickLocal)
        );
        assert_eq!(v.click(Some(&mut r), 3, local + 3, false, &dir, 20), None);
        assert_eq!(v.focus, Focus::Local);
        assert_eq!(v.local.selected, 2);
        assert_eq!(
            v.click(Some(&mut r), 3, local + 2, true, &dir, 20),
            Some(FilesAction::SetLocal(dir.join("photos")))
        );
        // A new download folder (the picker, Settings) is listed at once,
        // and another host keeps it.
        v.reset();
        let t = texts_in(&mut v, Some(&r), 20, &dir.join("photos"));
        assert!(t[local].ends_with("photos"), "{:?}", t[local]);
        let t = texts_in(&mut v, Some(&r), 20, &dir);
        assert_eq!(
            t[local + 2],
            format!(" ▸ photos{}", std::path::MAIN_SEPARATOR)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("C:\\Users\\me\\Downloads", 11), "…\\Downloads");
        assert_eq!(tail("short", 12), "short");
        assert_eq!(tail("abc", 0), "");
    }
}
