//! Session > Coding agents > Show what changed: the files changed in the
//! git repository a pane is in (what a coding agent did there), shown
//! side by side in a tab of their own.
//!
//! A fixed script on an exec channel of the pane's connection runs `git
//! diff` against HEAD, then shows each untracked file as new. The folder
//! goes on the script's standard input, never into the command, so
//! nothing in it reaches a shell as code. What comes back is the
//! server's text: [`parse`] reads it as data, and every character shown
//! is made visible first ([`clean`]), so nothing in a file can steer the
//! terminal.

use unicode_width::UnicodeWidthChar;

/// The output kept at most; past it the tab says it shows only part.
pub const KEEP: usize = 8 * 1024 * 1024;
/// Rows shown at most, well within the terminal's scrollback.
pub const MAX_ROWS: usize = 200_000;
/// Columns a tab advances to.
const TAB: usize = 4;
/// Untracked files shown at most, and the largest shown (bytes).
const UNTRACKED: usize = 500;
const BIG: usize = 1024 * 1024;

/// The command run on the server. The login shell only starts `sh` with
/// it, so csh and fish run it too; cmd.exe and PowerShell cannot. One
/// line (csh ends a quote at a line break), no single quote inside and
/// no `!`.
pub fn command() -> Vec<u8> {
    let git = "git -c core.quotepath=off --no-pager";
    let opts = "--no-color --no-ext-diff --no-textconv --src-prefix=a/ --dst-prefix=b/";
    let script = [
        "IFS= read -r d || exit 2".to_owned(),
        "command -v git >/dev/null 2>&1 || { echo @ferossh-diff nogit; exit 0; }".to_owned(),
        "cd -- \"$d\" 2>/dev/null || { echo @ferossh-diff nodir; exit 0; }".to_owned(),
        "t=$(git rev-parse --show-toplevel 2>/dev/null) || { echo @ferossh-diff norepo; exit 0; }"
            .to_owned(),
        "cd -- \"$t\" || exit 2".to_owned(),
        "printf \"@ferossh-diff top %s\\n\" \"$t\"; e=0".to_owned(),
        // The commit is looked up once: one an agent makes meanwhile
        // neither renames the heading nor moves what is compared.
        format!(
            "if c=$(git rev-parse -q --verify HEAD^{{commit}}); then \
             h=$(git log -1 --format=%h%x20%s \"$c\" --) || e=1; \
             printf \"@ferossh-diff head %s\\n\" \"$h\"; {git} diff {opts} -M \"$c\" -- || e=1; \
             else echo @ferossh-diff nohead; {git} diff {opts} --cached -- || e=1; \
             {git} diff {opts} -- || e=1; fi"
        ),
        // A name git had to quote (a line break or a quote in it) is left
        // out: it could not be read back here.
        // The list is read first so that a failing ls-files counts.
        format!(
            "u=$({git} ls-files --others --exclude-standard) || {{ u=; e=1; }}; \
             [ -z \"$u\" ] || printf \"%s\\n\" \"$u\" | head -n {UNTRACKED} | \
             while IFS= read -r f; do case $f in \\\"*) continue;; esac; \
             [ -f \"$f\" ] || continue; \
             if [ \"$(wc -c < \"$f\")\" -gt {BIG} ]; then printf \"@ferossh-diff big %s\\n\" \"$f\"; \
             else {git} diff {opts} --no-index -- /dev/null \"$f\"; \
             [ $? -le 1 ] || echo @ferossh-diff failed; fi; done"
        ),
        // git diff --no-index says 1 for "they differ"; more is an error.
        "[ $e = 0 ] || echo @ferossh-diff failed".to_owned(),
        "exit 0".to_owned(),
    ]
    .join("; ");
    format!("exec sh -c '{script}'").into_bytes()
}

/// What goes on the command's standard input: the folder, on one line;
/// `None` for a folder whose name could not go on one.
pub fn input(dir: &[u8]) -> Option<Vec<u8>> {
    if dir.is_empty() || dir.iter().any(|&b| b == b'\n' || b == b'\r' || b == 0) {
        return None;
    }
    let mut line = dir.to_vec();
    line.push(b'\n');
    Some(line)
}

/// What the command found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The server has no `git`.
    NoGit,
    /// The folder is not there (any more), or cannot be entered.
    NoDir,
    /// The folder is in no git repository.
    NoRepo,
    Changes(Diff),
}

/// The changes in one repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// The repository's top folder.
    pub top: String,
    /// The commit compared against, as short hash and subject; `None`
    /// before the first commit.
    pub head: Option<String>,
    pub files: Vec<File>,
    /// A git command failed: what shows may be missing files.
    pub failed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Modified,
    Added,
    Deleted,
    Renamed,
}

impl Kind {
    fn letter(self) -> (&'static str, &'static str) {
        match self {
            Self::Modified => ("M", "33"),
            Self::Added => ("A", "32"),
            Self::Deleted => ("D", "31"),
            Self::Renamed => ("R", "36"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Its name before and after (the same unless renamed), shown as is.
    pub old: String,
    pub new: String,
    pub kind: Kind,
    /// Git saw binary content, or the file was too big to show.
    pub note: Option<&'static str>,
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
}

impl File {
    fn new(old: String, new: String) -> Self {
        Self {
            old,
            new,
            kind: Kind::Modified,
            note: None,
            hunks: Vec::new(),
            added: 0,
            removed: 0,
        }
    }

    /// The name to show.
    pub fn name(&self) -> &str {
        if self.kind == Kind::Deleted {
            &self.old
        } else {
            &self.new
        }
    }
}

/// One `@@` part: rows side by side, a removed line next to the line
/// that took its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub new_start: usize,
    /// What git put after the `@@` (usually the function), cleaned.
    pub heading: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Line number and text before, and after; a context line has both
    /// and the same text.
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    /// Lines that changed (removed, added, or one in place of the other).
    pub changed: bool,
}

/// A line's number and text.
type Numbered = (usize, String);

/// Reads the command's output.
pub fn parse(out: &[u8]) -> Outcome {
    let text = String::from_utf8_lossy(out);
    let mut diff = Diff::default();
    let mut file: Option<File> = None;
    let mut hunk: Option<Hunk> = None;
    // Lines still to come in the hunk, before and after.
    let (mut left, mut right) = (0usize, 0usize);
    let (mut old_no, mut new_no) = (0usize, 0usize);
    let (mut minus, mut plus): (Vec<Numbered>, Vec<Numbered>) = (Vec::new(), Vec::new());
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(h) = hunk.as_mut()
            && (left > 0 || right > 0)
        {
            match line.as_bytes().first() {
                Some(b' ') | None if left > 0 && right > 0 => {
                    pair(h, &mut minus, &mut plus);
                    let t = line.get(1..).unwrap_or("").to_owned();
                    h.rows.push(Row {
                        left: Some((old_no, t.clone())),
                        right: Some((new_no, t)),
                        changed: false,
                    });
                    old_no += 1;
                    new_no += 1;
                    left -= 1;
                    right -= 1;
                    continue;
                }
                Some(b'-') if left > 0 => {
                    minus.push((old_no, line[1..].to_owned()));
                    if let Some(f) = file.as_mut() {
                        f.removed += 1;
                    }
                    old_no += 1;
                    left -= 1;
                    continue;
                }
                Some(b'+') if right > 0 => {
                    plus.push((new_no, line[1..].to_owned()));
                    if let Some(f) = file.as_mut() {
                        f.added += 1;
                    }
                    new_no += 1;
                    right -= 1;
                    continue;
                }
                Some(b'\\') => continue,
                // Not a hunk line after all: the counts were off.
                _ => {
                    left = 0;
                    right = 0;
                }
            }
        }
        if line.starts_with('\\') {
            continue;
        }
        // The hunk (if any) is complete.
        if let Some(mut h) = hunk.take() {
            pair(&mut h, &mut minus, &mut plus);
            if let Some(f) = file.as_mut() {
                f.hunks.push(h);
            }
        }
        if let Some(rest) = line.strip_prefix("@ferossh-diff ") {
            let (word, arg) = rest.split_once(' ').unwrap_or((rest, ""));
            match word {
                "nogit" => return Outcome::NoGit,
                "nodir" => return Outcome::NoDir,
                "norepo" => return Outcome::NoRepo,
                "top" => diff.top = clean(arg),
                "head" => diff.head = Some(clean(arg)),
                "failed" => diff.failed = true,
                "big" => {
                    flush(&mut diff, &mut file);
                    let mut f = File::new(String::new(), clean(arg));
                    f.kind = Kind::Added;
                    f.note = Some("New file, too big to show here.");
                    diff.files.push(f);
                }
                _ => {}
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(&mut diff, &mut file);
            let (a, b) = git_names(rest);
            file = Some(File::new(a, b));
            continue;
        }
        let Some(f) = file.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("@@ ") {
            let Some((ranges, heading)) = rest.split_once(" @@") else {
                continue;
            };
            let mut parts = ranges.split(' ');
            let (Some(a), Some(b)) = (
                parts
                    .next()
                    .and_then(|p| p.strip_prefix('-'))
                    .and_then(range),
                parts
                    .next()
                    .and_then(|p| p.strip_prefix('+'))
                    .and_then(range),
            ) else {
                continue;
            };
            (old_no, left) = a;
            (new_no, right) = b;
            // An empty side starts "at" the line before.
            if left == 0 {
                old_no += 1;
            }
            if right == 0 {
                new_no += 1;
            }
            hunk = Some(Hunk {
                old_start: old_no,
                new_start: new_no,
                heading: clean(heading.trim()),
                rows: Vec::new(),
            });
        } else if let Some(name) = line.strip_prefix("--- ") {
            if let Some(n) = side_name(name, "a/") {
                f.old = n;
            }
        } else if let Some(name) = line.strip_prefix("+++ ") {
            match side_name(name, "b/") {
                Some(n) => f.new = n,
                None => f.kind = Kind::Deleted,
            }
        } else if line.starts_with("new file mode") {
            f.kind = Kind::Added;
        } else if line.starts_with("deleted file mode") {
            f.kind = Kind::Deleted;
        } else if let Some(n) = line.strip_prefix("rename from ") {
            f.old = clean(&unquote(n));
            f.kind = Kind::Renamed;
        } else if let Some(n) = line.strip_prefix("rename to ") {
            f.new = clean(&unquote(n));
            f.kind = Kind::Renamed;
        } else if line.starts_with("Binary files ") {
            f.note = Some("Binary file: FeroSSH shows no lines of it.");
        }
    }
    if let Some(mut h) = hunk.take() {
        pair(&mut h, &mut minus, &mut plus);
        if let Some(f) = file.as_mut() {
            f.hunks.push(h);
        }
    }
    flush(&mut diff, &mut file);
    Outcome::Changes(diff)
}

/// Puts the removed and added lines collected so far side by side.
fn pair(h: &mut Hunk, minus: &mut Vec<Numbered>, plus: &mut Vec<Numbered>) {
    let n = minus.len().max(plus.len());
    let mut m = minus.drain(..);
    let mut p = plus.drain(..);
    for _ in 0..n {
        h.rows.push(Row {
            left: m.next(),
            right: p.next(),
            changed: true,
        });
    }
}

fn flush(diff: &mut Diff, file: &mut Option<File>) {
    if let Some(f) = file.take() {
        diff.files.push(f);
    }
}

/// `12,3` or `12` (one line) as start and count.
fn range(s: &str) -> Option<(usize, usize)> {
    match s.split_once(',') {
        Some((a, n)) => Some((a.parse().ok()?, n.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// The names on a `diff --git a/x b/y` line, as a fallback for files that
/// have no `---` and `+++` lines (a mode change, an empty new file).
fn git_names(rest: &str) -> (String, String) {
    if rest.starts_with('"') {
        // Quoted names: the first ends at its closing quote.
        let mut escaped = false;
        let end = rest
            .char_indices()
            .skip(1)
            .find(|&(_, c)| {
                let quote = c == '"' && !escaped;
                escaped = c == '\\' && !escaped;
                quote
            })
            .map_or(rest.len(), |(i, _)| i + 1);
        let a = unquote(&rest[..end]);
        let b = unquote(rest[end..].trim_start());
        let strip = |s: &str, p: &str| s.strip_prefix(p).unwrap_or(s).to_owned();
        return (clean(&strip(&a, "a/")), clean(&strip(&b, "b/")));
    }
    // The same name twice: `a/<n> b/<n>`.
    let len = rest.len();
    if len % 2 == 1 && len > 4 {
        let half = (len - 1) / 2;
        if let (Some(a), Some(b)) = (rest.get(..half), rest.get(half + 1..))
            && let (Some(x), Some(y)) = (a.strip_prefix("a/"), b.strip_prefix("b/"))
            && x == y
        {
            return (clean(x), clean(y));
        }
    }
    let (a, b) = rest.split_once(" b/").unwrap_or((rest, ""));
    (clean(a.strip_prefix("a/").unwrap_or(a)), clean(b))
}

/// A `---` or `+++` name without its `a/` or `b/`; `None` for /dev/null.
fn side_name(name: &str, prefix: &str) -> Option<String> {
    // Git ends a name with a space in it with a tab.
    let name = name.strip_suffix('\t').unwrap_or(name);
    if name == "/dev/null" {
        return None;
    }
    let name = unquote(name);
    Some(clean(name.strip_prefix(prefix).unwrap_or(&name)))
}

/// A name git put in double quotes, C-style, read back; others as they
/// are.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_owned();
    };
    let mut bytes = Vec::new();
    let mut it = inner.bytes().peekable();
    while let Some(b) = it.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match it.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'a') => bytes.push(7),
            Some(b'b') => bytes.push(8),
            Some(b'f') => bytes.push(12),
            Some(b'r') => bytes.push(b'\r'),
            Some(b'v') => bytes.push(11),
            Some(d @ b'0'..=b'3') => {
                let mut v = u32::from(d - b'0');
                for _ in 0..2 {
                    match it.peek() {
                        Some(&o @ b'0'..=b'7') => {
                            v = v * 8 + u32::from(o - b'0');
                            it.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(u8::try_from(v).unwrap_or(b'?'));
            }
            Some(other) => bytes.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Text as it may be shown: control characters become their visible
/// pictures (so no escape sequence reaches the terminal) and
/// direction overrides a visible mark (so the text reads as stored).
pub fn clean(s: &str) -> String {
    s.chars().map(visible).collect()
}

fn visible(c: char) -> char {
    match c {
        '\t' => ' ',
        '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + c as u32).unwrap_or('?'),
        '\u{7f}' => '\u{2421}',
        '\u{80}'..='\u{9f}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}'
        | '\u{061c}' => '\u{fffd}',
        c => c,
    }
}

/// One line of file text as shown, cut into pieces at most `width`
/// columns wide (a character wider than that gets a piece of its own):
/// tabs go to the next stop of [`TAB`], everything else is made
/// visible. At least one piece and at most `most`; work stops there, so
/// a huge line costs no more than what shows of it.
fn wrap(s: &str, width: usize, most: usize) -> Vec<(String, usize)> {
    let width = width.max(1);
    let most = most.max(1);
    let mut out = Vec::new();
    let (mut piece, mut used, mut col) = (String::new(), 0, 0);
    let mut push = |c: char, w: usize, out: &mut Vec<(String, usize)>| {
        if used + w > width && used > 0 {
            out.push((std::mem::take(&mut piece), used));
            used = 0;
        }
        if out.len() < most {
            piece.push(c);
            used += w;
        }
    };
    for c in s.chars() {
        if out.len() >= most {
            break;
        }
        if c == '\t' {
            for _ in 0..TAB - col % TAB {
                push(' ', 1, &mut out);
            }
            col += TAB - col % TAB;
        } else {
            let c = visible(c);
            let w = c.width().unwrap_or(0);
            push(c, w, &mut out);
            col += w;
        }
    }
    if out.len() < most {
        out.push((piece, used));
    }
    out
}

/// `s` cut to `width` columns, with `…` where it was cut.
fn fit(s: &str, width: usize) -> (String, usize) {
    let total: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= width {
        return (s.to_owned(), total);
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    if width > 0 {
        out.push('…');
        used += 1;
    }
    (out, used)
}

/// The colours of changed lines: backgrounds a little towards red and
/// green from the pane's own.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub removed: [u8; 3],
    pub added: [u8; 3],
}

/// What the tab shows.
#[derive(Debug, Default)]
pub struct Rendered {
    /// Lines joined with CR LF, ending without one.
    pub text: String,
    /// The line each file's heading is on, for n and p.
    pub files: Vec<usize>,
}

const DIM: &str = "\x1b[0;90m";
const RESET: &str = "\x1b[0m";

/// Lays `diff` out `cols` columns wide: a summary, then each file with
/// its lines side by side (stacked when the pane is narrow).
/// `notes` are shown in amber under the summary (why only part may
/// show).
pub fn render(diff: &Diff, cols: usize, look: &Look, notes: &[String]) -> Rendered {
    let cols = cols.max(20);
    let mut out: Vec<String> = Vec::new();
    let mut files = Vec::new();
    let (added, removed) = diff
        .files
        .iter()
        .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
    let (top, _) = fit(&diff.top, cols.saturating_sub(12));
    out.push(format!("\x1b[0;1mChanges in {top}{RESET}"));
    let since = match &diff.head {
        Some(h) => format!("since commit {h}"),
        None => "no commits yet: staged, then unstaged".to_owned(),
    };
    out.push(format!("{DIM}{}{RESET}", fit(&since, cols).0));
    let count = match diff.files.len() {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    };
    out.push(format!(
        "{DIM}{}{RESET}",
        fit(
            &format!(
                "{count}, +{added} -{removed}.   r: look again   n/p: next/previous file   \
                 arrows, PgUp/PgDn, Home/End: scroll"
            ),
            cols
        )
        .0
    ));
    let failed = diff
        .failed
        .then(|| "git reported an error, so files may be missing here.".to_owned());
    for note in notes.iter().chain(&failed) {
        out.push(format!("\x1b[0;33m{}{RESET}", fit(note, cols).0));
    }
    out.push(String::new());
    if diff.files.is_empty() && notes.is_empty() && !diff.failed {
        out.push("No changes: the files are as in the last commit.".to_owned());
    }
    // Every push below checks the row limit first, so a diff with many
    // files or one huge line stops at MAX_ROWS (plus one note).
    let mut over = false;
    for f in &diff.files {
        if out.len() >= MAX_ROWS {
            over = true;
            break;
        }
        let (letter, colour) = f.kind.letter();
        let counts = format!("  +{} -{}", f.added, f.removed);
        let (name, _) = fit(f.name(), cols.saturating_sub(4 + counts.len()));
        out.push(format!(
            " \x1b[0;{colour}m{letter}{RESET} {name}{DIM}{counts}{RESET}"
        ));
    }
    'files: for f in &diff.files {
        if over {
            break;
        }
        // A blank row, the header bar and maybe a note.
        if out.len() + 3 > MAX_ROWS {
            over = true;
            break;
        }
        out.push(String::new());
        files.push(out.len());
        let (letter, _) = f.kind.letter();
        let title = match f.kind {
            Kind::Renamed => format!(" {letter} {} (was {}) ", f.new, f.old),
            _ => format!(" {letter} {} ", f.name()),
        };
        let counts = format!(" +{} -{} ", f.added, f.removed);
        let (title, used) = fit(&title, cols.saturating_sub(counts.len()));
        let pad = cols.saturating_sub(used + counts.len());
        out.push(format!(
            "\x1b[0;1;7m{title}{}{counts}{RESET}",
            " ".repeat(pad)
        ));
        if let Some(note) = f.note {
            out.push(format!("{DIM}{}{RESET}", fit(note, cols).0));
        } else if f.hunks.is_empty() {
            out.push(format!(
                "{DIM}{}{RESET}",
                fit("No lines changed (the file's mode or name only).", cols).0
            ));
        }
        let most = f
            .hunks
            .iter()
            .flat_map(|h| &h.rows)
            .flat_map(|r| [r.left.as_ref(), r.right.as_ref()])
            .flatten()
            .map(|(n, _)| *n)
            .max()
            .unwrap_or(1);
        let nw = most.to_string().len().max(3);
        for (i, h) in f.hunks.iter().enumerate() {
            let at = format!(
                "{} lines {}, now {}  {}",
                if i == 0 { "──" } else { "⋯⋯" },
                h.old_start,
                h.new_start,
                h.heading
            );
            if out.len() >= MAX_ROWS {
                over = true;
                break 'files;
            }
            out.push(format!("{DIM}{}{RESET}", fit(&at, cols).0));
            for r in &h.rows {
                let budget = MAX_ROWS.saturating_sub(out.len());
                if budget == 0 {
                    over = true;
                    break 'files;
                }
                row(&mut out, r, cols, nw, look, budget);
            }
        }
    }
    if over || out.len() >= MAX_ROWS {
        out.push(format!(
            "\x1b[0;33m{}{RESET}",
            fit(
                "FeroSSH shows no more lines here: there are too many.",
                cols
            )
            .0
        ));
    }
    Rendered {
        text: out.join("\r\n"),
        files,
    }
}

/// One row, side by side when there is room: `nw` columns of line
/// number, a space, the text; a bar between the two sides.
/// At most `budget` rows (at least 1); a longer line is cut there.
fn row(out: &mut Vec<String>, r: &Row, cols: usize, nw: usize, look: &Look, budget: usize) {
    let bg = |rgb: [u8; 3]| format!("\x1b[48;2;{};{};{}m", rgb[0], rgb[1], rgb[2]);
    let half = (cols - 1) / 2;
    let side_text = half.saturating_sub(nw + 1);
    if side_text < 12 {
        // Too narrow: the old line above the new one, a whole row each.
        let start = out.len();
        let tw = cols.saturating_sub(nw + 2).max(1);
        let one =
            |out: &mut Vec<String>, side: &(usize, String), mark: char, colour: Option<[u8; 3]>| {
                let colour = colour.map(bg).unwrap_or_default();
                let room = budget.saturating_sub(out.len() - start);
                if room == 0 {
                    return;
                }
                for (k, (piece, used)) in wrap(&side.1, tw, room).into_iter().enumerate() {
                    let num = if k == 0 {
                        side.0.to_string()
                    } else {
                        String::new()
                    };
                    let pad = " ".repeat(tw - used.min(tw));
                    out.push(format!(
                        "{DIM}{num:>nw$}{RESET}{colour}{mark}{piece}{pad}{RESET}"
                    ));
                }
            };
        if !r.changed {
            if let Some(right) = &r.right {
                one(out, right, ' ', None);
            }
            return;
        }
        if let Some(left) = &r.left {
            one(out, left, '-', Some(look.removed));
        }
        if let Some(right) = &r.right {
            one(out, right, '+', Some(look.added));
        }
        return;
    }
    let right_text = (cols - 1 - half).saturating_sub(nw + 1);
    let cells = |side: &Option<(usize, String)>, width: usize| match side {
        Some((n, t)) => (Some(*n), wrap(t, width, budget)),
        None => (None, Vec::new()),
    };
    let (ln, lp) = cells(&r.left, side_text);
    let (rn, rp) = cells(&r.right, right_text);
    let height = lp.len().max(rp.len()).max(1);
    let (lbg, rbg) = if r.changed {
        (bg(look.removed), bg(look.added))
    } else {
        (String::new(), String::new())
    };
    for k in 0..height {
        let mut line = String::new();
        let side = |line: &mut String,
                    num: Option<usize>,
                    pieces: &[(String, usize)],
                    width: usize,
                    colour: &str| {
            let n = match num {
                Some(n) if k == 0 => n.to_string(),
                _ => String::new(),
            };
            line.push_str(&format!("{DIM}{n:>nw$}{RESET} "));
            match pieces.get(k) {
                Some((piece, used)) => {
                    let pad = " ".repeat(width - (*used).min(width));
                    line.push_str(&format!("{colour}{piece}{pad}{RESET}"));
                }
                None => line.push_str(&" ".repeat(width)),
            }
        };
        side(&mut line, ln, &lp, side_text, &lbg);
        line.push_str(&format!("{DIM}│{RESET}"));
        side(&mut line, rn, &rp, right_text, &rbg);
        out.push(line);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::*;

    fn changes(out: &str) -> Diff {
        match parse(out.as_bytes()) {
            Outcome::Changes(d) => d,
            other => panic!("{other:?}"),
        }
    }

    const LOOK: Look = Look {
        removed: [60, 20, 20],
        added: [20, 50, 20],
    };

    /// Visible text of a rendered line: escape sequences taken out.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            if esc {
                if c.is_ascii_alphabetic() {
                    esc = false;
                }
            } else if c == '\x1b' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn the_command_has_no_single_quote_or_bang_inside_and_is_one_line() {
        let c = String::from_utf8(command()).unwrap();
        let inner = c
            .strip_prefix("exec sh -c '")
            .unwrap()
            .strip_suffix('\'')
            .unwrap();
        assert!(!inner.contains('\''));
        assert!(!inner.contains('!'));
        assert!(!inner.contains('\n'));
        assert!(inner.contains("diff --no-color"));
    }

    #[test]
    fn the_folder_goes_on_one_line_of_input() {
        assert_eq!(input(b"/srv/a b").unwrap(), b"/srv/a b\n");
        assert!(input(b"/srv/a\nrm -rf").is_none());
        assert!(input(b"").is_none());
    }

    #[test]
    fn markers_say_why_there_is_nothing() {
        assert_eq!(parse(b"@ferossh-diff norepo\n"), Outcome::NoRepo);
        assert_eq!(parse(b"@ferossh-diff nodir\n"), Outcome::NoDir);
        assert_eq!(parse(b"@ferossh-diff nogit\n"), Outcome::NoGit);
    }

    const SAMPLE: &str = concat!(
        "@ferossh-diff top /home/me/app\n",
        "@ferossh-diff head 1a2b3c4 Fix the parser\n",
        "diff --git a/src/main.rs b/src/main.rs\n",
        "index 111..222 100644\n",
        "--- a/src/main.rs\n",
        "+++ b/src/main.rs\n",
        "@@ -1,5 +1,6 @@ fn main() {\n",
        " fn main() {\n",
        "-    println!(\"hi\");\n",
        "-    let x = 1;\n",
        "+    println!(\"hello\");\n",
        "+    let x = 2;\n",
        "+    let y = 3;\n",
        "     run();\n",
        "--- comment that starts like a header\n",
        "+++ and one more\n",
        " }\n",
        "diff --git a/old.txt b/new.txt\n",
        "similarity index 90%\n",
        "rename from old.txt\n",
        "rename to new.txt\n",
        "diff --git a/gone.txt b/gone.txt\n",
        "deleted file mode 100644\n",
        "--- a/gone.txt\n",
        "+++ /dev/null\n",
        "@@ -1 +0,0 @@\n",
        "-bye\n",
        "diff --git a/logo.png b/logo.png\n",
        "new file mode 100644\n",
        "Binary files /dev/null and b/logo.png differ\n",
        "diff --git a/notes.md b/notes.md\n",
        "new file mode 100644\n",
        "--- /dev/null\n",
        "+++ b/notes.md\n",
        "@@ -0,0 +1,2 @@\n",
        "+one\n",
        "+two\n",
        "\\ No newline at end of file\n",
        "@ferossh-diff big dump.log\n",
    );

    #[test]
    fn a_diff_reads_back_as_files_and_side_by_side_rows() {
        let sample = SAMPLE.replace("@@ -1,5 +1,6 @@", "@@ -1,6 +1,7 @@");
        let d = changes(&sample);
        assert_eq!(d.top, "/home/me/app");
        assert_eq!(d.head.as_deref(), Some("1a2b3c4 Fix the parser"));
        let names: Vec<_> = d.files.iter().map(|f| (f.name(), f.kind)).collect();
        assert_eq!(
            names,
            [
                ("src/main.rs", Kind::Modified),
                ("new.txt", Kind::Renamed),
                ("gone.txt", Kind::Deleted),
                ("logo.png", Kind::Added),
                ("notes.md", Kind::Added),
                ("dump.log", Kind::Added),
            ]
        );
        let main = &d.files[0];
        // "--- comment" and "+++ and one" are lines of the hunk, not
        // headers: the counts say so.
        assert_eq!((main.added, main.removed), (4, 3));
        let rows = &main.hunks[0].rows;
        assert_eq!(rows[0].left.as_ref().unwrap().0, 1);
        assert!(!rows[0].changed);
        // Removed lines next to the added lines that replaced them.
        assert_eq!(rows[1].left.as_ref().unwrap().1, "    println!(\"hi\");");
        assert_eq!(
            rows[1].right.as_ref().unwrap().1,
            "    println!(\"hello\");"
        );
        assert_eq!(rows[3].left, None);
        assert_eq!(
            rows[3].right.as_ref().unwrap(),
            &(4, "    let y = 3;".to_owned())
        );
        assert_eq!(rows[4].left.as_ref().unwrap().0, 4);
        assert_eq!(rows[4].right.as_ref().unwrap().0, 5);
        assert_eq!(
            rows[5].left.as_ref().unwrap().1,
            "-- comment that starts like a header"
        );
        assert_eq!(rows[5].right.as_ref().unwrap().1, "++ and one more");
        assert_eq!(main.hunks[0].heading, "fn main() {");
        assert_eq!(d.files[1].old, "old.txt");
        assert_eq!(
            d.files[2].hunks[0].rows[0].left.as_ref().unwrap(),
            &(1, "bye".to_owned())
        );
        assert!(d.files[3].note.is_some());
        let notes = &d.files[4].hunks[0].rows;
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[1].right.as_ref().unwrap(), &(2, "two".to_owned()));
        assert!(d.files[5].note.is_some());
    }

    #[test]
    fn nothing_from_the_server_can_steer_the_terminal() {
        let out = "@ferossh-diff top /r\x1b]0;evil\x07\n\
diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-\x1b[2Jold\n+\u{202e}new\u{9b}1m\n";
        let d = changes(out);
        let r = render(&d, 100, &LOOK, &[]);
        // The only escapes are FeroSSH's own colours: CSI ... m.
        for (i, _) in r.text.match_indices('\x1b') {
            let rest = &r.text[i + 1..];
            assert!(rest.starts_with('['), "{rest:?}");
            let end = rest.find(|c: char| c.is_ascii_alphabetic()).unwrap();
            assert_eq!(&rest[end..=end], "m");
            assert!(rest[1..end].chars().all(|c| c.is_ascii_digit() || c == ';'));
        }
        assert!(!r.text.contains('\u{202e}'));
        assert!(!r.text.contains('\u{9b}'));
        assert!(!r.text.contains('\x07'));
    }

    #[test]
    fn every_line_fits_the_pane_wide_or_narrow() {
        let long = "z".repeat(300);
        let wide = "界".repeat(80);
        let out = format!(
            "@ferossh-diff top /r\ndiff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n-{long}\n-\tTab\n+{wide}\n+short\n"
        );
        let d = changes(&out);
        for cols in [20, 33, 60, 80, 81, 200] {
            let r = render(&d, cols, &LOOK, &["Only part shows.".to_owned()]);
            for line in r.text.split("\r\n") {
                let w: usize = plain(line).chars().map(|c| c.width().unwrap_or(0)).sum();
                assert!(w <= cols, "{cols}: {w} {line:?}");
            }
        }
        // Long lines wrap rather than being cut: all 300 x's are shown.
        let r = render(&d, 100, &LOOK, &[]);
        assert_eq!(plain(&r.text).matches('z').count(), 300);
        assert!(plain(&r.text).contains("    Tab"));
    }

    #[test]
    fn file_headings_are_where_n_and_p_go() {
        let d = changes(SAMPLE);
        let r = render(&d, 120, &LOOK, &[]);
        let lines: Vec<_> = r.text.split("\r\n").collect();
        assert_eq!(r.files.len(), d.files.len());
        assert!(plain(lines[r.files[0]]).contains("src/main.rs"));
        assert!(plain(lines[r.files[1]]).contains("new.txt (was old.txt)"));
    }

    #[test]
    fn quoted_names_read_back() {
        assert_eq!(unquote("\"a/caf\\303\\251 \\\"x\\\"\""), "a/café \"x\"");
        assert_eq!(
            git_names("\"a/tab\\there\" \"b/tab\\there\""),
            ("tab here".to_owned(), "tab here".to_owned())
        );
        assert_eq!(
            git_names("a/two words b/two words"),
            ("two words".to_owned(), "two words".to_owned())
        );
        assert_eq!(side_name("b/with space\t", "b/").unwrap(), "with space");
        assert_eq!(side_name("/dev/null", "a/"), None);
    }

    /// Runs the command as a login shell would, in a scratch repository.
    #[cfg(unix)]
    fn run(dir: &std::path::Path) -> Outcome {
        run_with(dir, None)
    }

    /// Runs the script with `bin` first on the PATH.
    #[cfg(unix)]
    fn run_with(dir: &std::path::Path, bin: Option<&std::path::Path>) -> Outcome {
        use std::io::Write;
        let mut sh = std::process::Command::new("sh");
        if let Some(bin) = bin {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut dirs = vec![bin.to_path_buf()];
            dirs.extend(std::env::split_paths(&path));
            sh.env("PATH", std::env::join_paths(dirs).unwrap());
        }
        let mut child = sh
            .arg("-c")
            .arg(String::from_utf8(command()).unwrap())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let line = input(dir.as_os_str().as_encoded_bytes()).unwrap();
        child.stdin.take().unwrap().write_all(&line).unwrap();
        let out = child.wait_with_output().unwrap();
        parse(&out.stdout)
    }

    #[cfg(unix)]
    #[test]
    fn the_script_reads_a_real_repository() {
        let git_here = std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !git_here {
            return;
        }
        let tmp = std::env::temp_dir().join(format!("ferossh-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("my repo's");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "{args:?}");
        };
        assert_eq!(run(&tmp), Outcome::NoRepo);
        assert_eq!(run(&tmp.join("missing")), Outcome::NoDir);
        git(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        // Before the first commit: the staged file shows.
        git(&["add", "a.txt"]);
        let Outcome::Changes(d) = run(&repo.join("sub")) else {
            panic!()
        };
        assert_eq!(d.head, None);
        assert_eq!(d.files.len(), 1);
        git(&["commit", "-q", "-m", "first"]);
        std::fs::write(repo.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(repo.join("sub").join("new file.md"), "hello\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(repo.join("x.log"), "ignored\n").unwrap();
        let Outcome::Changes(d) = run(&repo.join("sub")) else {
            panic!()
        };
        assert!(d.top.ends_with("my repo's"), "{}", d.top);
        // The untracked files' "they differ" (status 1) is no failure.
        assert!(!d.failed);
        assert!(d.head.as_deref().unwrap().ends_with(" first"));
        let names: Vec<_> = d.files.iter().map(|f| (f.name(), f.kind)).collect();
        assert_eq!(
            names,
            [
                ("a.txt", Kind::Modified),
                (".gitignore", Kind::Added),
                ("sub/new file.md", Kind::Added),
            ]
        );
        assert_eq!((d.files[0].added, d.files[0].removed), (2, 1));
        assert_eq!(
            d.files[2].hunks[0].rows[0].right,
            Some((1, "hello".to_owned()))
        );
        // A git whose ls-files fails: the tab says files may be missing,
        // never "No changes".
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "second"]);
        let real = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout;
        let real = String::from_utf8(real).unwrap().trim().to_owned();
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("git");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nfor a; do [ \"$a\" = ls-files ] && exit 128; done\nexec '{real}' \"$@\"\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let Outcome::Changes(d) = run(&repo) else {
            panic!()
        };
        assert!(!d.failed && d.files.is_empty());
        let Outcome::Changes(d) = run_with(&repo, Some(&bin)) else {
            panic!()
        };
        assert!(d.failed);
        assert!(!plain(&render(&d, 80, &LOOK, &[]).text).contains("No changes"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_failed_git_is_never_no_changes() {
        let d =
            changes("@ferossh-diff top /r\n@ferossh-diff head abc Init\n@ferossh-diff failed\n");
        assert!(d.failed);
        let text = plain(&render(&d, 80, &LOOK, &[]).text);
        assert!(text.contains("git reported an error"));
        assert!(!text.contains("No changes"));
    }

    #[test]
    fn a_huge_line_stops_at_the_row_limit() {
        let tabs = "\t".repeat(2_000_000);
        let out = format!(
            "@ferossh-diff top /r\ndiff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+{tabs}\n"
        );
        let d = changes(&out);
        let r = render(&d, 20, &LOOK, &[]);
        let rows = r.text.split("\r\n").count();
        assert!(rows <= MAX_ROWS + 1, "{rows}");
        assert!(plain(&r.text).contains("FeroSSH shows"));
        assert_eq!(wrap(&tabs, 10, 3).len(), 3);
        assert_eq!(wrap("", 10, 3), [(String::new(), 0)]);
    }

    #[test]
    fn a_long_removed_then_added_line_stops_at_the_row_limit() {
        let tabs = "\t".repeat(2_000_000);
        let out = format!(
            "@ferossh-diff top /r\ndiff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-{tabs}\n+{tabs}\n"
        );
        let d = changes(&out);
        // 20 columns stacks the sides; 200 puts them side by side.
        for cols in [20, 200] {
            let r = render(&d, cols, &LOOK, &[]);
            let rows = r.text.split("\r\n").count();
            assert!(rows <= MAX_ROWS + 1, "{cols}: {rows}");
            // Wide, the line fits in fewer rows than the limit.
            assert_eq!(plain(&r.text).contains("FeroSSH shows"), cols == 20);
        }
    }

    #[test]
    fn many_files_without_lines_stop_at_the_row_limit() {
        let mut out = String::from("@ferossh-diff top /r\n");
        for i in 0..(MAX_ROWS / 2) {
            out.push_str(&format!(
                "diff --git a/f{i} b/f{i}\nold mode 100644\nnew mode 100755\n"
            ));
        }
        let d = changes(&out);
        assert_eq!(d.files.len(), MAX_ROWS / 2);
        let r = render(&d, 80, &LOOK, &[]);
        let rows = r.text.split("\r\n").count();
        assert!(rows <= MAX_ROWS + 1, "{rows}");
        assert!(plain(&r.text).contains("FeroSSH shows"));
        assert!(r.files.iter().all(|&at| at < rows));
    }

    #[test]
    fn no_changes_says_so() {
        let d = changes("@ferossh-diff top /r\n@ferossh-diff head abc Init\n");
        let r = render(&d, 80, &LOOK, &[]);
        assert!(plain(&r.text).contains("No changes"));
        assert!(r.files.is_empty());
    }
}
