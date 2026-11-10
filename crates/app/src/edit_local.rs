//! Editing a server file in a local editor: File transfer's "Edit
//! locally" downloads the file into a folder of its own under the temp
//! folder, opens it in an editor, and puts it back on the server each
//! time the editor saves it. Here is the bookkeeping, apart from Windows:
//! which downloads to wait for, which files to watch, and when a save has
//! settled enough to send.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ferossh_sftp::{JobId, State};

/// What a file looks like on disk: changed when either part changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub modified: SystemTime,
    pub len: u64,
}

impl Stamp {
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: meta.modified().ok()?,
            len: meta.len(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    /// Coming down in job `JobId`.
    Fetching(JobId),
    /// In the editor; `sent` is what the server has.
    Open { sent: Option<Stamp> },
    /// Seen changed: sent once it stays this way for a tick, so a save
    /// that comes in pieces goes up whole.
    Settling { sent: Option<Stamp>, seen: Stamp },
    /// Going up in job `JobId`, as it was at `sending`.
    Sending { job: JobId, sending: Stamp },
}

#[derive(Debug)]
struct Edit {
    /// The transfer queue (host) the file belongs to.
    queue: String,
    remote: Vec<u8>,
    local: PathBuf,
    /// The server copy's mode, which the copies put back keep.
    mode: Option<u32>,
    phase: Phase,
}

/// What the window does for the edits.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// The download is in: open it in an editor.
    Open(PathBuf),
    /// Saved: put `local` back over `remote` on queue `queue`, then say
    /// which job does it with [`Edits::sending`].
    PutBack {
        queue: String,
        local: PathBuf,
        remote: Vec<u8>,
        mode: Option<u32>,
    },
    /// Something to show in the File transfer pane of queue `queue`.
    Notice { queue: String, text: String },
}

/// The files being edited, across hosts.
#[derive(Debug, Default)]
pub struct Edits {
    list: Vec<Edit>,
}

/// How often the window calls [`Edits::tick`].
pub const TICK: Duration = Duration::from_secs(1);
/// Edit folders older than this are removed when a new edit starts.
pub const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Watches download `job` of `remote` (on queue `queue`) into `local`.
    /// An edit of the same file already under way is replaced.
    pub fn start(
        &mut self,
        queue: &str,
        remote: &[u8],
        local: PathBuf,
        job: JobId,
        mode: Option<u32>,
    ) {
        self.list
            .retain(|e| !(e.queue == queue && e.remote == remote));
        self.list.push(Edit {
            queue: queue.to_owned(),
            remote: remote.to_vec(),
            local,
            mode,
            phase: Phase::Fetching(job),
        });
    }

    /// The put-back of the edit of `local` runs as job `job`.
    pub fn sending(&mut self, local: &Path, job: JobId, sending: Stamp) {
        if let Some(e) = self.list.iter_mut().find(|e| e.local == local) {
            e.phase = Phase::Sending { job, sending };
        }
    }

    /// Stops watching the edits of queues `live` does not hold.
    pub fn retain(&mut self, live: impl Fn(&str) -> bool) {
        self.list.retain(|e| live(&e.queue));
    }

    /// Moves every edit on: `job` gives a job's state on a queue (`None`
    /// when the queue or job is gone), `stamp` a local file's.
    pub fn tick(
        &mut self,
        job: impl Fn(&str, JobId) -> Option<State>,
        stamp: impl Fn(&Path) -> Option<Stamp>,
    ) -> Vec<Action> {
        let mut out = Vec::new();
        let name =
            |e: &Edit| ferossh_sftp::names::display(ferossh_sftp::names::file_name(&e.remote));
        self.list.retain_mut(|e| {
            match e.phase {
                Phase::Fetching(id) => match job(&e.queue, id) {
                    Some(State::Done) => {
                        let now = stamp(&e.local);
                        if now.is_none() {
                            return false;
                        }
                        e.phase = Phase::Open { sent: now };
                        out.push(Action::Open(e.local.clone()));
                        out.push(Action::Notice {
                            queue: e.queue.clone(),
                            text: format!(
                                "Editing {} locally: each save goes back to the server.",
                                name(e)
                            ),
                        });
                    }
                    Some(s) if !s.finished() => {}
                    // Failed, cancelled, skipped or gone: the queue says why.
                    _ => return false,
                },
                Phase::Open { sent } => {
                    let Some(now) = stamp(&e.local) else {
                        return false;
                    };
                    if Some(now) != sent {
                        e.phase = Phase::Settling { sent, seen: now };
                    }
                }
                Phase::Settling { sent, seen } => {
                    let Some(now) = stamp(&e.local) else {
                        return false;
                    };
                    if now == seen {
                        out.push(Action::PutBack {
                            queue: e.queue.clone(),
                            local: e.local.clone(),
                            remote: e.remote.clone(),
                            mode: e.mode,
                        });
                        // Until the window says which job: stays settled
                        // as sent, so a failure to start is retried.
                        e.phase = Phase::Open { sent: Some(now) };
                    } else {
                        e.phase = Phase::Settling { sent, seen: now };
                    }
                }
                Phase::Sending { job: id, sending } => match job(&e.queue, id) {
                    Some(State::Done) => {
                        e.phase = Phase::Open {
                            sent: Some(sending),
                        };
                        out.push(Action::Notice {
                            queue: e.queue.clone(),
                            text: format!("Saved {} to the server.", name(e)),
                        });
                    }
                    Some(s) if !s.finished() => {}
                    Some(_) => {
                        // It failed or was cancelled: the queue shows why,
                        // and the next save tries again.
                        e.phase = Phase::Open {
                            sent: Some(sending),
                        };
                        out.push(Action::Notice {
                            queue: e.queue.clone(),
                            text: format!(
                                "{} was not saved to the server: save it again to retry.",
                                name(e)
                            ),
                        });
                    }
                    None => return false,
                },
            }
            true
        });
        out
    }
}

/// A fresh folder for one edit under `root`, and the edit folders there
/// older than [`KEEP`] removed.
pub fn edit_folder(root: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    let now = SystemTime::now();
    if let Ok(rd) = std::fs::read_dir(root) {
        for entry in rd.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > KEEP);
            if old && entry.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let stamp = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    for n in 0..1000u32 {
        let dir = root.join(format!("{stamp}-{n}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free edit folder"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    impl Edits {
        /// The local copy of `remote` on `queue` being edited, if any.
        pub fn local_of(&self, queue: &str, remote: &[u8]) -> Option<&Path> {
            self.list
                .iter()
                .find(|e| e.queue == queue && e.remote == remote)
                .map(|e| e.local.as_path())
        }
    }

    use std::cell::RefCell;
    use std::collections::HashMap;

    fn at(secs: u64, len: u64) -> Stamp {
        Stamp {
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            len,
        }
    }

    #[test]
    fn opens_after_download_and_sends_settled_saves() {
        let jobs: RefCell<HashMap<JobId, State>> = RefCell::new(HashMap::new());
        let disk: RefCell<Option<Stamp>> = RefCell::new(None);
        let mut edits = Edits::default();
        let local = PathBuf::from("/tmp/e/1/a.conf");
        jobs.borrow_mut().insert(1, State::Running);
        edits.start("q", b"/etc/a.conf", local.clone(), 1, Some(0o100_640));
        let tick = |edits: &mut Edits| {
            edits.tick(
                |q, id| {
                    assert_eq!(q, "q");
                    jobs.borrow().get(&id).copied()
                },
                |_| *disk.borrow(),
            )
        };
        assert!(tick(&mut edits).is_empty());
        jobs.borrow_mut().insert(1, State::Done);
        *disk.borrow_mut() = Some(at(10, 5));
        let out = tick(&mut edits);
        assert_eq!(out[0], Action::Open(local.clone()));
        assert!(matches!(&out[1], Action::Notice { text, .. } if text.contains("a.conf")));
        assert!(tick(&mut edits).is_empty(), "nothing saved yet");

        // A save in two writes: sent once it holds still for a tick.
        *disk.borrow_mut() = Some(at(20, 3));
        assert!(tick(&mut edits).is_empty());
        *disk.borrow_mut() = Some(at(20, 9));
        assert!(tick(&mut edits).is_empty());
        let out = tick(&mut edits);
        assert_eq!(
            out,
            [Action::PutBack {
                queue: "q".into(),
                local: local.clone(),
                remote: b"/etc/a.conf".to_vec(),
                mode: Some(0o100_640),
            }]
        );
        jobs.borrow_mut().insert(2, State::Running);
        edits.sending(&local, 2, at(20, 9));
        // Saved again while it goes up: sent again afterwards.
        *disk.borrow_mut() = Some(at(30, 9));
        assert!(tick(&mut edits).is_empty());
        jobs.borrow_mut().insert(2, State::Done);
        let out = tick(&mut edits);
        assert!(matches!(&out[0], Action::Notice { text, .. } if text.starts_with("Saved")));
        assert!(tick(&mut edits).is_empty(), "the newer save settles");
        assert!(matches!(tick(&mut edits)[0], Action::PutBack { .. }));

        // A failed put-back is retried at the next save, not before.
        jobs.borrow_mut().insert(3, State::Failed);
        edits.sending(&local, 3, at(30, 9));
        let out = tick(&mut edits);
        assert!(matches!(&out[0], Action::Notice { text, .. } if text.contains("not saved")));
        assert!(tick(&mut edits).is_empty());
        *disk.borrow_mut() = Some(at(40, 9));
        assert!(tick(&mut edits).is_empty());
        assert!(matches!(tick(&mut edits)[0], Action::PutBack { .. }));

        // The local file gone: no longer watched.
        *disk.borrow_mut() = None;
        assert!(tick(&mut edits).is_empty());
        assert!(edits.is_empty());
    }

    #[test]
    fn a_failed_download_or_a_closed_host_ends_the_edit() {
        let mut edits = Edits::default();
        edits.start("q", b"/a", PathBuf::from("/x/a"), 1, None);
        edits.start("r", b"/b", PathBuf::from("/x/b"), 1, None);
        assert_eq!(edits.local_of("r", b"/b"), Some(Path::new("/x/b")));
        let out = edits.tick(
            |q, _| {
                Some(if q == "q" {
                    State::Failed
                } else {
                    State::Running
                })
            },
            |_| Some(at(1, 1)),
        );
        assert!(out.is_empty());
        assert_eq!(edits.local_of("q", b"/a"), None);
        assert!(!edits.is_empty());
        edits.retain(|q| q != "r");
        assert!(edits.is_empty());
    }

    #[test]
    fn makes_a_fresh_folder_and_clears_old_ones() {
        let root = std::env::temp_dir().join(format!("ferossh-edit-test-{}", std::process::id()));
        let a = edit_folder(&root).unwrap();
        let b = edit_folder(&root).unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
