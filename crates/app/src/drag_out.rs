//! Files dragged from the Remote list out to Explorer, the desktop or any
//! program that takes dropped files.
//!
//! Explorer copies files that exist, and these are on the server. So the
//! drag offers files in a fresh folder under %TEMP%: empty stand-ins
//! while the mouse moves, which give Explorer the names, icons and "Copy
//! to" it shows without anything read from the server. When the button
//! is let go over a place that takes them, the stand-ins go, the transfer
//! queue downloads the real files in their place (with the Mark-of-the-Web
//! every download gets), and only once they are all complete is the drop
//! let through: the target then copies finished files, and Explorer's copy
//! keeps the mark. Escape while they download cancels both.
//!
//! The window does the drag and the files on this PC; the downloads run
//! through the connection's sandboxed worker like any other. The folders
//! stay a day (a program the files were dropped on may open them from
//! there) and are removed when a later drag starts.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use ferossh_sftp::{JobId, State, names};

/// How long a drag's folder is kept.
pub const KEEP: Duration = Duration::from_secs(24 * 3600);

/// A fresh folder for one drag, older ones removed.
pub fn folder() -> std::io::Result<PathBuf> {
    crate::edit_local::fresh_folder(&std::env::temp_dir().join("FeroSSH-drag"), KEEP)
}

/// The stand-ins a drag offers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StandIns {
    /// One per remote entry, in the drag's folder.
    pub paths: Vec<PathBuf>,
    /// The remote names they stand in for, in the same order.
    pub keys: Vec<Vec<u8>>,
    /// Entries left out: no name Windows takes, or one that another entry
    /// already has here (names that differ only in case, say).
    pub left_out: usize,
}

/// Makes an empty file or folder in `folder` for each remote entry
/// (`key`, is a folder), named as a download would name it.
pub fn stand_ins(folder: &Path, entries: &[(Vec<u8>, bool)]) -> std::io::Result<StandIns> {
    let mut out = StandIns::default();
    for (key, dir) in entries {
        let Some(name) = names::local_name(key) else {
            out.left_out += 1;
            continue;
        };
        let path = folder.join(name);
        let made = if *dir {
            std::fs::create_dir(&path)
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map(drop)
        };
        match made {
            Ok(()) => {
                out.paths.push(path);
                out.keys.push(key.clone());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => out.left_out += 1,
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Removes the stand-ins, so the downloads find nothing in their way.
/// Only what is still empty: never anything with content.
pub fn remove_stand_ins(paths: &[PathBuf]) {
    for p in paths {
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => {
                let _ = std::fs::remove_dir(p);
            }
            Ok(m) if m.is_file() && m.len() == 0 => {
                let _ = std::fs::remove_file(p);
            }
            _ => {}
        }
    }
}

/// The downloads for one drag, shared by the file pane (which waits for
/// them) and the main window (which queues them).
#[derive(Debug, Default)]
pub struct Wait {
    /// The transfer queue they run on, once queued.
    pub queue: Option<String>,
    pub jobs: Vec<JobId>,
    /// `Ok` once every one is done; why not, when one failed, was
    /// cancelled or could not start.
    pub outcome: Option<Result<(), String>>,
}

pub type Ticket = Rc<RefCell<Wait>>;

/// Where the downloads stand, from their jobs' states (`None`: the job is
/// gone from the queue).
pub fn outcome(states: impl IntoIterator<Item = Option<State>>) -> Option<Result<(), String>> {
    let mut open = false;
    let mut any = false;
    for state in states {
        any = true;
        match state {
            None | Some(State::Cancelled | State::Skipped) => {
                return Some(Err("The download for the drop was cancelled.".into()));
            }
            Some(State::Failed) => {
                return Some(Err(
                    "A download for the drop failed: the queue says why.".into()
                ));
            }
            Some(State::Done) => {}
            Some(_) => open = true,
        }
    }
    if !any {
        return Some(Err("Nothing to download for the drop.".into()));
    }
    (!open).then_some(Ok(()))
}

/// A file inside one of the drag's folder downloads asks whether to
/// overwrite (two names the same on Windows): nobody can answer while
/// the drop waits, so it is cancelled instead.
pub fn asking_under(jobs: &[ferossh_sftp::Job], ours: &[JobId]) -> bool {
    jobs.iter().filter(|j| j.state == State::Asking).any(|j| {
        let mut at = Some(j.id);
        // Bounded, in case of a loop.
        for _ in 0..=jobs.len() {
            let Some(id) = at else {
                return false;
            };
            if ours.contains(&id) {
                return true;
            }
            at = jobs.iter().find(|p| p.id == id).and_then(|p| p.parent);
        }
        false
    })
}

#[cfg(windows)]
pub use ole::drag;

#[cfg(windows)]
#[allow(unsafe_code)]
mod ole {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    use windows::Win32::Foundation::{
        DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, S_OK,
    };
    use windows::Win32::System::Com::{CoTaskMemFree, IDataObject};
    use windows::Win32::System::Ole::{
        DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropSource, IDropSource_Impl, OleInitialize,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        BHID_DataObject, SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHParseDisplayName,
    };
    use windows::core::{BOOL, HRESULT, HSTRING, Result, implement};

    /// Called once, when the button is let go, with whether the place
    /// under the mouse said it takes the files: true lets the drop
    /// through.
    pub type OnDrop = Box<dyn FnOnce(bool) -> bool>;

    #[implement(IDropSource)]
    struct Source {
        on_drop: RefCell<Option<OnDrop>>,
        /// What the place under the mouse said it would do.
        effect: Cell<DROPEFFECT>,
    }

    impl IDropSource_Impl for Source_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() {
                return DRAGDROP_S_CANCEL;
            }
            if keys.0 & MK_LBUTTON.0 != 0 {
                return S_OK;
            }
            let taken = self.effect.get() != DROPEFFECT_NONE;
            let on_drop = self.on_drop.borrow_mut().take();
            if on_drop.is_some_and(|f| f(taken)) {
                DRAGDROP_S_DROP
            } else {
                DRAGDROP_S_CANCEL
            }
        }

        fn GiveFeedback(&self, effect: DROPEFFECT) -> HRESULT {
            self.effect.set(effect);
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    thread_local! {
        static OLE: Cell<bool> = const { Cell::new(false) };
    }

    /// OLE on the UI thread, for good: a target may still use the drag's
    /// data after the drag returns, which needs OLE up then.
    fn ole() -> Result<()> {
        if OLE.get() {
            return Ok(());
        }
        // SAFETY: on the UI thread, once; never undone.
        unsafe { OleInitialize(None)? };
        OLE.set(true);
        Ok(())
    }

    /// The shell's own data object for `paths` (all in one folder), as
    /// Explorer makes for its files: file names, the list of items and
    /// the drag image all come with it.
    fn data_object(paths: &[PathBuf]) -> Result<IDataObject> {
        let mut pidls: Vec<*const ITEMIDLIST> = Vec::with_capacity(paths.len());
        let made = (|| -> Result<IDataObject> {
            for p in paths {
                let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
                // SAFETY: the path string lives through the call; the
                // item ID list it returns is freed below.
                unsafe {
                    SHParseDisplayName(&HSTRING::from(p.as_os_str()), None, &mut pidl, 0, None)?;
                }
                pidls.push(pidl);
            }
            // SAFETY: valid item ID lists, which the array copies.
            let items = unsafe { SHCreateShellItemArrayFromIDLists(&pidls)? };
            // SAFETY: a plain call on the array.
            unsafe { items.BindToHandler(None, &BHID_DataObject) }
        })();
        for pidl in pidls {
            // SAFETY: each came from SHParseDisplayName, freed once.
            unsafe { CoTaskMemFree(Some(pidl.cast())) };
        }
        made
    }

    /// Drags `paths` from window `hwnd` (with the left button down) until
    /// it is let go; `on_drop` decides whether it drops. Files are only
    /// ever copied: a "move" would leave a target thinking it may delete
    /// what it was given.
    pub fn drag(hwnd: HWND, paths: &[PathBuf], on_drop: OnDrop) -> Result<DROPEFFECT> {
        ole()?;
        let data = data_object(paths)?;
        let source: IDropSource = Source {
            on_drop: RefCell::new(Some(on_drop)),
            effect: Cell::new(DROPEFFECT_NONE),
        }
        .into();
        // SAFETY: COM objects that live through the call, on the thread
        // OLE is up on.
        unsafe { SHDoDragDrop(Some(hwnd), &data, &source, DROPEFFECT_COPY) }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("ferossh-drag-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn stand_ins_are_empty_and_named_as_downloads() {
        let dir = Dir::new("names");
        let entries = vec![
            (b"report.pdf".to_vec(), false),
            (b"logs".to_vec(), true),
            (b"a:b?.txt".to_vec(), false),
            // Same name on Windows' case-blind disks (and the same name
            // once sanitised): left out, not overwritten.
            (b"a_b_.txt".to_vec(), false),
            (b"..".to_vec(), true),
        ];
        let s = stand_ins(dir.path(), &entries).unwrap();
        assert_eq!(s.keys.len(), s.paths.len());
        assert_eq!(s.keys[0], b"report.pdf");
        assert_eq!(s.paths[0], dir.path().join("report.pdf"));
        assert!(s.paths[1].is_dir());
        assert_eq!(s.paths[2], dir.path().join("a_b_.txt"));
        assert_eq!(s.left_out, 2);
        assert_eq!(std::fs::metadata(&s.paths[0]).unwrap().len(), 0);

        // A real download landed in one; the others go.
        std::fs::write(&s.paths[2], b"data").unwrap();
        remove_stand_ins(&s.paths);
        assert!(!s.paths[0].exists());
        assert!(!s.paths[1].exists());
        assert_eq!(std::fs::read(&s.paths[2]).unwrap(), b"data");
    }

    #[test]
    fn a_folder_with_something_in_it_stays() {
        let dir = Dir::new("full");
        let s = stand_ins(dir.path(), &[(b"d".to_vec(), true)]).unwrap();
        std::fs::write(s.paths[0].join("x"), b"x").unwrap();
        remove_stand_ins(&s.paths);
        assert!(s.paths[0].join("x").exists());
    }

    #[test]
    fn the_drop_waits_for_every_download() {
        use State::*;
        assert_eq!(outcome([Some(Done), Some(Running)]), None);
        assert_eq!(outcome([Some(Queued), Some(Waiting)]), None);
        assert_eq!(outcome([Some(Done), Some(Done)]), Some(Ok(())));
        assert!(matches!(outcome([Some(Done), Some(Failed)]), Some(Err(_))));
        assert!(matches!(
            outcome([Some(Running), Some(Cancelled)]),
            Some(Err(_))
        ));
        assert!(matches!(outcome([Some(Running), None]), Some(Err(_))));
        assert!(matches!(outcome([]), Some(Err(_))));
    }
}
