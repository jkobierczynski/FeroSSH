//! Where FeroSSH keeps its files, and portable use. Normally settings
//! and connections go in `%APPDATA%\FeroSSH` and what belongs to this PC
//! only (logs, command history, approvals) in `%LOCALAPPDATA%\FeroSSH`.
//!
//! When a folder named `FeroSSH-data` sits beside `ferossh.exe` (on a USB
//! stick, say), FeroSSH is portable: everything it writes goes in that
//! folder, `known_hosts` included, and it saves no passwords in the
//! Windows Credential Manager (only in the master password vault), so a
//! PC it ran on keeps nothing of it. Approvals of password and proxy
//! commands are not kept at all then: the folder travels, so a command
//! is asked about again each time FeroSSH starts.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The folder beside the exe that makes FeroSSH portable.
pub const DATA_DIR: &str = "FeroSSH-data";

/// The data folder of a portable FeroSSH whose exe is `exe`.
fn data_dir_for(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?.join(DATA_DIR);
    dir.is_dir().then_some(dir)
}

/// The data folder when FeroSSH runs portable; looked up once.
pub fn data_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| data_dir_for(&std::env::current_exe().ok()?))
        .as_deref()
}

pub fn is_portable() -> bool {
    data_dir().is_some()
}

/// FeroSSH's folder for settings and connections: `%APPDATA%\FeroSSH`,
/// or the portable data folder.
pub fn app_dir() -> PathBuf {
    if let Some(d) = data_dir() {
        return d.to_path_buf();
    }
    std::env::var_os("APPDATA")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("FeroSSH")
}

/// FeroSSH's folder for what stays on this PC: `%LOCALAPPDATA%\FeroSSH`,
/// or `local` in the portable data folder. `None` when Windows names no
/// such folder.
pub fn local_dir() -> Option<PathBuf> {
    if let Some(d) = data_dir() {
        return Some(d.join("local"));
    }
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("FeroSSH"))
}

/// The `known_hosts` file FeroSSH writes when portable; `None` means the
/// user's own `~/.ssh/known_hosts`.
pub fn known_hosts() -> Option<PathBuf> {
    data_dir().map(|d| d.join("known_hosts"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn portable_only_with_the_folder() {
        let root = std::env::temp_dir().join(format!("ferossh-portable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("ferossh.exe");
        assert_eq!(data_dir_for(&exe), None);
        std::fs::write(root.join(DATA_DIR), "").unwrap();
        // A file of that name is not the folder.
        assert_eq!(data_dir_for(&exe), None);
        std::fs::remove_file(root.join(DATA_DIR)).unwrap();
        std::fs::create_dir(root.join(DATA_DIR)).unwrap();
        assert_eq!(data_dir_for(&exe), Some(root.join(DATA_DIR)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
