//! Session > Coding agents > Start <agent> in a new worktree: a coding
//! agent of its own git worktree and branch, so several agents can work
//! on one repository without stepping on each other's files.
//!
//! A fixed script on an exec channel of the pane's connection runs `git
//! worktree add -b <agent> ../<repo>-<agent>` next to the repository's
//! main worktree (`<agent>-2`, `-3`… when the name is taken), and the
//! agent then starts there in its own tmux window, as any agent does
//! ([`crate::agents`]). The window is marked with [`OPTION`], which
//! holds the branch and the folder, so the dashboard and Remove worktree
//! know them on any later attach, from any PC.
//!
//! Remove worktree runs `git worktree remove` (which keeps a worktree
//! with uncommitted or untracked changes) and then `git branch -d` (which
//! keeps a branch that is not merged yet), so committed and uncommitted
//! work stays. Files git ignores (build output, `node_modules`, a local
//! `.env`) go with the folder, as the confirmation says.
//!
//! The folder and names go on the scripts' standard input, never into
//! the command, so nothing in them reaches a shell as code. What comes
//! back is read as data.

/// The tmux window option that holds an agent worktree's branch and
/// folder, as [`option_value`] writes them.
pub const OPTION: &str = "@ferossh-worktree";

/// The mark each line of the scripts' answers starts with.
const MARK: &str = "@ferossh-worktree";

/// Branches tried at most for one agent: `claude`, `claude-2` … `claude-99`.
const TRIES: u32 = 99;

/// The longest branch name read back (an existing worktree's).
const MAX_BRANCH: usize = 100;

/// The longest branch name made for an agent, before `-99`.
const MAX_BASE: usize = 37;

/// The longest folder read back.
const MAX_PATH: usize = 4096;

/// An agent's worktree: its branch and its folder on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub branch: String,
    pub path: String,
}

/// Whether `name` is a branch name this module makes: letters, digits,
/// `.`, `_` and `-`, starting with a letter or digit, without `..`, not
/// ending in `.` or `.lock`. Such a name needs no quoting anywhere and
/// git takes it.
pub fn valid_branch(name: &str) -> bool {
    name.len() <= MAX_BRANCH
        && name.split('/').all(|part| {
            part.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                && !part.ends_with('.')
                && !part.ends_with(".lock")
        })
        && !name.contains("..")
}

/// Whether `path` is a folder this module uses: absolute, on one line,
/// without control characters.
fn valid_path(path: &str) -> bool {
    path.starts_with('/') && path.len() <= MAX_PATH && !path.chars().any(char::is_control)
}

/// The branch an agent named `agent` ([`crate::tmux::agent_name`]) gets
/// first: its name made into a [`valid_branch`], or `agent`.
pub fn branch_base(agent: &str) -> String {
    let mut name: String = agent
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    while name.contains("..") {
        name = name.replace("..", ".");
    }
    let name = name.trim_start_matches(['.', '_', '-']);
    // Room for `-99`.
    let mut name: String = name.chars().take(MAX_BASE).collect();
    loop {
        if let Some(rest) = name.strip_suffix(".lock") {
            name = rest.to_owned();
        } else if name.ends_with('.') {
            name.pop();
        } else {
            break;
        }
    }
    if valid_branch(&name) {
        name
    } else {
        "agent".to_owned()
    }
}

/// The value of [`OPTION`] for `w`: the branch, a space, the folder
/// (which may hold spaces itself).
pub fn option_value(w: &Worktree) -> String {
    format!("{} {}", w.branch, w.path)
}

/// `value` as one tmux argument, whole: single-quoted, with a `'` closed,
/// escaped and opened again (`'\''`), which tmux joins up. tmux expands
/// nothing in it (`set-option` without `-F`). `value` is an
/// [`option_value`], so it has no control characters.
pub fn tmux_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// [`OPTION`] as read back: only what [`option_value`] writes, so a
/// value set by hand cannot put anything odd on the screen or into a
/// command.
pub fn parse_option(value: &str) -> Option<Worktree> {
    let (branch, path) = value.split_once(' ')?;
    (valid_branch(branch) && valid_path(path)).then(|| Worktree {
        branch: branch.to_owned(),
        path: path.to_owned(),
    })
}

/// The agents waiting to start in a new worktree, by the tmux pane whose
/// folder tmux was asked for, in the order asked: tmux answers in order,
/// so each answer goes with the oldest ask of its pane. `V` says what to
/// do with the folder (the agent command, by default).
#[derive(Debug)]
pub struct Asks<K, V = String> {
    by: std::collections::HashMap<K, std::collections::VecDeque<V>>,
}

impl<K, V> Default for Asks<K, V> {
    fn default() -> Self {
        Self {
            by: std::collections::HashMap::new(),
        }
    }
}

impl<K: Eq + std::hash::Hash + Copy, V> Asks<K, V> {
    /// A connection that never answers keeps only this many per pane.
    pub const MAX: usize = 16;

    /// Asks, with `ask`, for pane `key`'s folder to start agent `command`
    /// in. `None` when the pane has [`Self::MAX`] unanswered: then `ask`
    /// is not called, since an ask whose answer nobody waits for would
    /// shift every later answer. Nothing is kept when `ask` sends nothing.
    pub fn ask<A>(
        &mut self,
        key: K,
        command: impl Into<V>,
        ask: impl FnOnce() -> Vec<A>,
    ) -> Option<Vec<A>> {
        if self.by.get(&key).is_some_and(|q| q.len() >= Self::MAX) {
            return None;
        }
        let sent = ask();
        if !sent.is_empty() {
            self.by.entry(key).or_default().push_back(command.into());
        }
        Some(sent)
    }

    /// The agent command tmux's answer for pane `key` belongs to.
    pub fn answered(&mut self, key: K) -> Option<V> {
        let q = self.by.get_mut(&key)?;
        let command = q.pop_front();
        if q.is_empty() {
            self.by.remove(&key);
        }
        command
    }
}

/// The command that makes the worktree. The login shell only starts `sh`
/// with it, so csh and fish run it too; cmd.exe and PowerShell cannot.
/// One line (csh ends a quote at a line break), no single quote, no `!`
/// and no doubled backslash (fish reads those in single quotes).
pub fn add_command() -> Vec<u8> {
    let script = [
        "IFS= read -r d || exit 2".to_owned(),
        "IFS= read -r a || exit 2".to_owned(),
        format!("command -v git >/dev/null 2>&1 || {{ echo {MARK} nogit; exit 0; }}"),
        format!("cd -- \"$d\" 2>/dev/null || {{ echo {MARK} nodir; exit 0; }}"),
        format!(
            "t=$(git rev-parse --show-toplevel 2>/dev/null) && [ -n \"$t\" ] || \
             {{ echo {MARK} norepo; exit 0; }}"
        ),
        // Next to the main worktree, also when asked from another one:
        // never repo-claude-claude.
        "m=$(git worktree list --porcelain 2>/dev/null | sed -n \"1s/^worktree //p\")".to_owned(),
        "[ -n \"$m\" ] && [ -d \"$m\" ] || m=$t".to_owned(),
        "p=$(dirname -- \"$m\"); r=$(basename -- \"$m\"); b=$a; n=1".to_owned(),
        format!(
            "while git show-ref --verify --quiet \"refs/heads/$b\" || [ -e \"$p/$r-$b\" ]; do \
             n=$((n+1)); [ $n -le {TRIES} ] || {{ echo {MARK} full; exit 0; }}; b=$a-$n; done"
        ),
        "w=$p/$r-$b".to_owned(),
        // Only a folder the answer can carry back (see `valid_path`):
        // nothing is made for one that could not be read.
        format!("case $w in /*) ;; *) echo {MARK} badpath; exit 0;; esac"),
        // Byte by byte: the C0 controls and DEL, then the C1 controls
        // (U+0080 to U+009F, \302\200 to \302\237 in UTF-8), which
        // `char::is_control` refuses too.
        "LC_ALL=C; export LC_ALL".to_owned(),
        format!("case $w in *[[:cntrl:]]*) echo {MARK} badpath; exit 0;; esac"),
        "c=$(printf \"\\302[\\200-\\237]\")".to_owned(),
        format!("case $w in *$c*) echo {MARK} badpath; exit 0;; esac"),
        format!("[ ${{#w}} -le {MAX_PATH} ] || {{ echo {MARK} badpath; exit 0; }}"),
        // From the commit the asking pane has checked out.
        format!("git worktree add -b \"$b\" -- \"$w\" >&2 || {{ echo {MARK} failed; exit 0; }}"),
        format!("printf \"{MARK} made %s %s\\n\" \"$b\" \"$w\""),
    ]
    .join("; ");
    format!("exec sh -c '{script}'").into_bytes()
}

/// What goes on [`add_command`]'s standard input: the folder the agent
/// was asked from and the branch to start with; `None` for a folder whose
/// name could not go on one line.
pub fn add_input(dir: &[u8], base: &str) -> Option<Vec<u8>> {
    if dir.is_empty() || dir.iter().any(|&b| b == b'\n' || b == b'\r' || b == 0) {
        return None;
    }
    if !valid_branch(base) {
        return None;
    }
    let mut input = dir.to_vec();
    input.push(b'\n');
    input.extend_from_slice(base.as_bytes());
    input.push(b'\n');
    Some(input)
}

/// What [`add_command`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    Made(Worktree),
    /// The server has no `git`.
    NoGit,
    /// The folder is not there (any more), or cannot be entered.
    NoDir,
    /// The folder is in no git repository.
    NoRepo,
    /// Every branch name from `agent` to `agent-99` is taken.
    Full,
    /// The worktree's folder would have a control character in its name,
    /// or be too long: nothing was made.
    BadPath,
    /// git refused, or there was no answer: what it said, if anything.
    Failed(String),
}

/// The last line of `bytes` with something in it, made safe to show.
fn last_said(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().map(str::trim).rfind(|l| !l.is_empty());
    crate::ssh::sanitize_line(line.unwrap_or(""))
        .chars()
        .take(300)
        .collect()
}

/// The script's own answer in `out`: the rest of its last marked line.
fn answer(out: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(out);
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix(MARK)?.strip_prefix(' '))
        .map(|rest| rest.trim_end_matches('\r').to_owned())
}

/// What [`add_command`]'s standard output `out` and standard error `err`
/// mean.
pub fn parse_add(out: &[u8], err: &[u8]) -> Added {
    let Some(answer) = answer(out) else {
        return Added::Failed(last_said(err));
    };
    match answer.as_str() {
        "nogit" => Added::NoGit,
        "nodir" => Added::NoDir,
        "norepo" => Added::NoRepo,
        "full" => Added::Full,
        "badpath" => Added::BadPath,
        "failed" => Added::Failed(last_said(err)),
        other => other
            .strip_prefix("made ")
            .and_then(parse_option)
            .map_or_else(|| Added::Failed(last_said(err)), Added::Made),
    }
}

/// The command that lists the worktrees of the repository the folder on
/// its standard input is in, for Session > Coding agents > Start <agent>
/// in an existing worktree: `git worktree list --porcelain -z` (or,
/// with a git older than 2.36, without `-z`) after the answer line. The
/// answer is only given once git has listed them; when it cannot, its
/// error goes to standard error. Like [`add_command`], one line for `sh`.
pub fn list_command() -> Vec<u8> {
    let script = [
        "IFS= read -r d || exit 2".to_owned(),
        format!("command -v git >/dev/null 2>&1 || {{ echo {MARK} nogit; exit 0; }}"),
        format!("cd -- \"$d\" 2>/dev/null || {{ echo {MARK} nodir; exit 0; }}"),
        format!(
            "git rev-parse --show-toplevel >/dev/null 2>&1 || {{ echo {MARK} norepo; exit 0; }}"
        ),
        format!(
            "git worktree list --porcelain -z >/dev/null 2>&1 && \
             {{ echo {MARK} listz; exec git worktree list --porcelain -z; }}"
        ),
        format!(
            "git worktree list --porcelain >/dev/null 2>&1 || \
             {{ echo {MARK} failed; exec git worktree list --porcelain >&2; }}"
        ),
        format!("echo {MARK} list"),
        "exec git worktree list --porcelain".to_owned(),
    ]
    .join("; ");
    format!("exec sh -c '{script}'").into_bytes()
}

/// What goes on [`list_command`]'s standard input; `None` for a folder
/// whose name could not go on one line.
pub fn list_input(dir: &[u8]) -> Option<Vec<u8>> {
    if dir.is_empty() || dir.iter().any(|&b| b == b'\n' || b == b'\r' || b == 0) {
        return None;
    }
    let mut input = dir.to_vec();
    input.push(b'\n');
    Some(input)
}

/// What [`list_command`] (or git on this PC) found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listed {
    /// The repository's worktrees other than its main one, each with a
    /// branch checked out (maybe none).
    Found(Vec<Worktree>),
    NoGit,
    NoDir,
    NoRepo,
    Failed(String),
}

/// The worktrees in `git worktree list --porcelain` output `data` (with
/// `-z` when `nul`), as branch and folder: not the first (the main
/// worktree), nor one that is bare, has no branch checked out (detached)
/// or whose folder is gone (prunable). Folders are as git printed them;
/// one that is not UTF-8, or that git quoted (without `-z`, for a name
/// with a newline or such in it), is left out rather than changed.
pub fn porcelain(data: &[u8], nul: bool) -> Vec<(String, String)> {
    let sep = if nul { 0 } else { b'\n' };
    let mut found = Vec::new();
    let mut first = true;
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut skip = false;
    let mut end = |path: &mut Option<String>,
                   branch: &mut Option<String>,
                   skip: &mut bool,
                   first: &mut bool| {
        if let (false, false, Some(p), Some(b)) = (*first, *skip, path.take(), branch.take()) {
            found.push((b, p));
        }
        *path = None;
        *branch = None;
        *skip = false;
        *first = false;
    };
    for field in data.split(|&b| b == sep) {
        let field = if nul {
            field
        } else {
            field.strip_suffix(b"\r").unwrap_or(field)
        };
        if field.is_empty() {
            // A blank line ends an entry; so does the end of the output.
            if path.is_some() || skip || branch.is_some() {
                end(&mut path, &mut branch, &mut skip, &mut first);
            }
            continue;
        }
        let Ok(line) = std::str::from_utf8(field) else {
            if field.starts_with(b"worktree ") || field.starts_with(b"branch ") {
                skip = true;
            }
            continue;
        };
        if let Some(p) = line.strip_prefix("worktree ") {
            if path.is_some() {
                // No blank line between them: a new entry all the same.
                end(&mut path, &mut branch, &mut skip, &mut first);
            }
            if !nul && p.starts_with('"') {
                skip = true;
            }
            path = Some(p.to_owned());
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = Some(b.to_owned());
        } else if line == "bare" || line == "detached" || line.starts_with("prunable") {
            skip = true;
        }
    }
    end(&mut path, &mut branch, &mut skip, &mut first);
    found
}

/// What [`list_command`]'s standard output `out` and standard error
/// `err` mean. Only worktrees whose branch and folder FeroSSH can carry
/// (see [`OPTION`]) are kept.
pub fn parse_list(out: &[u8], err: &[u8]) -> Listed {
    let mark = format!("{MARK} ");
    let mut answer = None;
    let mut at = 0;
    for line in out.split(|&b| b == b'\n') {
        at += line.len() + 1;
        if let Some(a) = line.strip_prefix(mark.as_bytes()) {
            answer = Some(a.strip_suffix(b"\r").unwrap_or(a));
            break;
        }
    }
    let Some(answer) = answer else {
        return Listed::Failed(last_said(err));
    };
    let rest = out.get(at..).unwrap_or_default();
    let found = |nul| {
        Listed::Found(
            porcelain(rest, nul)
                .into_iter()
                .filter(|(b, p)| valid_branch(b) && valid_path(p))
                .map(|(branch, path)| Worktree { branch, path })
                .collect(),
        )
    };
    match answer {
        b"nogit" => Listed::NoGit,
        b"nodir" => Listed::NoDir,
        b"norepo" => Listed::NoRepo,
        b"listz" => found(true),
        b"list" => found(false),
        _ => Listed::Failed(last_said(err)),
    }
}

/// The command that removes a worktree: `git worktree remove`, which
/// refuses one with uncommitted or untracked changes, and then `git
/// branch -d`, which keeps a branch that is not merged. Like
/// [`add_command`], one line for `sh`.
pub fn remove_command() -> Vec<u8> {
    let script = [
        "IFS= read -r w || exit 2".to_owned(),
        "IFS= read -r b || exit 2".to_owned(),
        format!("command -v git >/dev/null 2>&1 || {{ echo {MARK} nogit; exit 0; }}"),
        format!("cd -- \"$w\" 2>/dev/null || {{ echo {MARK} nodir; exit 0; }}"),
        format!(
            "c=$(git rev-parse --show-toplevel 2>/dev/null) && [ -n \"$c\" ] || \
             {{ echo {MARK} norepo; exit 0; }}"
        ),
        "m=$(git worktree list --porcelain | sed -n \"1s/^worktree //p\")".to_owned(),
        format!("[ \"$c\" = \"$m\" ] && {{ echo {MARK} main; exit 0; }}"),
        // Only the worktree that has this branch checked out: a folder
        // reused for something else is left alone.
        "o=$(git worktree list --porcelain | while IFS= read -r l; do case $l in \
         \"worktree \"*) t=${l#worktree };; \"branch refs/heads/$b\") printf \"%s\" \"$t\"; break;; \
         esac; done)"
            .to_owned(),
        format!("[ \"$o\" = \"$c\" ] || {{ echo {MARK} other; exit 0; }}"),
        "cd -- \"$m\" || exit 2".to_owned(),
        format!("git worktree remove -- \"$c\" >&2 || {{ echo {MARK} kept; exit 0; }}"),
        format!(
            "if git branch -d \"$b\" >&2; then echo {MARK} removed deleted; \
             else echo {MARK} removed kept; fi"
        ),
    ]
    .join("; ");
    format!("exec sh -c '{script}'").into_bytes()
}

/// What goes on [`remove_command`]'s standard input; `None` for a
/// worktree this module did not make.
pub fn remove_input(w: &Worktree) -> Option<Vec<u8>> {
    (valid_branch(&w.branch) && valid_path(&w.path))
        .then(|| format!("{}\n{}\n", w.path, w.branch).into_bytes())
}

/// What [`remove_command`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// The folder went; `branch_deleted` when the branch went too (it
    /// was merged), else it stays with the agent's commits.
    Removed {
        branch_deleted: bool,
    },
    NoGit,
    /// The folder is not there any more.
    NoDir,
    NoRepo,
    /// The folder is the repository's main worktree: never removed.
    Main,
    /// The folder no longer holds the branch's worktree.
    Other,
    /// git kept it (changes not committed, or locked): what it said.
    Kept(String),
    Failed(String),
}

/// What [`remove_command`]'s output means.
pub fn parse_remove(out: &[u8], err: &[u8]) -> Removal {
    let Some(answer) = answer(out) else {
        return Removal::Failed(last_said(err));
    };
    match answer.as_str() {
        "nogit" => Removal::NoGit,
        "nodir" => Removal::NoDir,
        "norepo" => Removal::NoRepo,
        "main" => Removal::Main,
        "other" => Removal::Other,
        "kept" => Removal::Kept(last_said(err)),
        "removed deleted" => Removal::Removed {
            branch_deleted: true,
        },
        "removed kept" => Removal::Removed {
            branch_deleted: false,
        },
        _ => Removal::Failed(last_said(err)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn asks_wait_in_order_and_a_full_pane_asks_nothing() {
        let mut asks = Asks::<u32>::default();
        let mut sent = 0;
        for n in 0..Asks::<u32>::MAX {
            let a = asks.ask(1, format!("agent{n}"), || {
                sent += 1;
                vec![n]
            });
            assert_eq!(a, Some(vec![n]));
        }
        // Full: tmux is not asked, so no answer is left without its ask.
        assert_eq!(
            asks.ask(1, "late", || -> Vec<usize> { unreachable!() }),
            None
        );
        assert_eq!(sent, Asks::<u32>::MAX);
        // Another pane still asks.
        assert_eq!(asks.ask(2, "other", || vec![0]), Some(vec![0]));
        // Nothing sent, nothing kept.
        assert_eq!(asks.ask(3, "none", Vec::<u8>::new), Some(Vec::new()));
        assert_eq!(asks.answered(3), None);
        // The later answers go with their own asks, oldest first.
        assert_eq!(asks.answered(1).as_deref(), Some("agent0"));
        assert_eq!(asks.ask(1, "now", || vec![0]), Some(vec![0]));
        for n in 1..Asks::<u32>::MAX {
            assert_eq!(asks.answered(1), Some(format!("agent{n}")));
        }
        assert_eq!(asks.answered(1).as_deref(), Some("now"));
        assert_eq!(asks.answered(1), None);
        assert_eq!(asks.answered(2).as_deref(), Some("other"));
    }

    #[test]
    fn existing_worktrees_are_read_from_porcelain() {
        let text = "worktree /srv/app\nHEAD 1\nbranch refs/heads/main\n\n\
                    worktree /srv/app-claude\nHEAD 2\nbranch refs/heads/claude\n\n\
                    worktree /srv/my app\nHEAD 3\nbranch refs/heads/feature/x\nlocked\n\n\
                    worktree /srv/app-d\nHEAD 4\ndetached\n\n\
                    worktree /srv/gone\nHEAD 5\nbranch refs/heads/gone\nprunable gitdir file points to non-existent location\n";
        assert_eq!(
            porcelain(text.as_bytes(), false),
            [
                ("claude".to_owned(), "/srv/app-claude".to_owned()),
                ("feature/x".to_owned(), "/srv/my app".to_owned())
            ]
        );
        let mut out = b"noise\n@ferossh-worktree list\n".to_vec();
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(
            b"\nworktree relative\nbranch refs/heads/r\n\nworktree /x\nbranch refs/heads/a b\n",
        );
        assert_eq!(
            parse_list(&out, b""),
            Listed::Found(vec![
                Worktree {
                    branch: "claude".into(),
                    path: "/srv/app-claude".into()
                },
                Worktree {
                    branch: "feature/x".into(),
                    path: "/srv/my app".into()
                },
            ])
        );
        assert_eq!(
            parse_list(b"@ferossh-worktree norepo\n", b""),
            Listed::NoRepo
        );
        assert_eq!(
            parse_list(b"", b"sh: 1: boom\n"),
            Listed::Failed("sh: 1: boom".into())
        );
        // git failed after all: its error, not an empty list.
        assert_eq!(
            parse_list(
                b"@ferossh-worktree failed\n",
                b"fatal: not a git repository\n"
            ),
            Listed::Failed("fatal: not a git repository".into())
        );
        // With -z: a folder with a newline in it is whole, so left out;
        // one with spaces stays; one that is not UTF-8 is left out, not
        // changed.
        let z = b"worktree /srv/app\0HEAD 1\0branch refs/heads/main\0\0\
                  worktree /srv/a\nb\0HEAD 2\0branch refs/heads/nl\0\0\
                  worktree /srv/my app\0HEAD 3\0branch refs/heads/sp\0\0\
                  worktree /srv/\xff\0HEAD 4\0branch refs/heads/bad\0\0";
        let mut out = b"@ferossh-worktree listz\n".to_vec();
        out.extend_from_slice(z);
        assert_eq!(
            parse_list(&out, b""),
            Listed::Found(vec![Worktree {
                branch: "sp".into(),
                path: "/srv/my app".into()
            }])
        );
        // Without -z, a name git had to quote is left out too.
        let mut out = b"@ferossh-worktree list\n".to_vec();
        out.extend_from_slice(
            b"worktree /srv/app\nbranch refs/heads/main\n\n\
              worktree \"/srv/a\\nb\"\nbranch refs/heads/nl\n\n\
              worktree /srv/\xff\nbranch refs/heads/bad\n",
        );
        assert_eq!(parse_list(&out, b""), Listed::Found(Vec::new()));
        assert_eq!(list_input(b"/a\nb"), None);
        assert_eq!(list_input(b"/a b").unwrap(), b"/a b\n");
    }

    #[test]
    fn branch_names_are_plain() {
        assert_eq!(branch_base("claude"), "claude");
        assert_eq!(branch_base("aider.py"), "aider.py");
        assert_eq!(branch_base("-x"), "x");
        assert_eq!(branch_base(".hidden"), "hidden");
        assert_eq!(branch_base("a..b"), "a.b");
        assert_eq!(branch_base("x.lock"), "x");
        assert_eq!(branch_base("x."), "x");
        assert_eq!(branch_base("..."), "agent");
        assert_eq!(branch_base(""), "agent");
        assert_eq!(branch_base("rm -rf /"), "rm-rf");
        assert!(branch_base(&"a".repeat(80)).len() <= MAX_BASE);
        for bad in [
            "", "-a", ".a", "a..b", "a.", "a.lock", "a b", "a;b", "a$b", "/a", "a/", "a//b",
            "a/.b", "a/-b", "a.lock/b",
        ] {
            assert!(!valid_branch(bad), "{bad}");
        }
        // An existing worktree's branch may have folders.
        assert!(valid_branch("feature/login-2"));
        assert!(!valid_branch(&"a".repeat(MAX_BRANCH + 1)));
    }

    #[test]
    fn the_option_holds_branch_and_folder() {
        let w = Worktree {
            branch: "claude-2".into(),
            path: "/home/me/my repo-claude-2".into(),
        };
        assert_eq!(parse_option(&option_value(&w)), Some(w));
        assert_eq!(parse_option("claude"), None);
        assert_eq!(parse_option("claude relative/path"), None);
        assert_eq!(parse_option("-x /tmp"), None);
        assert_eq!(parse_option("x /tmp/a\x1b[2Jb"), None);
        assert_eq!(parse_option(""), None);
    }

    #[test]
    fn tmux_quoting_keeps_apostrophes() {
        assert_eq!(tmux_quote("b /srv/it's"), "'b /srv/it'\\''s'");
    }

    #[test]
    fn inputs_are_lines() {
        assert_eq!(
            add_input(b"/srv/a b", "claude").unwrap(),
            b"/srv/a b\nclaude\n"
        );
        assert_eq!(add_input(b"/srv/a\nb", "claude"), None);
        assert_eq!(add_input(b"", "claude"), None);
        assert_eq!(add_input(b"/srv", "-x"), None);
        let w = Worktree {
            branch: "x".into(),
            path: "/a\nb".into(),
        };
        assert_eq!(remove_input(&w), None);
    }

    #[test]
    fn commands_survive_csh_and_fish() {
        for c in [add_command(), remove_command(), list_command()] {
            let c = String::from_utf8(c).unwrap();
            let inner = c
                .strip_prefix("exec sh -c '")
                .and_then(|s| s.strip_suffix('\''))
                .unwrap();
            // fish reads only \' and \\ in single quotes.
            assert!(!inner.contains(['\'', '!', '\n']), "{inner}");
            assert!(!inner.contains("\\\\"), "{inner}");
        }
    }

    #[test]
    fn answers_are_read() {
        assert_eq!(
            parse_add(
                b"noise\n@ferossh-worktree made claude-2 /srv/a b-claude-2\n",
                b""
            ),
            Added::Made(Worktree {
                branch: "claude-2".into(),
                path: "/srv/a b-claude-2".into()
            })
        );
        assert_eq!(parse_add(b"@ferossh-worktree norepo\n", b""), Added::NoRepo);
        assert_eq!(
            parse_add(b"@ferossh-worktree failed\n", b"fatal: bad\x1b[31m\n"),
            Added::Failed(crate::ssh::sanitize_line("fatal: bad\x1b[31m"))
        );
        assert_eq!(
            parse_add(b"@ferossh-worktree made ../x /y\n", b""),
            Added::Failed(String::new())
        );
        assert_eq!(
            parse_add(b"", b"sh: not found"),
            Added::Failed("sh: not found".into())
        );
        assert_eq!(
            parse_remove(b"@ferossh-worktree removed kept\n", b""),
            Removal::Removed {
                branch_deleted: false
            }
        );
        assert_eq!(
            parse_remove(b"@ferossh-worktree main\n", b""),
            Removal::Main
        );
    }

    #[cfg(unix)]
    fn run(command: Vec<u8>, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        use std::io::Write;
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(String::from_utf8(command).unwrap())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.stdout, out.stderr)
    }

    #[cfg(unix)]
    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    #[cfg(unix)]
    fn add(dir: &std::path::Path, base: &str) -> Added {
        let input = add_input(dir.as_os_str().as_encoded_bytes(), base).unwrap();
        let (out, err) = run(add_command(), &input);
        parse_add(&out, &err)
    }

    #[cfg(unix)]
    fn remove(w: &Worktree) -> Removal {
        let (out, err) = run(remove_command(), &remove_input(w).unwrap());
        parse_remove(&out, &err)
    }

    #[cfg(unix)]
    #[test]
    fn worktrees_are_made_and_removed_in_a_real_repository() {
        let root = std::env::temp_dir().join(format!("ferossh-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("my repo $x");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "one"]);
        // Asked from a subfolder: next to the main worktree.
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        let Added::Made(first) = add(&repo.join("src/deep"), "claude") else {
            panic!("not made");
        };
        let canon = std::fs::canonicalize(&root).unwrap();
        assert_eq!(first.branch, "claude");
        assert_eq!(
            first.path,
            canon.join("my repo $x-claude").to_str().unwrap()
        );
        assert!(std::path::Path::new(&first.path).join("a.txt").is_file());
        // The name is taken: the next one; asked from inside a worktree,
        // still next to the main one.
        let Added::Made(second) = add(std::path::Path::new(&first.path), "claude") else {
            panic!("not made");
        };
        assert_eq!(second.branch, "claude-2");
        assert_eq!(
            second.path,
            canon.join("my repo $x-claude-2").to_str().unwrap()
        );
        // A folder name that could not come back: nothing is made.
        let tab = root.join("tab\there");
        std::fs::create_dir_all(&tab).unwrap();
        git(&tab, &["init", "-q"]);
        std::fs::write(tab.join("a"), "a").unwrap();
        git(&tab, &["add", "."]);
        git(&tab, &["commit", "-qm", "one"]);
        assert_eq!(add(&tab, "claude"), Added::BadPath);
        assert!(git(&tab, &["branch", "--list", "claude"]).trim().is_empty());
        assert!(!root.join("tab\there-claude").exists());
        // Nor one with a C1 control (U+0085, next line) in it.
        let nel = root.join("nel\u{85}here");
        std::fs::create_dir_all(&nel).unwrap();
        git(&nel, &["init", "-q"]);
        std::fs::write(nel.join("a"), "a").unwrap();
        git(&nel, &["add", "."]);
        git(&nel, &["commit", "-qm", "one"]);
        assert_eq!(add(&nel, "claude"), Added::BadPath);
        assert!(git(&nel, &["branch", "--list", "claude"]).trim().is_empty());
        assert!(!root.join("nel\u{85}here-claude").exists());
        // Start in an existing worktree lists both, from any folder of
        // the repository, but not the main one.
        let list = |dir: &std::path::Path| {
            let (out, err) = run(
                list_command(),
                &list_input(dir.as_os_str().as_encoded_bytes()).unwrap(),
            );
            parse_list(&out, &err)
        };
        // One whose folder has a newline in it is left out.
        let nl = root.join("nl\nhere");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "nl", nl.to_str().unwrap()],
        );
        assert_eq!(
            list(&repo.join("src")),
            Listed::Found(vec![first.clone(), second.clone()])
        );
        git(&repo, &["worktree", "remove", nl.to_str().unwrap()]);
        assert_eq!(list(&root), Listed::NoRepo);
        assert_eq!(list(&root.join("nope")), Listed::NoDir);
        // Not a repository, not a folder.
        assert_eq!(add(&root, "claude"), Added::NoRepo);
        assert_eq!(add(&root.join("nope"), "claude"), Added::NoDir);

        // Untracked work is kept, worktree and branch.
        std::fs::write(std::path::Path::new(&second.path).join("new.txt"), "x").unwrap();
        assert!(matches!(remove(&second), Removal::Kept(_)));
        assert!(std::path::Path::new(&second.path).is_dir());
        // Committed but not merged: the folder goes, the branch stays.
        git(std::path::Path::new(&second.path), &["add", "."]);
        git(
            std::path::Path::new(&second.path),
            &["commit", "-qm", "two"],
        );
        assert_eq!(
            remove(&second),
            Removal::Removed {
                branch_deleted: false
            }
        );
        assert!(!std::path::Path::new(&second.path).exists());
        assert!(git(&repo, &["branch", "--list", "claude-2"]).contains("claude-2"));
        // Nothing new on it: the branch goes too.
        assert_eq!(
            remove(&first),
            Removal::Removed {
                branch_deleted: true
            }
        );
        assert!(
            git(&repo, &["branch", "--list", "claude"])
                .trim()
                .is_empty()
        );
        assert_eq!(remove(&first), Removal::NoDir);
        // The main worktree is never removed, nor a worktree that holds
        // another branch now.
        let main = Worktree {
            branch: "main".into(),
            path: canon.join("my repo $x").to_str().unwrap().into(),
        };
        assert_eq!(remove(&main), Removal::Main);
        let Added::Made(third) = add(&repo, "codex") else {
            panic!("not made");
        };
        let wrong = Worktree {
            branch: "claude-2".into(),
            path: third.path.clone(),
        };
        assert_eq!(remove(&wrong), Removal::Other);
        assert!(std::path::Path::new(&third.path).is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }
}
