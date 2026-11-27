//! Git worktrees for coding agents on this PC (Session > Coding agents >
//! Start <agent> in a new git worktree, from a Windows shell): the same
//! rules as [`crate::worktree`] on a server, carried out with this PC's
//! own `git.exe` instead of a script, each argument passed whole.
//!
//! The worktree goes next to the repository's main worktree as
//! `<repo>-<agent>` on branch `<agent>` (`-2` … `-99` when taken), from
//! the commit the folder has checked out. Removing runs `git worktree
//! remove` without `--force` (git keeps a worktree with changes) and then
//! `git branch -d` (git keeps a branch that is not merged), and never
//! touches the main worktree.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::worktree::{Added, Listed, Removal, Worktree, valid_branch};

/// Branches tried at most, as on a server.
const TRIES: u32 = 99;

/// What git printed last on standard error, made safe to show.
fn said(err: &[u8]) -> String {
    let text = String::from_utf8_lossy(err);
    let line = text.lines().map(str::trim).rfind(|l| !l.is_empty());
    crate::ssh::sanitize_line(line.unwrap_or(""))
        .chars()
        .take(300)
        .collect()
}

/// Environment variables that tell git which repository to use.
const GIT_LOCATION_VARS: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

struct Git<'a> {
    program: &'a Path,
}

struct Ran {
    ok: bool,
    out: String,
    /// `out` as git wrote it.
    raw: Vec<u8>,
    err: Vec<u8>,
}

impl Git<'_> {
    fn run(&self, dir: &Path, args: &[&str]) -> Option<Ran> {
        let mut command = Command::new(self.program);
        command
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .env("GIT_TERMINAL_PROMPT", "0");
        // A FeroSSH started from a git hook or a shell that set these
        // would point git at another repository than `dir`.
        for name in GIT_LOCATION_VARS {
            command.env_remove(name);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console flashes up.
            command.creation_flags(0x0800_0000);
        }
        let output = command.output().ok()?;
        Some(Ran {
            ok: output.status.success(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            raw: output.stdout,
            err: output.stderr,
        })
    }

    /// The main worktree of the repository `dir` is in, from `git
    /// worktree list` (its first entry), else `toplevel`.
    fn main_worktree(&self, dir: &Path, toplevel: &Path) -> PathBuf {
        self.run(dir, &["worktree", "list", "--porcelain"])
            .filter(|r| r.ok)
            .and_then(|r| {
                r.out
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("worktree "))
                    .map(native)
            })
            .filter(|m| m.is_dir())
            .unwrap_or_else(|| toplevel.to_path_buf())
    }

    fn toplevel(&self, dir: &Path) -> Option<PathBuf> {
        self.run(dir, &["rev-parse", "--show-toplevel"])
            .filter(|r| r.ok)
            .map(|r| r.out.trim_end_matches(['\r', '\n']).to_owned())
            .filter(|t| !t.is_empty())
            .map(|t| native(&t))
    }
}

/// A path as git prints it (`C:/Users/me/app` on Windows) in this PC's
/// own form.
fn native(path: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.replace('/', "\\"))
    } else {
        PathBuf::from(path)
    }
}

fn same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Makes a worktree for an agent whose branches start with `base`, of the
/// repository folder `dir` is in, with git `program`.
pub fn add(program: &Path, dir: &Path, base: &str) -> Added {
    if !valid_branch(base) {
        return Added::Failed("not a branch name FeroSSH makes".into());
    }
    let git = Git { program };
    if git.run(Path::new("."), &["--version"]).is_none() {
        return Added::NoGit;
    }
    if !dir.is_dir() {
        return Added::NoDir;
    }
    let Some(top) = git.toplevel(dir) else {
        return Added::NoRepo;
    };
    let main = git.main_worktree(dir, &top);
    let (Some(parent), Some(repo)) = (main.parent(), main.file_name()) else {
        return Added::BadPath;
    };
    let repo = repo.to_string_lossy().into_owned();
    let mut n = 1;
    let (branch, path) = loop {
        let branch = if n == 1 {
            base.to_owned()
        } else {
            format!("{base}-{n}")
        };
        let path = parent.join(format!("{repo}-{branch}"));
        let taken = git
            .run(
                dir,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .is_some_and(|r| r.ok)
            || path.exists();
        if !taken {
            break (branch, path);
        }
        n += 1;
        if n > TRIES {
            return Added::Full;
        }
    };
    let text = path.to_string_lossy().into_owned();
    if text.chars().any(char::is_control) || text.len() > 4096 {
        return Added::BadPath;
    }
    match git.run(dir, &["worktree", "add", "-b", &branch, "--", &text]) {
        Some(r) if r.ok => Added::Made(Worktree { branch, path: text }),
        Some(r) => Added::Failed(said(&r.err)),
        None => Added::NoGit,
    }
}

/// The worktrees of the repository folder `dir` is in, other than its
/// main one, with git `program` (Start <agent> in an existing worktree).
pub fn list(program: &Path, dir: &Path) -> Listed {
    let git = Git { program };
    if git.run(Path::new("."), &["--version"]).is_none() {
        return Listed::NoGit;
    }
    if !dir.is_dir() {
        return Listed::NoDir;
    }
    if git.toplevel(dir).is_none() {
        return Listed::NoRepo;
    }
    // -z (git 2.36 and later) keeps a folder name with a newline in it
    // whole, so it is left out rather than cut short.
    let (ran, nul) = match git.run(dir, &["worktree", "list", "--porcelain", "-z"]) {
        Some(r) if r.ok => (Some(r), true),
        _ => (git.run(dir, &["worktree", "list", "--porcelain"]), false),
    };
    match ran {
        Some(r) if r.ok => Listed::Found(
            crate::worktree::porcelain(&r.raw, nul)
                .into_iter()
                .map(|(branch, path)| (branch, native(&path).to_string_lossy().into_owned()))
                .filter(|(b, p)| {
                    valid_branch(b) && !p.chars().any(char::is_control) && p.len() <= 4096
                })
                .map(|(branch, path)| Worktree { branch, path })
                .collect(),
        ),
        Some(r) => Listed::Failed(said(&r.err)),
        None => Listed::NoGit,
    }
}

/// Whether no program works in folder `dir` or has a file in it open, so
/// Windows lets it be deleted: renaming it fails while one does. It is
/// renamed next to itself and straight back. `Err` with where it is when
/// it could not be renamed back.
#[cfg(windows)]
fn free(dir: &Path) -> Result<bool, PathBuf> {
    let mut probe = dir.as_os_str().to_owned();
    probe.push(".ferossh-in-use-check");
    let probe = PathBuf::from(probe);
    // Gone already: `remove` says so.
    if !dir.is_dir() {
        return Ok(true);
    }
    if probe.exists() || std::fs::rename(dir, &probe).is_err() {
        return Ok(false);
    }
    for _ in 0..50 {
        if std::fs::rename(&probe, dir).is_ok() {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(probe)
}

/// [`remove`], once the programs of an agent that was just stopped let go
/// of its folder (Windows does not wait for them, and cannot delete a
/// folder one still works in), waiting at most `wait`. When they still
/// hold it then, git does not run (it would delete part of the folder).
pub fn remove_when_free(program: &Path, w: &Worktree, wait: std::time::Duration) -> Removal {
    #[cfg(windows)]
    {
        let start = std::time::Instant::now();
        loop {
            match free(Path::new(&w.path)) {
                Ok(true) => break,
                Ok(false) if start.elapsed() < wait => {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                // git would delete what it can and stop half way.
                Ok(false) => {
                    return Removal::Failed(
                        "a program still works in the folder or has a file in it open, so \
                         nothing was removed; close it and try again"
                            .to_owned(),
                    );
                }
                Err(left) => {
                    return Removal::Failed(format!(
                        "Windows did not let FeroSSH give the folder its name back, so it \
                         is now {}: rename it back to {}",
                        crate::ssh::sanitize_line(&left.to_string_lossy()),
                        crate::ssh::sanitize_line(&w.path)
                    ));
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = wait;
    remove(program, w)
}

/// Removes agent worktree `w` with git `program`, as [`crate::worktree::
/// remove_command`] does on a server.
pub fn remove(program: &Path, w: &Worktree) -> Removal {
    if !valid_branch(&w.branch) {
        return Removal::Other;
    }
    let git = Git { program };
    if git.run(Path::new("."), &["--version"]).is_none() {
        return Removal::NoGit;
    }
    let dir = Path::new(&w.path);
    if !dir.is_dir() {
        return Removal::NoDir;
    }
    let Some(top) = git.toplevel(dir) else {
        return Removal::NoRepo;
    };
    let main = git.main_worktree(dir, &top);
    if same(&top, &main) {
        return Removal::Main;
    }
    // Only the worktree that has this branch checked out.
    let list = git
        .run(dir, &["worktree", "list", "--porcelain"])
        .map(|r| r.out)
        .unwrap_or_default();
    let mut at: Option<PathBuf> = None;
    let mut holder = None;
    let wanted = format!("branch refs/heads/{}", w.branch);
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            at = Some(native(p));
        } else if line == wanted {
            holder = at.clone();
            break;
        }
    }
    if !holder.is_some_and(|h| same(&h, &top)) {
        return Removal::Other;
    }
    let top_text = top.to_string_lossy().into_owned();
    match git.run(&main, &["worktree", "remove", "--", &top_text]) {
        Some(r) if r.ok => {}
        Some(r) => return Removal::Kept(said(&r.err)),
        None => return Removal::Failed(String::new()),
    }
    let deleted = git
        .run(&main, &["branch", "-d", &w.branch])
        .is_some_and(|r| r.ok);
    Removal::Removed {
        branch_deleted: deleted,
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn git_path() -> Option<PathBuf> {
        let ok = Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        ok.then(|| PathBuf::from("git"))
    }

    fn sh(dir: &Path, script: &str) {
        let mut command = Command::new("sh");
        for name in GIT_LOCATION_VARS {
            command.env_remove(name);
        }
        let s = command
            .arg("-c")
            .arg(script)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(s.success(), "{script}");
    }

    #[test]
    fn makes_and_removes_worktrees_like_the_server_does() {
        let Some(git) = git_path() else {
            eprintln!("git is not installed: skipped");
            return;
        };
        let root = std::env::temp_dir().join(format!("ferossh-lwt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("my app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        sh(
            &repo,
            "git init -q -b main . && echo a > f && git add f && git commit -qm one",
        );
        // From a subfolder: next to the main worktree.
        let Added::Made(w1) = add(&git, &repo.join("src"), "claude") else {
            panic!("not made");
        };
        assert_eq!(w1.branch, "claude");
        assert_eq!(Path::new(&w1.path), root.join("my app-claude"));
        // From inside that worktree: still next to the main one, -2.
        let Added::Made(w2) = add(&git, Path::new(&w1.path), "claude") else {
            panic!("not made");
        };
        assert_eq!(w2.branch, "claude-2");
        assert_eq!(Path::new(&w2.path), root.join("my app-claude-2"));
        assert_eq!(add(&git, &root.join("none"), "claude"), Added::NoDir);
        assert_eq!(add(&git, &root, "claude"), Added::NoRepo);
        // Both are listed for Start in an existing worktree, not the main.
        assert_eq!(
            list(&git, &repo.join("src")),
            Listed::Found(vec![w1.clone(), w2.clone()])
        );
        assert_eq!(list(&git, &root), Listed::NoRepo);
        // Untracked work is kept.
        std::fs::write(Path::new(&w1.path).join("new"), "x").unwrap();
        assert!(matches!(remove(&git, &w1), Removal::Kept(_)));
        assert!(Path::new(&w1.path).is_dir());
        // An unmerged branch stays; a merged one goes.
        std::fs::remove_file(Path::new(&w1.path).join("new")).unwrap();
        sh(
            Path::new(&w1.path),
            "echo b > g && git add g && git commit -qm two",
        );
        assert_eq!(
            remove(&git, &w1),
            Removal::Removed {
                branch_deleted: false
            }
        );
        assert!(!Path::new(&w1.path).exists());
        assert_eq!(
            remove(&git, &w2),
            Removal::Removed {
                branch_deleted: true
            }
        );
        // Never the main worktree, nor a folder holding another branch.
        let main = Worktree {
            branch: "main".into(),
            path: repo.to_string_lossy().into_owned(),
        };
        assert_eq!(remove(&git, &main), Removal::Main);
        let Added::Made(w3) = add(&git, &repo, "codex") else {
            panic!("not made");
        };
        let wrong = Worktree {
            branch: "other".into(),
            path: w3.path.clone(),
        };
        assert_eq!(remove(&git, &wrong), Removal::Other);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_git() {
        let missing = Path::new("/nonexistent/git");
        assert_eq!(add(missing, Path::new("/"), "claude"), Added::NoGit);
        let w = Worktree {
            branch: "claude".into(),
            path: "/tmp".into(),
        };
        assert_eq!(remove(missing, &w), Removal::NoGit);
    }
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used)]
mod windows_tests {
    use super::*;

    #[test]
    fn a_folder_is_free_only_once_nothing_holds_it() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("ferossh-free-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("held.txt"), b"x").unwrap();
        // As a program working in it holds it: a handle to the folder
        // that does not share delete (FILE_SHARE_READ | FILE_SHARE_WRITE,
        // FILE_FLAG_BACKUP_SEMANTICS to open a folder).
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .custom_flags(0x0200_0000)
            .open(&dir)
            .unwrap();
        assert_eq!(free(&dir), Ok(false));
        assert!(dir.is_dir());
        // Held past the wait: nothing runs, and the folder stays.
        let w = Worktree {
            branch: "claude".into(),
            path: dir.to_string_lossy().into_owned(),
        };
        let got = remove_when_free(Path::new("git"), &w, std::time::Duration::from_millis(300));
        assert!(matches!(got, Removal::Failed(_)), "{got:?}");
        assert!(dir.join("held.txt").is_file());
        drop(held);
        assert_eq!(free(&dir), Ok(true));
        assert!(dir.join("held.txt").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
