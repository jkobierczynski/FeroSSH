//! The command library that ships inside FeroSSH: a few hundred ready
//! one-liners and scripts for Linux, system administration, Windows,
//! networking, databases, containers, the cloud, security, hacking and
//! more, laid out as ordinary script files under `scripts\Library` so
//! they read, run, copy and change like any other script (see
//! [`crate::snippet_store`]).
//!
//! The library is embedded in the exe (as a handful of bundle files, one
//! per category), so there is nothing to install and the exe stays small.
//! It is not written automatically: the user asks for it from
//! Session > Scripts > Write built-in scripts, which calls [`write`]. A
//! script that is not in the folder yet is written; a script already
//! there is left as the user has it unless they choose to overwrite. So
//! the user's own edits and deletions are never disturbed behind their
//! back. The top folder is `Library`, keeping the shipped set apart from
//! the user's own categories.

use std::io;
use std::path::{Path, PathBuf};

use crate::login::Snippet;
use crate::settings::Settings;
use crate::snippet_store::{file_for, file_text};

/// The folder, below the scripts folder, the library is written into.
pub const FOLDER: &str = "Library";

/// The bundles built into the exe: a category name and its file, each
/// file holding one `Snippet` block per script.
const BUNDLES: &[(&str, &str)] = &[
    ("Linux", include_str!("../library/Linux.scripts")),
    (
        "System administration",
        include_str!("../library/System administration.scripts"),
    ),
    ("Windows", include_str!("../library/Windows.scripts")),
    ("Networking", include_str!("../library/Networking.scripts")),
    (
        "Network devices",
        include_str!("../library/Network devices.scripts"),
    ),
    (
        "Security & hardening",
        include_str!("../library/Security & hardening.scripts"),
    ),
    (
        "Monitoring & logs",
        include_str!("../library/Monitoring & logs.scripts"),
    ),
    (
        "Storage & backup",
        include_str!("../library/Storage & backup.scripts"),
    ),
    ("Databases", include_str!("../library/Databases.scripts")),
    (
        "Web servers & TLS",
        include_str!("../library/Web servers & TLS.scripts"),
    ),
    (
        "Containers & Kubernetes",
        include_str!("../library/Containers & Kubernetes.scripts"),
    ),
    ("DevOps", include_str!("../library/DevOps.scripts")),
    ("Cloud", include_str!("../library/Cloud.scripts")),
    (
        "Virtualization",
        include_str!("../library/Virtualization.scripts"),
    ),
    (
        "Development",
        include_str!("../library/Development.scripts"),
    ),
    ("Hacking", include_str!("../library/Hacking.scripts")),
];

/// Every library script, each with its category set to `Library/<name>`.
pub fn scripts() -> Vec<Snippet> {
    let mut out = Vec::new();
    for (category, text) in BUNDLES {
        for mut s in Settings::parse(text).snippets {
            if s.name.trim().is_empty() || s.lines.is_empty() {
                continue;
            }
            s.category = format!("{FOLDER}/{category}");
            s.file = None;
            out.push(s);
        }
    }
    out
}

/// A library file: where it goes under the scripts folder (`/` between
/// parts) and what it holds.
fn wanted() -> Vec<(String, String)> {
    scripts()
        .iter()
        .map(|s| (file_for(s), file_text(s)))
        .collect()
}

/// `rel` (with `/` between parts) as a path under `dir`, if it stays
/// within it.
fn under(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut p = dir.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':']) {
            return None;
        }
        p.push(part);
    }
    Some(p)
}

/// Writes `text` to `path` through a temporary file, making its folders.
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("library-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// How the built-in library stands against the scripts folder, so the
/// menu can tell the user what writing it would do.
#[derive(Debug)]
pub struct Status {
    /// Library scripts not in the folder yet (a fresh [`write`] adds them).
    pub missing: usize,
    /// Library scripts already in the folder (left alone unless the user
    /// chooses to overwrite).
    pub present: usize,
}

impl Status {
    /// The whole library, however it is split between on disk and not.
    pub fn total(&self) -> usize {
        self.missing + self.present
    }
}

/// Looks over the scripts folder without changing anything.
pub fn status(scripts_dir: &Path) -> Status {
    let mut missing = 0;
    let mut present = 0;
    for (rel, _) in wanted() {
        match under(scripts_dir, &rel) {
            Some(path) if path.exists() => present += 1,
            Some(_) => missing += 1,
            None => {}
        }
    }
    Status { missing, present }
}

/// What a [`write`] did.
#[derive(Debug)]
pub struct Written {
    /// Scripts written (new ones, plus overwritten ones when asked).
    pub written: usize,
    /// Scripts left as the user had them (already there, not overwriting).
    pub kept: usize,
}

/// Writes the built-in library into the scripts folder.
///
/// A script that is not there yet is always written. A script that is
/// already there is overwritten only when `overwrite` is true; otherwise
/// it is left exactly as the user has it and counted as kept. Returns how
/// many were written and how many kept, or the first write error (the
/// folder is read-only, say), with whatever was written so far left in
/// place.
pub fn write(scripts_dir: &Path, overwrite: bool) -> io::Result<Written> {
    let mut written = 0;
    let mut kept = 0;
    for (rel, text) in wanted() {
        let Some(path) = under(scripts_dir, &rel) else {
            continue;
        };
        if path.exists() && !overwrite {
            kept += 1;
            continue;
        }
        write_file(&path, &text)?;
        written += 1;
    }
    Ok(Written { written, kept })
}

/// Whether `rel` (a script's file, `/` between parts) is one of the
/// library's, so the view can mark it or treat it as read-only elsewhere
/// if it ever wants to. Currently used by tests.
#[allow(dead_code)]
pub fn is_library(rel: &str) -> bool {
    rel.split('/').next() == Some(FOLDER)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::snippet_store::EXTENSION;

    /// A temporary directory that cleans itself up when the test ends,
    /// pass or fail. Derefs to its path, so `write(&dir, ..)` works.
    struct TempDir(PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "ferossh-library-test-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn count_files(dir: &Path) -> usize {
        let mut n = 0;
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            for e in std::fs::read_dir(&at).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == EXTENSION) {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn library_parses_and_has_every_category() {
        let scripts = scripts();
        assert!(scripts.len() > 200, "only {} scripts", scripts.len());
        for (category, _) in BUNDLES {
            let want = format!("{FOLDER}/{category}");
            assert!(
                scripts.iter().any(|s| s.category == want),
                "no scripts for {category}"
            );
        }
        // Every script has a name, lines and a file that stays in Library.
        for s in &scripts {
            assert!(!s.name.trim().is_empty());
            assert!(!s.lines.is_empty());
            assert!(is_library(&file_for(s)));
        }
    }

    #[test]
    fn two_scripts_never_want_the_same_file() {
        let scripts = scripts();
        let mut files: Vec<String> = scripts.iter().map(|s| file_for(s).to_lowercase()).collect();
        files.sort();
        let before = files.len();
        files.dedup();
        assert_eq!(before, files.len(), "two library scripts share a file name");
    }

    #[test]
    fn write_puts_the_whole_library_down_then_loads_it() {
        let dir = temp("write");
        let n = scripts().len();
        let before = status(&dir);
        assert_eq!(before.missing, n);
        assert_eq!(before.present, 0);

        let done = write(&dir, false).unwrap();
        assert_eq!(done.written, n);
        assert_eq!(done.kept, 0);
        assert_eq!(count_files(&dir), n);

        let (loaded, problems) = crate::snippet_store::load(&dir);
        assert!(problems.is_empty(), "problems: {problems:?}");
        assert_eq!(loaded.len(), n);
        assert!(loaded.iter().all(|s| s.category.starts_with(FOLDER)));

        // Now everything is present.
        let after = status(&dir);
        assert_eq!(after.missing, 0);
        assert_eq!(after.present, n);
    }

    #[test]
    fn without_overwrite_an_edited_script_is_kept() {
        let dir = temp("keep");
        write(&dir, false).unwrap();
        let edited = under(&dir, &file_for(&scripts()[0])).unwrap();
        let mine = "Snippet \"Mine\"\n    Line echo mine\n";
        std::fs::write(&edited, mine).unwrap();

        // A second write without overwrite leaves the edit and writes nothing.
        let done = write(&dir, false).unwrap();
        assert_eq!(done.written, 0);
        assert_eq!(done.kept, scripts().len());
        assert_eq!(std::fs::read_to_string(&edited).unwrap(), mine);
    }

    #[test]
    fn with_overwrite_an_edited_script_is_replaced() {
        let dir = temp("overwrite");
        write(&dir, false).unwrap();
        let first = &scripts()[0];
        let edited = under(&dir, &file_for(first)).unwrap();
        std::fs::write(&edited, "Snippet \"Mine\"\n    Line echo mine\n").unwrap();

        // Overwriting restores the shipped text for every script.
        let done = write(&dir, true).unwrap();
        assert_eq!(done.written, scripts().len());
        assert_eq!(done.kept, 0);
        assert_eq!(std::fs::read_to_string(&edited).unwrap(), file_text(first));
    }

    #[test]
    fn a_deleted_script_comes_back_only_on_the_next_write() {
        let dir = temp("delete");
        write(&dir, false).unwrap();
        let gone = under(&dir, &file_for(&scripts()[0])).unwrap();
        std::fs::remove_file(&gone).unwrap();

        // status counts it as missing again.
        assert_eq!(status(&dir).missing, 1);

        // Writing (even without overwrite) puts the one missing file back.
        let done = write(&dir, false).unwrap();
        assert_eq!(done.written, 1);
        assert!(gone.exists());
    }
}
