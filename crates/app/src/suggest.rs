//! Command suggestions as you type (Edit > Suggest commands as you type).
//!
//! FeroSSH reads the command line the way it shows on the screen: from
//! where typing started (the end of the prompt) to the cursor. When the
//! host's echo shows it and Enter runs it, the line goes into this host's
//! history. While typing, the most recent earlier command that starts
//! with what is typed so far shows greyed after the cursor; Right arrow
//! or End types the rest of it, Ctrl+Right its next word, Ctrl+Up and
//! Ctrl+Down go through the other matches, and Shift+Delete forgets it.
//! When history has nothing, a short built-in list of common commands
//! fills in.
//!
//! Nothing typed at a prompt the host does not echo is ever seen, since
//! only the screen is read: a password typed at `Password:` leaves the
//! line empty. Lines at a prompt that asks for a password, passphrase, PIN
//! or code are never kept even when echoed, nor are commands that carry
//! one (`PASSWORD=...`, `mysql -pSecret`, `https://user:pass@`), nor lines
//! that start with a space (as bash's `ignorespace`). History is per host
//! and stays on this PC, encrypted with the user's Windows sign-in
//! ([`crate::history_store`]).

use ferossh_term::{InputLine, InputMark, Terminal};

/// Commands kept per host; the oldest go first.
pub const MAX_ENTRIES: usize = 1000;
/// Longer lines are not kept.
pub const MAX_LEN: usize = 1024;
/// Suggestions offered for one line, at most.
pub const MAX_CHOICES: usize = 20;

/// One host's commands, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    /// The host it belongs to, as [`crate::win`] names it.
    pub key: String,
    entries: Vec<String>,
}

impl History {
    pub fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            entries: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Adds `command` as the most recent, moving it if it was there.
    /// `false` when it was already the most recent.
    pub fn record(&mut self, command: &str) -> bool {
        if self.entries.last().is_some_and(|c| c == command) {
            return false;
        }
        self.entries.retain(|c| c != command);
        self.entries.push(command.to_owned());
        if self.entries.len() > MAX_ENTRIES {
            let extra = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..extra);
        }
        true
    }

    /// Forgets `command`; `true` if it was kept.
    pub fn remove(&mut self, command: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|c| c != command);
        self.entries.len() != before
    }

    /// Kept commands longer than `typed` that start with it, most recent
    /// first.
    pub fn matching<'a>(&'a self, typed: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.entries
            .iter()
            .rev()
            .map(String::as_str)
            .filter(move |c| c.len() > typed.len() && c.starts_with(typed))
    }

    /// The text kept on disk (before encryption): a header, the host, then
    /// one command per line, oldest first.
    pub fn to_text(&self) -> String {
        let mut out = format!("{HEADER}\n{}\n", self.key);
        for c in &self.entries {
            out.push_str(c);
            out.push('\n');
        }
        out
    }

    /// Reads [`Self::to_text`]; `None` when it is not a history of `key`.
    pub fn parse(text: &str, key: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != HEADER || lines.next()? != key {
            return None;
        }
        let mut h = Self::new(key);
        for line in lines {
            if keepable(line) {
                h.record(line);
            }
        }
        Some(h)
    }
}

const HEADER: &str = "FeroSSH command history 1";

/// Whether `command` may be kept at all: one line of text, not too long.
fn keepable(command: &str) -> bool {
    !command.trim().is_empty() && command.len() <= MAX_LEN && !command.chars().any(char::is_control)
}

/// Words that make a prompt a question for a secret wherever they are.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "passcode",
    "wachtwoord",
    "kennwort",
    "passwort",
    "contraseña",
    "senha",
    "parola",
    "hasło",
    "salasana",
    "lösenord",
    "adgangskode",
    "passord",
];

/// Words that make a prompt a question for a secret when it ends with a
/// colon or question mark (so a host called `token-server` still gets
/// suggestions).
const SECRET_HINTS: &[&str] = &[
    "pin",
    "otp",
    "token",
    "secret",
    "code",
    "key",
    "mfa",
    "2fa",
    "totp",
    "verification",
];

/// Whether the text before the typing asks for a secret: `Password:`,
/// `[sudo] password for jo:`, `Enter passphrase for key`, `PIN:`,
/// `Verification code:` and the like.
pub fn secret_prompt(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    if lower.contains("mot de passe") {
        return true;
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.iter().any(|w| SECRET_WORDS.contains(w)) {
        return true;
    }
    let asks = lower.trim_end().ends_with([':', '?', '：']);
    asks && words.iter().any(|w| SECRET_HINTS.contains(w))
}

/// Whether a command line carries a secret: an assignment or option
/// naming one (`PASSWORD=`, `--token x`, `api_key:`), MySQL's `-pSecret`,
/// or a URL with a password in it.
pub fn carries_secret(command: &str) -> bool {
    let lower = command.to_lowercase();
    const NAMES: &[&str] = &[
        "pass",
        "pwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "auth",
        "credential",
        "private_key",
        "private-key",
    ];
    // NAME= NAME: --NAME value: what follows the name is the secret.
    for name in NAMES {
        let mut from = 0;
        while let Some(i) = lower[from..].find(name) {
            let end = from + i + name.len();
            // The rest of the word (`password`, `TOKEN_FILE`).
            let tail: String = lower[end..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            let after = lower[end + tail.len()..].trim_start_matches(['"', '\'']);
            // `password = 'x'` at a Python or SQL prompt too.
            let next = after.trim_start();
            if next.starts_with('=') || next.starts_with(':') {
                return true;
            }
            let before = &lower[..from + i];
            if before.ends_with("--") || before.ends_with(" -") {
                // An option: `--password x`, `--token=x` (above).
                if after.starts_with(' ') && !after.trim().is_empty() {
                    return true;
                }
            }
            from = end;
        }
    }
    // mysql -pSecret, mysqldump -pSecret.
    if lower
        .split_whitespace()
        .next()
        .is_some_and(|w| w.starts_with("mysql") || w.starts_with("mariadb"))
        && command
            .split_whitespace()
            .any(|w| w.len() > 2 && w.starts_with("-p"))
    {
        return true;
    }
    // curl -u user:pass, sshpass -p pass.
    let words: Vec<&str> = command.split_whitespace().collect();
    for pair in words.windows(2) {
        if let [flag, value] = pair {
            let curl_user = matches!(*flag, "-u" | "--user" | "-U" | "--proxy-user");
            if curl_user && value.contains(':') {
                return true;
            }
        }
    }
    if words.first().is_some_and(|w| *w == "sshpass")
        && words
            .iter()
            .any(|w| *w == "-p" || (w.starts_with("-p") && w.len() > 2))
    {
        return true;
    }
    // scheme://user:pass@host
    let mut rest = command;
    while let Some(i) = rest.find("://") {
        rest = &rest[i + 3..];
        let authority = rest.split(['/', ' ', '\'', '"']).next().unwrap_or("");
        if let Some((userinfo, _)) = authority.rsplit_once('@')
            && userinfo.contains(':')
        {
            return true;
        }
    }
    false
}

/// What to keep of a line Enter ran at `prompt`, if anything.
pub fn to_record(prompt: &str, line: &str) -> Option<String> {
    // A leading space means "keep this out of history", as in bash.
    if line.starts_with(' ') || secret_prompt(prompt) {
        return None;
    }
    let line = line.trim_end();
    if line.chars().count() < 2
        || !keepable(line)
        || line.chars().all(|c| matches!(c, '*' | '•' | '●' | '∙'))
        || carries_secret(line)
    {
        return None;
    }
    Some(line.to_owned())
}

/// Common commands, for when this host's history has nothing yet.
const BUILTIN: &[&str] = &[
    "apt install ",
    "apt list --upgradable",
    "apt search ",
    "apt update",
    "apt upgrade",
    "cat /etc/os-release",
    "cd ..",
    "chmod +x ",
    "chown -R ",
    "cp -r ",
    "crontab -e",
    "crontab -l",
    "curl -fsSL ",
    "df -h",
    "dmesg -T | tail",
    "dnf install ",
    "dnf upgrade",
    "docker compose down",
    "docker compose logs -f",
    "docker compose pull",
    "docker compose up -d",
    "docker exec -it ",
    "docker images",
    "docker logs -f ",
    "docker ps -a",
    "du -sh *",
    "find . -name ",
    "free -h",
    "git add .",
    "git branch -a",
    "git checkout ",
    "git commit -m ",
    "git diff --staged",
    "git fetch --all --prune",
    "git log --oneline --graph",
    "git pull --rebase",
    "git push",
    "git stash pop",
    "git status",
    "git switch ",
    "grep -rn ",
    "history | grep ",
    "hostnamectl",
    "htop",
    "ip -br addr",
    "ip route",
    "journalctl -f",
    "journalctl -u ",
    "journalctl -xe",
    "kubectl config get-contexts",
    "kubectl describe pod ",
    "kubectl get pods -A",
    "kubectl logs -f ",
    "less ",
    "ls -la",
    "ls -lh",
    "lsblk",
    "mkdir -p ",
    "mount | column -t",
    "ping -c 4 ",
    "ps aux | grep ",
    "rsync -avP ",
    "ss -tulpn",
    "sudo -i",
    "sudo systemctl restart ",
    "sudo systemctl status ",
    "systemctl list-units --failed",
    "systemctl restart ",
    "systemctl status ",
    "tail -f ",
    "tar -czf ",
    "tar -xzf ",
    "tmux attach",
    "tmux ls",
    "top",
    "uname -a",
    "uptime",
    "vim ",
    "watch -n 1 ",
    "whoami",
];

/// What could finish `typed`: this host's history first, most recent
/// first, then the built-in commands; each a whole line longer than
/// `typed`, at most [`MAX_CHOICES`].
pub fn choices(history: Option<&History>, typed: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if typed.trim().is_empty() || typed.starts_with(' ') {
        return out;
    }
    let mut add = |c: &str| {
        if out.len() < MAX_CHOICES && !out.iter().any(|o| o == c) {
            out.push(c.to_owned());
        }
    };
    if let Some(h) = history {
        for c in h.matching(typed) {
            add(c);
        }
    }
    // Built-ins after two characters, so one letter is not a guess.
    if typed.chars().count() >= 2 {
        for c in BUILTIN {
            if c.len() > typed.len() && c.starts_with(typed) {
                add(c);
            }
        }
    }
    out
}

/// The next word of `rest`, the spaces before it included: what
/// Ctrl+Right takes of a suggestion.
pub fn next_word(rest: &str) -> &str {
    let start = rest.len() - rest.trim_start().len();
    let end = rest[start..].find(' ').map_or(rest.len(), |i| start + i);
    &rest[..end]
}

/// One pane's typing, followed on its screen.
#[derive(Debug, Default)]
pub struct Tracker {
    /// Where the line being typed starts.
    mark: Option<InputMark>,
    /// The line Enter just ran, as it read then: read again once the
    /// cursor moved past it, since the shell may redraw it first (zsh and
    /// fish drop their own grey suggestion).
    pending: Option<(InputMark, String)>,
    /// Which of the choices shows: Ctrl+Up and Ctrl+Down move it.
    pub choice: usize,
    /// The host's output count when a key was last sent: until the host
    /// answers, the screen shows the line from before that key, and no
    /// suggestion may be made from it.
    sent_at: Option<u64>,
    /// Typed ahead while the host had not answered the last Enter yet:
    /// where this line starts is unknown, so it is not followed.
    skip_line: bool,
    /// The line as last read with the cursor at its end.
    seen: Option<String>,
    /// What the line must read once the host echoed the text typed since
    /// `seen`: until it does, no suggestion (keys typed faster than the
    /// echo would otherwise get one made for an older line).
    expect: Option<String>,
}

impl Tracker {
    /// Follows `bytes` the user typed, before they go to the host (the
    /// cursor still where the typing lands). What comes back is a line
    /// an earlier Enter ran, to keep.
    pub fn typed(&mut self, term: &Terminal, bytes: &[u8]) -> Option<String> {
        self.choice = 0;
        self.sent_at = Some(term.output_count());
        let had_mark = self.mark.is_some();
        let base = self.expect.take().or_else(|| self.seen.take());
        let done = self.settle(term);
        match bytes {
            b"\r" | b"\r\n" | b"\n" => {
                self.skip_line = false;
                if let Some(mut mark) = self.mark.take()
                    && let Some(line) = term.input_line(&mut mark)
                {
                    let whole = format!("{}{}", line.before, line.after);
                    self.pending = Some((mark, whole));
                }
            }
            // Ctrl+C, Ctrl+D, Ctrl+Z: the line is dropped, or the program
            // ends.
            [0x03] | [0x04] | [0x1a] => {
                self.mark = None;
                self.skip_line = false;
            }
            // A paste of several lines runs them as they come: not followed.
            b if b.contains(&b'\r') || b.contains(&b'\n') => self.mark = None,
            // The first key of a line marks where it starts. A mark that
            // no longer reads (a reverse search shows instead of the
            // prompt) stays: it reads again once the prompt is back, and a
            // new mark in mid-line would take typed text for the prompt.
            _ => {
                let answered = self
                    .pending
                    .as_ref()
                    .is_none_or(|(mark, _)| !term.input_here(mark));
                if !answered {
                    self.skip_line = true;
                }
                if self.mark.is_none() && !self.skip_line {
                    self.mark = term.input_mark();
                }
                // Plain text typed at the end of the line: the line it
                // makes is known. Other keys (Backspace, arrows, Tab) make
                // a line only the host knows: then any output will do.
                let base = if had_mark { base } else { Some(String::new()) };
                let text = std::str::from_utf8(bytes)
                    .ok()
                    .filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
                if self.mark.is_some()
                    && let (Some(base), Some(text)) = (base, text)
                {
                    self.expect = Some(base + text);
                }
            }
        }
        done
    }

    /// After output: the line Enter ran, once the cursor moved past it.
    pub fn poll(&mut self, term: &Terminal) -> Option<String> {
        self.settle(term)
    }

    fn settle(&mut self, term: &Terminal) -> Option<String> {
        let (mark, read) = self.pending.as_ref()?;
        let now = term.input_text(mark);
        // Still on the line as Enter left it: not answered yet. (A line
        // the shell redrew, or a mark that moved with a full scrollback
        // onto the next prompt, reads differently.)
        if term.input_here(mark) && now.as_deref() == Some(read.as_str()) {
            return None;
        }
        let (mark, read) = (mark.clone(), read.clone());
        self.pending = None;
        // The line as redrawn, when the shell only dropped the end (its
        // own grey suggestion); otherwise as Enter read it.
        let line = now
            .filter(|t| !t.is_empty() && read.starts_with(t.as_str()))
            .unwrap_or(read);
        to_record(mark.prompt(), &line)
    }

    /// The line being typed, when suggestions may show for it: typing
    /// started on the main screen, not at a prompt for a secret, and the
    /// cursor is at its end.
    pub fn line(&mut self, term: &Terminal) -> Option<InputLine> {
        if term.wrap_pending() {
            return None;
        }
        let mark = self.mark.as_mut()?;
        if secret_prompt(mark.prompt()) {
            return None;
        }
        let line = term.input_line(mark)?;
        if let Some(e) = &self.expect {
            if line.before != *e {
                return None;
            }
            self.expect = None;
            self.sent_at = None;
        } else if let Some(at) = self.sent_at {
            if at == term.output_count() {
                return None;
            }
            self.sent_at = None;
        }
        if !line.after.is_empty() {
            return None;
        }
        self.seen = Some(line.before.clone());
        Some(line)
    }

    /// Forgets the line being typed (the pane was cleared or reset).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_term::Settings;

    #[test]
    fn history_keeps_the_most_recent_once() {
        let mut h = History::new("ssh:jo@h:22");
        assert!(h.record("ls -la"));
        assert!(h.record("git status"));
        assert!(!h.record("git status"));
        assert!(h.record("ls -la"));
        assert_eq!(h.matching("").collect::<Vec<_>>(), ["ls -la", "git status"]);
        assert_eq!(h.matching("git").collect::<Vec<_>>(), ["git status"]);
        // Only longer lines finish what is typed.
        assert_eq!(h.matching("git status").count(), 0);
        for i in 0..MAX_ENTRIES + 5 {
            h.record(&format!("echo {i}"));
        }
        assert_eq!(h.len(), MAX_ENTRIES);
        assert!(h.remove("echo 10"));
        assert!(!h.remove("echo 10"));
    }

    #[test]
    fn history_text_round_trips_and_checks_its_host() {
        let mut h = History::new("ssh:jo@h:22");
        h.record("cd /var/log");
        h.record("tail -f syslog");
        let text = h.to_text();
        assert_eq!(History::parse(&text, "ssh:jo@h:22"), Some(h));
        assert_eq!(History::parse(&text, "ssh:jo@other:22"), None);
        assert_eq!(History::parse("junk", "ssh:jo@h:22"), None);
    }

    #[test]
    fn secret_prompts_are_recognised() {
        for p in [
            "Password: ",
            "[sudo] password for jo: ",
            "Enter passphrase for key '/home/jo/.ssh/id_ed25519': ",
            "jo@host's password: ",
            "PIN: ",
            "Verification code: ",
            "Enter OTP: ",
            "Wachtwoord: ",
            "Mot de passe : ",
            "Token? ",
        ] {
            assert!(secret_prompt(p), "{p}");
        }
        for p in [
            "jo@token-server:~$ ",
            "jo@debian:~/code$ ",
            "PS C:\\Users\\jo> ",
            "mysql> ",
            "",
        ] {
            assert!(!secret_prompt(p), "{p}");
        }
    }

    #[test]
    fn commands_carrying_secrets_are_not_kept() {
        for c in [
            "export DB_PASSWORD=hunter2",
            "PGPASSWORD='x' psql",
            "curl -H 'Authorization: Bearer abc' https://x",
            "curl https://jo:hunter2@example.com/",
            "git clone https://jo:tok@github.com/a/b",
            "mysql -u root -phunter2",
            "tool --password hunter2",
            "tool --api-key=abc",
            "export GITHUB_TOKEN=abc",
            "password = 'hunter2'",
            "SET PASSWORD = 'x';",
            "token : abc",
            "curl -u jo:hunter2 https://x",
            "sshpass -p hunter2 ssh jo@h",
        ] {
            assert!(carries_secret(c), "{c}");
        }
        for c in [
            "ls -la",
            "passwd",
            "sudo passwd jo",
            "git clone https://github.com/a/b",
            "ssh jo@host",
            "mysql -u root -p",
            "cat token.txt",
            "grep -r password src",
            "curl -u jo https://x",
            "echo \"a   b\"",
        ] {
            assert!(!carries_secret(c), "{c}");
        }
    }

    #[test]
    fn what_is_recorded() {
        assert_eq!(to_record("$ ", "ls -la  "), Some("ls -la".into()));
        assert_eq!(to_record("$ ", " ls -la"), None);
        assert_eq!(to_record("Password: ", "hunter2"), None);
        assert_eq!(to_record("$ ", "********"), None);
        assert_eq!(to_record("$ ", "l"), None);
        assert_eq!(to_record("$ ", ""), None);
        assert_eq!(to_record("$ ", &"x".repeat(MAX_LEN + 1)), None);
    }

    #[test]
    fn choices_put_history_before_built_ins() {
        let mut h = History::new("k");
        h.record("git status --short");
        h.record("git log -p");
        let c = choices(Some(&h), "git s");
        assert_eq!(c[0], "git status --short");
        assert!(c.contains(&"git status".to_owned()));
        assert!(c.contains(&"git stash pop".to_owned()));
        // One letter: history only.
        assert_eq!(choices(Some(&h), "g"), ["git log -p", "git status --short"]);
        assert!(choices(None, "g").is_empty());
        assert!(choices(Some(&h), " git").is_empty());
        assert!(choices(Some(&h), "").is_empty());
    }

    #[test]
    fn next_word_takes_spaces_and_one_word() {
        assert_eq!(next_word("atus --short"), "atus");
        assert_eq!(next_word(" --short x"), " --short");
        assert_eq!(next_word("x"), "x");
        assert_eq!(next_word(""), "");
    }

    fn term() -> Terminal {
        Terminal::new(40, 6, &Settings::new(100))
    }

    #[test]
    fn tracker_records_the_echoed_line_after_enter() {
        let mut t = term();
        let mut k = Tracker::default();
        t.feed(b"jo@h:~$ ");
        for b in b"ls -la" {
            assert_eq!(k.typed(&t, &[*b]), None);
            t.feed(&[*b]);
        }
        assert_eq!(k.line(&t).unwrap().before, "ls -la");
        assert_eq!(k.typed(&t, b"\r"), None);
        // Not until the shell moved on.
        assert_eq!(k.poll(&t), None);
        t.feed(b"\r\ntotal 0\r\njo@h:~$ ");
        assert_eq!(k.poll(&t), Some("ls -la".into()));
        assert_eq!(k.poll(&t), None);
    }

    #[test]
    fn tracker_keeps_nothing_typed_without_echo() {
        let mut t = term();
        let mut k = Tracker::default();
        t.feed(b"$ sudo -i\r\n[sudo] password for jo: ");
        for b in b"hunter2" {
            k.typed(&t, &[*b]);
        }
        // No echo: nothing to suggest for, nothing read.
        assert!(k.line(&t).is_none());
        k.typed(&t, b"\r");
        t.feed(b"\r\nroot@h:~# ");
        assert_eq!(k.poll(&t), None);
    }

    #[test]
    fn tracker_reads_the_line_as_redrawn_after_enter() {
        let mut t = term();
        let mut k = Tracker::default();
        t.feed(b"% ");
        k.typed(&t, b"l");
        t.feed(b"l");
        k.typed(&t, b"s");
        // The shell's own grey suggestion after the cursor.
        t.feed(b"s -la\x1b[4D");
        assert!(k.line(&t).is_none(), "the shell suggests already");
        k.typed(&t, b"\r");
        // zsh drops it before the new line.
        t.feed(b"\x1b[K\r\ntotal 0\r\n% ");
        assert_eq!(k.poll(&t), Some("ls".into()));
    }

    #[test]
    fn tracker_keeps_a_command_when_typing_runs_ahead_of_the_host() {
        let mut t = term();
        let mut k = Tracker::default();
        t.feed(b"$ ");
        k.typed(&t, b"l");
        t.feed(b"l");
        k.typed(&t, b"s");
        t.feed(b"s");
        k.typed(&t, b"\r");
        // pwd typed before the host answered ls.
        for b in b"pwd" {
            assert_eq!(k.typed(&t, &[*b]), None);
        }
        t.feed(b"\r\nfile\r\n$ pwd");
        assert_eq!(k.poll(&t), Some("ls".into()));
        // The typed-ahead line is not followed (its start is unknown).
        assert!(k.line(&t).is_none());
        k.typed(&t, b"\r");
        t.feed(b"\r\n/home/jo\r\n$ ");
        assert_eq!(k.poll(&t), None);
    }

    #[test]
    fn tracker_records_with_no_scrollback_left() {
        let mut t = Terminal::new(40, 3, &Settings::new(0));
        let mut k = Tracker::default();
        t.feed(b"a\r\nb\r\n$ ");
        k.typed(&t, b"l");
        t.feed(b"l");
        k.typed(&t, b"s");
        t.feed(b"s");
        k.typed(&t, b"\r");
        t.feed(b"\r\n$ ");
        assert_eq!(k.poll(&t), Some("ls".into()));
    }

    #[test]
    fn no_suggestion_until_the_host_echoes() {
        let mut t = term();
        let mut k = Tracker::default();
        t.feed(b"$ ");
        k.typed(&t, b"g");
        t.feed(b"g");
        assert_eq!(k.line(&t).unwrap().before, "g");
        k.typed(&t, b"i");
        // A refresh before the echo: the screen still shows "g".
        assert!(k.line(&t).is_none());
        t.feed(b"i");
        assert_eq!(k.line(&t).unwrap().before, "gi");
    }

    #[test]
    fn no_suggestion_while_keys_typed_ahead_are_not_echoed() {
        let mut t = term();
        let mut k = Tracker::default();
        t.feed(b"$ ");
        k.typed(&t, b"g");
        t.feed(b"g");
        assert_eq!(k.line(&t).unwrap().before, "g");
        // Two keys before the first echo.
        k.typed(&t, b"i");
        k.typed(&t, b"t");
        t.feed(b"i");
        assert!(k.line(&t).is_none(), "t is not echoed yet");
        t.feed(b"t");
        assert_eq!(k.line(&t).unwrap().before, "git");
    }

    #[test]
    fn tracker_drops_the_line_on_ctrl_c() {
        let mut t = term();
        let mut k = Tracker::default();
        t.feed(b"$ ");
        k.typed(&t, b"r");
        t.feed(b"rm -rf build");
        k.typed(&t, &[0x03]);
        t.feed(b"^C\r\n$ ");
        k.typed(&t, b"\r");
        t.feed(b"\r\n$ ");
        assert_eq!(k.poll(&t), None);
    }
}
