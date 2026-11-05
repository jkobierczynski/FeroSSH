//! The snippets folder, `%APPDATA%\FeroSSH\snippets`: one file per command
//! snippet, so snippets can be copied to another PC, shared, kept in git
//! or backed up one by one.
//!
//! A file holds one `Snippet` block of the settings syntax (see
//! [`crate::settings`]) and ends in `.snippet`. The folder a file is in,
//! below the snippets folder, is the snippet's category:
//!
//! ```text
//! snippets\
//!     Show disk use.snippet           no category
//!     Network\
//!         Show interfaces.snippet     category Network
//!         Cisco\
//!             Save config.snippet     category Network/Cisco
//! ```
//!
//! A file is named after its snippet, with the characters Windows does not
//! allow in a file name made `_`. Dropping a file or a folder of files
//! into the snippets folder adds those snippets; FeroSSH reads the folder
//! again each time the snippets view opens.
//!
//! Older versions kept snippets in `ferossh.conf`; [`migrate`] moves them
//! here.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use crate::login::Snippet;
use crate::settings::{Settings, snippet_block};

/// What a snippet file's name ends in.
pub const EXTENSION: &str = "snippet";
/// Levels of categories within categories.
pub const MAX_DEPTH: usize = 4;
/// Snippet files read at most, so a wrong folder never stalls the window.
const MAX_FILES: usize = 5000;
/// The biggest snippet file read.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// The longest file or folder name written, in characters.
const MAX_NAME: usize = 80;

/// A name made safe as one Windows file or folder name: no `<>:"/\|?*`
/// or control characters, no dots or spaces at the ends, not a device
/// name such as `CON`, and at most [`MAX_NAME`] characters. Empty when
/// nothing is left.
pub fn file_part(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(MAX_NAME)
        .collect();
    let trimmed = mapped.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    if device {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// A category as typed (`Network / Cisco`, `Network\Cisco`) made into
/// what the folders will be called: levels split at `/` or `\`, each a
/// safe folder name, at most [`MAX_DEPTH`] of them, joined with `/`.
pub fn clean_category(text: &str) -> String {
    text.split(['/', '\\'])
        .map(file_part)
        .filter(|p| !p.is_empty())
        .take(MAX_DEPTH)
        .collect::<Vec<_>>()
        .join("/")
}

/// Where `snippet` belongs, relative to the snippets folder with `/`
/// between parts: its category's folders, then its name.
pub fn file_for(snippet: &Snippet) -> String {
    let mut name = file_part(&snippet.name);
    if name.is_empty() {
        name = "snippet".into();
    }
    let category = clean_category(&snippet.category);
    if category.is_empty() {
        format!("{name}.{EXTENSION}")
    } else {
        format!("{category}/{name}.{EXTENSION}")
    }
}

/// `rel` (with `/` between parts) as a path under `dir`; `None` for one
/// that would leave `dir`.
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

/// What a snippet's file holds.
pub fn file_text(snippet: &Snippet) -> String {
    let mut out = String::from(
        "# A FeroSSH command snippet. Copy this file into another FeroSSH's snippets\n\
         # folder to bring the snippet along; the folder it is in is its category.\n",
    );
    out.push_str(&snippet_block(snippet));
    out
}

/// The snippet in a file's text, in `category`. Only the first `Snippet`
/// block counts.
pub fn parse_file(text: &str, category: &str) -> Option<Snippet> {
    let mut s = Settings::parse(text).snippets.into_iter().next()?;
    s.category = category.to_owned();
    (!s.lines.is_empty()).then_some(s)
}

/// Sorts snippets the way the snippets view lists them: snippets without
/// a category first, then by category, then by name, ignoring case.
pub fn sort(snippets: &mut [Snippet]) {
    snippets.sort_by_cached_key(|s| {
        (
            !s.category.is_empty(),
            s.category.to_lowercase(),
            s.name.to_lowercase(),
        )
    });
}

/// Every snippet in the folder, sorted (see [`sort`]), and what could not
/// be read. A missing folder has none.
pub fn load(dir: &Path) -> (Vec<Snippet>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let mut dirs = vec![(dir.to_path_buf(), Vec::<String>::new())];
    let mut seen = 0;
    while let Some((at, category)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_FILES {
                problems.push(format!(
                    "{} has more than {MAX_FILES} files; the rest are left out.",
                    dir.display()
                ));
                sort(&mut out);
                return (out, problems);
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if category.len() < MAX_DEPTH && !name.starts_with('.') {
                    let mut c = category.clone();
                    c.push(name);
                    dirs.push((entry.path(), c));
                }
                continue;
            }
            let Some(stem) = name
                .rsplit_once('.')
                .filter(|(_, ext)| ext.eq_ignore_ascii_case(EXTENSION))
                .map(|(stem, _)| stem.to_owned())
            else {
                continue;
            };
            if !kind.is_file() || stem.is_empty() {
                continue;
            }
            let path = entry.path();
            let text = match std::fs::metadata(&path) {
                Ok(m) if m.len() > MAX_FILE_BYTES => {
                    problems.push(format!("{} is too big for a snippet.", path.display()));
                    continue;
                }
                Ok(_) => match std::fs::read(&path) {
                    Ok(b) => String::from_utf8_lossy(&b)
                        .trim_start_matches('\u{feff}')
                        .to_owned(),
                    Err(e) => {
                        problems.push(format!("Could not read {}: {e}", path.display()));
                        continue;
                    }
                },
                Err(e) => {
                    problems.push(format!("Could not read {}: {e}", path.display()));
                    continue;
                }
            };
            match parse_file(&text, &category.join("/")) {
                Some(mut s) => {
                    let mut rel = category.clone();
                    rel.push(name);
                    s.file = Some(rel.join("/"));
                    out.push(s);
                }
                None => problems.push(format!(
                    "{} has no snippet: it needs a Snippet line and at least one Line.",
                    path.display()
                )),
            }
        }
    }
    sort(&mut out);
    (out, problems)
}

/// Writes a file through a temporary one, so a crash never leaves half a
/// file.
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("snippet-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Removes `path`'s empty folders, up to `dir`.
fn prune(dir: &Path, path: &Path) {
    let mut at = path.parent();
    while let Some(p) = at {
        if p == dir || !p.starts_with(dir) || std::fs::remove_dir(p).is_err() {
            break;
        }
        at = p.parent();
    }
}

/// Saves `snippet` in its file, named after its category and name; when
/// that changed, the file it had is removed. Its `file` is set to the
/// new one. Another snippet's file is never overwritten: that is an
/// error saying so.
pub fn save(dir: &Path, snippet: &mut Snippet) -> Result<(), String> {
    snippet.category = clean_category(&snippet.category);
    let rel = file_for(snippet);
    let path = under(dir, &rel).ok_or_else(|| format!("{rel} is not a file name."))?;
    let old = snippet
        .file
        .as_deref()
        .and_then(|f| under(dir, f).map(|p| (f.to_owned(), p)));
    let same = old
        .as_ref()
        .is_some_and(|(f, _)| f.eq_ignore_ascii_case(&rel));
    if !same && path.exists() {
        let mut why = format!("There is already a snippet called {}", snippet.name);
        if !snippet.category.is_empty() {
            let _ = write!(why, " in {}", snippet.category);
        }
        why.push('.');
        return Err(why);
    }
    write_file(&path, &file_text(snippet))
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    if let Some((f, old_path)) = old
        && !f.eq_ignore_ascii_case(&rel)
    {
        let _ = std::fs::remove_file(&old_path);
        prune(dir, &old_path);
    }
    snippet.file = Some(rel);
    Ok(())
}

/// Removes `snippet`'s file, and its category's folders when they are
/// left empty.
pub fn remove(dir: &Path, snippet: &Snippet) -> io::Result<()> {
    let Some(path) = snippet.file.as_deref().and_then(|f| under(dir, f)) else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    prune(dir, &path);
    Ok(())
}

/// Whether a snippet like this one (same category and name) is in
/// `snippets` already.
pub fn has(snippets: &[Snippet], snippet: &Snippet) -> bool {
    let rel = file_for(snippet);
    snippets
        .iter()
        .any(|s| file_for(s).eq_ignore_ascii_case(&rel))
}

/// Adds `snippet` as a new file: when its name is taken in its category,
/// the file is called `name (2)`, `name (3)` and so on; the snippet keeps
/// its name.
pub fn add(dir: &Path, snippet: &mut Snippet) -> Result<(), String> {
    snippet.file = None;
    snippet.category = clean_category(&snippet.category);
    let base = file_for(snippet);
    let stem = base.strip_suffix(&format!(".{EXTENSION}")).unwrap_or(&base);
    for n in 1..1000 {
        let rel = if n == 1 {
            base.clone()
        } else {
            format!("{stem} ({n}).{EXTENSION}")
        };
        let path = under(dir, &rel).ok_or_else(|| format!("{rel} is not a file name."))?;
        if path.exists() {
            continue;
        }
        write_file(&path, &file_text(snippet))
            .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
        snippet.file = Some(rel);
        return Ok(());
    }
    Err(format!("Too many snippets are called {}.", snippet.name))
}

/// Moves the snippets an older FeroSSH kept in `ferossh.conf` into the
/// folder, a file each (see [`add`]); returns how many. A snippet whose
/// file is there already with the same text (an earlier move that
/// stopped half way) is not written twice.
pub fn migrate(dir: &Path, old: &[Snippet]) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not make {}: {e}", dir.display()))?;
    let mut n = 0;
    for s in old {
        let mut s = s.clone();
        if s.name.trim().is_empty() || s.lines.is_empty() {
            continue;
        }
        s.category = clean_category(&s.category);
        let done = under(dir, &file_for(&s))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|t| t == file_text(&s));
        if !done {
            add(dir, &mut s)?;
        }
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ferossh-snippets-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn snip(name: &str, category: &str) -> Snippet {
        Snippet {
            name: name.into(),
            category: category.into(),
            lines: vec!["show version".into(), "@key enter".into()],
            hosts: vec!["sw-*".into()],
            hotkey: Some(3),
            ..Snippet::default()
        }
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_part("Show interfaces"), "Show interfaces");
        assert_eq!(file_part("a/b\\c:d*e?\"f\"<g>|h"), "a_b_c_d_e__f__g__h");
        assert_eq!(file_part("  ..hidden. "), "hidden");
        assert_eq!(file_part("con"), "_con");
        assert_eq!(file_part("COM1.txt"), "_COM1.txt");
        assert_eq!(file_part("Console"), "Console");
        assert_eq!(file_part(".."), "");
        assert_eq!(file_part(&"x".repeat(500)).len(), MAX_NAME);
        assert_eq!(
            clean_category(" Network / Cisco\\IOS "),
            "Network/Cisco/IOS"
        );
        assert_eq!(clean_category("../../etc"), "etc");
        assert_eq!(clean_category("a/b/c/d/e/f"), "a/b/c/d");
        assert_eq!(clean_category("//"), "");
        assert_eq!(
            file_for(&snip("Save: all", "Net/Lab")),
            "Net/Lab/Save_ all.snippet"
        );
        assert_eq!(file_for(&snip("..", "")), "snippet.snippet");
        assert!(under(Path::new("x"), "../y").is_none());
        assert!(under(Path::new("x"), "a/../y").is_none());
    }

    #[test]
    fn files_round_trip_with_their_folder_as_category() {
        let dir = temp("round");
        let mut a = snip("Show interfaces", "Network/Cisco");
        let mut b = snip("Disk use", "");
        b.lines.push("@type tail -f /var/log/syslog".into());
        save(&dir, &mut a).unwrap();
        save(&dir, &mut b).unwrap();
        assert_eq!(
            a.file.as_deref(),
            Some("Network/Cisco/Show interfaces.snippet")
        );
        assert!(
            dir.join("Network")
                .join("Cisco")
                .join("Show interfaces.snippet")
                .is_file()
        );
        let (all, problems) = load(&dir);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(all, [b.clone(), a.clone()], "no category first");
        // Renaming moves the file and drops the folders left empty.
        let mut moved = a.clone();
        moved.name = "Interfaces".into();
        moved.category = "Lab".into();
        save(&dir, &mut moved).unwrap();
        assert!(!dir.join("Network").exists());
        let (all, _) = load(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].category, "Lab");
        // A name another snippet has is refused, not overwritten.
        let mut clash = snip("Disk use", "");
        assert!(save(&dir, &mut clash).unwrap_err().contains("already"));
        // Adding finds a free file name instead.
        add(&dir, &mut clash).unwrap();
        assert_eq!(clash.file.as_deref(), Some("Disk use (2).snippet"));
        remove(&dir, &moved).unwrap();
        assert!(!dir.join("Lab").exists());
        let (all, _) = load(&dir);
        assert_eq!(all.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hand_made_and_broken_files() {
        let dir = temp("hand");
        std::fs::create_dir_all(dir.join("Ops")).unwrap();
        std::fs::write(
            dir.join("Ops").join("whatever.SNIPPET"),
            "\u{feff}Snippet \"Restart web\"\n  Line sudo systemctl restart nginx\n",
        )
        .unwrap();
        std::fs::write(dir.join("empty.snippet"), "# nothing\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "Snippet x\n Line y\n").unwrap();
        let (all, problems) = load(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Restart web");
        assert_eq!(all[0].category, "Ops");
        assert_eq!(all[0].file.as_deref(), Some("Ops/whatever.SNIPPET"));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("empty.snippet"));
        // Saving it again under its own name moves it to the file it
        // would be called.
        let mut s = all[0].clone();
        save(&dir, &mut s).unwrap();
        assert!(dir.join("Ops").join("Restart web.snippet").is_file());
        assert_eq!(load(&dir).0.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_keeps_every_snippet() {
        let dir = temp("migrate");
        let mut other_a = snip("A", "");
        other_a.lines = vec!["uptime".into()];
        let old = vec![snip("A", ""), other_a, snip("B", "")];
        assert_eq!(migrate(&dir, &old).unwrap(), 3);
        let (all, _) = load(&dir);
        assert_eq!(all.len(), 3);
        // Moving again after a stop half way adds nothing twice.
        assert_eq!(migrate(&dir, &old[2..]).unwrap(), 1);
        assert_eq!(load(&dir).0.len(), 3);
        assert_eq!(all.iter().filter(|s| s.name == "A").count(), 2);
        assert!(has(&all, &snip("b", "")));
        assert!(!has(&all, &snip("B", "Other")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lines_starting_with_equals_keep_them() {
        let mut s = snip("eq", "");
        s.lines = vec!["==S".into(), "=x".into(), "a=b".into()];
        s.when = "=More=".into();
        let back = parse_file(&file_text(&s), "").unwrap();
        assert_eq!(back.lines, s.lines);
        assert_eq!(back.when, s.when);
    }

    #[test]
    fn lines_cannot_escape_the_block() {
        let mut s = snip("x\nSnippet y", "");
        s.lines = vec!["ls\nSnippet z".into()];
        let text = file_text(&s);
        assert!(!text.contains("\nSnippet y"));
        assert!(!text.contains("\nSnippet z"));
        assert_eq!(parse_file(&text, "").unwrap().lines.len(), 1);
    }
}
