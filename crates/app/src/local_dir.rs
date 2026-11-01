//! The file view's Local PC section: a listing of one folder on this PC,
//! read straight from disk (the UI process does disk I/O; the sandboxed
//! workers never see local files).
//!
//! Above a drive's root comes "This PC", the list of drives, on Windows.

use std::path::{Path, PathBuf};

/// One row of the local listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalEntry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub size: u64,
}

/// Where `..` leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Up {
    Folder(PathBuf),
    /// The list of drives.
    Drives,
}

/// A listed local folder, or the list of drives.
#[derive(Debug, Default)]
pub struct LocalDir {
    /// The folder listed; `None` while the drives are listed.
    pub dir: Option<PathBuf>,
    pub up: Option<Up>,
    pub entries: Vec<LocalEntry>,
    pub error: Option<String>,
}

impl LocalDir {
    /// Lists `dir`: folders first, then files, each by name ignoring
    /// case. Hidden and system files are left out, as Explorer does.
    pub fn read(dir: &Path) -> Self {
        let up = match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => Some(Up::Folder(p.to_path_buf())),
            _ if !drives().is_empty() => Some(Up::Drives),
            _ => None,
        };
        let mut out = Self {
            dir: Some(dir.to_path_buf()),
            up,
            ..Self::default()
        };
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                out.error = Some(format!("{}: {e}", dir.display()));
                return out;
            }
        };
        for item in read.flatten() {
            // Follows links, so a linked folder opens like a folder.
            let Ok(meta) = std::fs::metadata(item.path()) else {
                continue;
            };
            if hidden(&item) {
                continue;
            }
            out.entries.push(LocalEntry {
                name: item.file_name().to_string_lossy().into_owned(),
                path: item.path(),
                dir: meta.is_dir(),
                size: meta.len(),
            });
        }
        out.entries.sort_by(|a, b| {
            b.dir
                .cmp(&a.dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        out
    }

    /// The drives of this PC, as "This PC" lists them.
    pub fn drives() -> Self {
        Self {
            dir: None,
            up: None,
            entries: drives()
                .into_iter()
                .map(|path| LocalEntry {
                    name: path.display().to_string(),
                    path,
                    dir: true,
                    size: 0,
                })
                .collect(),
            error: None,
        }
    }

    /// The title's name for what is listed.
    pub fn label(&self) -> String {
        self.dir
            .as_ref()
            .map_or_else(|| "This PC".to_owned(), |d| d.display().to_string())
    }
}

#[cfg(windows)]
fn hidden(item: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    item.metadata()
        .is_ok_and(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0)
}

#[cfg(not(windows))]
fn hidden(_: &std::fs::DirEntry) -> bool {
    false
}

/// The drive roots, `C:\` and so on.
#[cfg(windows)]
#[allow(unsafe_code)]
fn drives() -> Vec<PathBuf> {
    // SAFETY: no arguments; returns a bit mask.
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| PathBuf::from(format!("{}:\\", char::from(b'A' + i))))
        .collect()
}

#[cfg(not(windows))]
fn drives() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_first_by_name() {
        let dir = std::env::temp_dir().join(format!("ferossh-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Beta")).unwrap();
        std::fs::create_dir_all(dir.join("alpha")).unwrap();
        std::fs::write(dir.join("b.txt"), b"12345").unwrap();
        std::fs::write(dir.join("A.txt"), b"1").unwrap();
        let l = LocalDir::read(&dir);
        let names: Vec<_> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Beta", "A.txt", "b.txt"]);
        assert!(l.entries[0].dir && !l.entries[2].dir);
        assert_eq!(l.entries[3].size, 5);
        assert_eq!(l.up, Some(Up::Folder(std::env::temp_dir())));
        assert_eq!(l.label(), dir.display().to_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_folder_says_why() {
        let l = LocalDir::read(Path::new("/no/such/ferossh/folder"));
        assert!(l.entries.is_empty());
        assert!(l.error.unwrap().starts_with("/no/such/ferossh/folder: "));
    }

    #[test]
    fn above_the_root_come_the_drives() {
        let l = LocalDir::read(Path::new("/"));
        // Windows has drives to list; elsewhere the root is the top.
        let want = if cfg!(windows) {
            Some(Up::Drives)
        } else {
            None
        };
        assert_eq!(l.up, want);
        if cfg!(windows) {
            assert!(LocalDir::drives().entries.iter().any(|e| e.dir));
        }
        assert_eq!(LocalDir::drives().label(), "This PC");
    }
}
