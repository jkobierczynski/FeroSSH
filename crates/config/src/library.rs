//! The command library that ships inside FeroSSH: a few hundred ready
//! one-liners and scripts for Linux, system administration, Windows,
//! DevOps, development, hacking and networking, laid out as ordinary
//! script files under `scripts\Library` so they read, run, copy and
//! change like any other script (see [`crate::snippet_store`]).
//!
//! The library is embedded in the exe (as a handful of bundle files, one
//! per category), so there is nothing to install and the exe stays
//! small. [`sync`] writes it into the scripts folder, and keeps the
//! user's own changes: a library script the user has edited is left as
//! they left it, one the user has deleted stays gone, and only scripts
//! the user has not touched are refreshed when FeroSSH is upgraded, so a
//! new release's new or improved library scripts still arrive. The top
//! folder is `Library`, keeping the shipped set apart from the user's
//! own categories.

use std::path::{Path, PathBuf};

use crate::login::Snippet;
use crate::settings::Settings;
use crate::snippet_store::{file_for, file_text};

/// The folder, below the scripts folder, the library is written into.
pub const FOLDER: &str = "Library";

/// Bumped whenever the embedded library changes, so [`sync`] knows to
/// look the folder over again; otherwise it leaves a folder it has
/// already written for this version alone. The crate version does, so an
/// upgrade always re-syncs.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The bundles built into the exe: a category name and its file, each
/// file holding one `Snippet` block per script.
const BUNDLES: &[(&str, &str)] = &[
    ("Linux", include_str!("../library/Linux.scripts")),
    (
        "System administration",
        include_str!("../library/System administration.scripts"),
    ),
    ("Windows", include_str!("../library/Windows.scripts")),
    ("DevOps", include_str!("../library/DevOps.scripts")),
    (
        "Development",
        include_str!("../library/Development.scripts"),
    ),
    ("Hacking", include_str!("../library/Hacking.scripts")),
    ("Networking", include_str!("../library/Networking.scripts")),
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

/// A 64-bit FNV-1a hash of `bytes`, as hex: enough to tell a file the
/// user has changed from one they have not.
fn hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
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
fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("library-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The manifest file, which records the version last written and the
/// hash of every script as it was shipped. It has no script extension,
/// so the loader passes over it.
fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(FOLDER).join(".manifest")
}

/// The shipped hash of each library file at the last sync, by relative
/// path, and the version written then.
fn read_manifest(dir: &Path) -> (String, std::collections::HashMap<String, String>) {
    let mut version = String::new();
    let mut map = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(manifest_path(dir)) else {
        return (version, map);
    };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("# ferossh-library ") {
            version = v.trim().to_owned();
        } else if let Some((h, rel)) = line.split_once('\t') {
            map.insert(rel.to_owned(), h.to_owned());
        }
    }
    (version, map)
}

/// Brings `scripts_dir/Library` in line with the library built into this
/// FeroSSH, without disturbing the user's own work:
///
/// * a script the folder has never held is written;
/// * a script still exactly as FeroSSH last wrote it is refreshed to the
///   new version (the user had not changed it);
/// * a script the user has edited is left as it is;
/// * a script the user has deleted (shipped before, now gone) stays gone.
///
/// Does nothing when the folder was already written by this version, so
/// opening the scripts view again is free. A folder that cannot be
/// written (read-only, say) is left alone.
pub fn sync(scripts_dir: &Path) {
    let (old_version, shipped) = read_manifest(scripts_dir);
    if old_version == VERSION {
        return;
    }
    let mut manifest = format!("# ferossh-library {VERSION}\n");
    let mut lines: Vec<String> = Vec::new();
    for (rel, text) in wanted() {
        let Some(path) = under(scripts_dir, &rel) else {
            continue;
        };
        let new_hash = hash(text.as_bytes());
        let on_disk = std::fs::read(&path).ok();
        let write = match &on_disk {
            // There: refresh only one the user has left untouched since we
            // wrote it.
            Some(bytes) => shipped.get(&rel).is_some_and(|h| *h == hash(bytes)),
            // Missing: a new script, unless the user deleted one we shipped.
            None => !shipped.contains_key(&rel),
        };
        if write && write_file(&path, &text).is_err() {
            return;
        }
        lines.push(format!("{new_hash}\t{rel}"));
    }
    lines.sort();
    for l in lines {
        manifest.push_str(&l);
        manifest.push('\n');
    }
    let _ = write_file(&manifest_path(scripts_dir), &manifest);
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

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferossh-library-test-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        assert!(scripts.len() > 150, "only {} scripts", scripts.len());
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
    fn sync_writes_the_whole_library_then_loads_it() {
        let dir = temp("write");
        sync(&dir);
        assert_eq!(count_files(&dir), scripts().len());
        let (loaded, problems) = crate::snippet_store::load(&dir);
        assert!(problems.is_empty(), "problems: {problems:?}");
        assert_eq!(loaded.len(), scripts().len());
        assert!(loaded.iter().all(|s| s.category.starts_with(FOLDER)));
    }

    #[test]
    fn sync_is_cheap_the_second_time() {
        let dir = temp("again");
        sync(&dir);
        // A library file, edited by hand.
        let edited = under(&dir, &file_for(&scripts()[0])).unwrap();
        std::fs::write(&edited, "Snippet \"Mine\"\n    Line echo mine\n").unwrap();
        // Same version: sync leaves everything, including the edit.
        sync(&dir);
        assert_eq!(
            std::fs::read_to_string(&edited).unwrap(),
            "Snippet \"Mine\"\n    Line echo mine\n"
        );
    }

    #[test]
    fn an_edited_script_survives_an_upgrade_but_an_untouched_one_refreshes() {
        let dir = temp("upgrade");
        // Pretend an older FeroSSH wrote the library: a manifest of a
        // different version, with the files it shipped.
        sync(&dir);
        let manifest = std::fs::read_to_string(manifest_path(&dir)).unwrap();
        let older = manifest.replacen(VERSION, "0.0.0-old", 1);
        std::fs::write(manifest_path(&dir), &older).unwrap();

        let all = scripts();
        let edited_rel = file_for(&all[0]);
        let untouched_rel = file_for(&all[1]);
        let edited = under(&dir, &edited_rel).unwrap();
        let untouched = under(&dir, &untouched_rel).unwrap();
        // The user edits the first; the second stays exactly as shipped.
        std::fs::write(&edited, "Snippet \"Changed\"\n    Line echo hi\n").unwrap();

        sync(&dir);
        // The edit is kept.
        assert_eq!(
            std::fs::read_to_string(&edited).unwrap(),
            "Snippet \"Changed\"\n    Line echo hi\n"
        );
        // The untouched one is the shipped text.
        assert_eq!(
            std::fs::read_to_string(&untouched).unwrap(),
            file_text(&all[1])
        );
        // The manifest now records this version.
        assert!(
            std::fs::read_to_string(manifest_path(&dir))
                .unwrap()
                .contains(VERSION)
        );
    }

    #[test]
    fn a_deleted_library_script_stays_deleted_on_the_next_run() {
        let dir = temp("delete");
        sync(&dir);
        let gone_rel = file_for(&scripts()[0]);
        let gone = under(&dir, &gone_rel).unwrap();
        std::fs::remove_file(&gone).unwrap();
        // A new version comes along.
        let manifest = std::fs::read_to_string(manifest_path(&dir)).unwrap();
        std::fs::write(
            manifest_path(&dir),
            manifest.replacen(VERSION, "0.0.0-old", 1),
        )
        .unwrap();
        sync(&dir);
        assert!(!gone.exists(), "a deleted library script came back");
    }
}
