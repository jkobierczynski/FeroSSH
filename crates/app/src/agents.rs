//! Coding agents (Claude Code, Codex, aider and the like) that run in
//! tmux on a server, so they keep working when the connection drops and
//! come back with it (Session > Coding agents).
//!
//! An agent gets a tmux window of its own, named after it and marked with
//! [`crate::tmux::AGENT_OPTION`], in the pane's directory; its command is
//! typed at that window's shell, so the shell stays when the agent exits.
//! When tmux already shows on the pane's host, the window goes into that
//! session ([`crate::tmux::Tmux::start_agent`]); otherwise into session
//! [`SESSION`], made if needed by the line [`start_line`] types, which
//! tmux integration then attaches.
//!
//! This only decides; the window types the lines and shows the state.

use std::time::{Duration, Instant};

use crate::attention::State;
use crate::tmux::{AGENT_OPTION, agent_name};

/// The tmux session agents go into when none shows on the host.
pub const SESSION: &str = "agents";

/// What Session > Coding agents offers unless `AgentCommand` lines say
/// otherwise.
pub const DEFAULT_COMMANDS: [&str; 3] = ["claude", "codex", "aider"];

/// The most commands the menu lists.
pub const MAX_COMMANDS: usize = 16;

/// `text` single-quoted for a POSIX shell (and fish and tcsh).
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The commands the menu offers: the user's, or the defaults. Lines with
/// control characters are left out.
pub fn commands(configured: &[String]) -> Vec<String> {
    let mine: Vec<String> = configured
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty() && !c.chars().any(char::is_control))
        .map(str::to_owned)
        .take(MAX_COMMANDS)
        .collect();
    if mine.is_empty() {
        DEFAULT_COMMANDS.iter().map(|c| (*c).to_owned()).collect()
    } else {
        mine
    }
}

/// What to type at a shell on the server, before the line that attaches
/// session [`SESSION`]: under `sh`, whatever the login shell, it opens a
/// window for `command` in the shell's directory (in a new session
/// [`SESSION`] when there is none), marks it as an agent's and types the
/// command there. Starts with a space, so shells that keep such lines out
/// of their history do; ends in `; ` for the attach.
pub fn start_line(command: &str) -> String {
    let name = sh_quote(&agent_name(command));
    let session = sh_quote(SESSION);
    let target = sh_quote(&format!("={SESSION}:"));
    let exact = sh_quote(&format!("={SESSION}"));
    // The command and its Enter as hex keys: tmux reads nothing in them
    // (a `\;` stays as typed).
    let mut keys = hex_keys(command.trim().as_bytes());
    keys.push_str(" 0d");
    // tmux expands formats in `-c`: a `#` in the directory is doubled.
    // The `_` keeps newlines at the end of the name from `$(…)`.
    // When another client made the session meanwhile, new-session fails
    // and the window goes into that one. The new pane is named by its id,
    // so the keys reach it whichever pane another client selects.
    let script = format!(
        "d=$(printf %s_ \"$PWD\" | sed \"s/#/##/g\"); d=${{d%_}}; p=; \
         tmux has-session -t {exact} 2>/dev/null || \
         p=$(tmux new-session -d -s {session} -c \"$d\" -n {name} -P -F '#{{pane_id}}' 2>/dev/null); \
         [ -n \"$p\" ] || p=$(tmux new-window -d -t {target} -c \"$d\" -n {name} -P -F '#{{pane_id}}'); \
         [ -n \"$p\" ] && tmux set-option -w -t \"$p\" {AGENT_OPTION} {name} \
         && tmux send-keys -t \"$p\" -H{keys}"
    );
    format!(" sh -c {}; ", sh_quote(&script))
}

/// `bytes` as ` 63 6c …`, for `send-keys -H`.
fn hex_keys(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 3);
    for b in bytes {
        let _ = write!(out, " {b:02x}");
    }
    out
}

/// What the session manager says an agent is doing, and since when.
pub fn status(
    wants_you: Option<Instant>,
    state: State,
    now: Instant,
) -> (&'static str, Option<Duration>) {
    if let Some(since) = wants_you {
        return ("needs you", Some(now.saturating_duration_since(since)));
    }
    match state {
        State::Working(since) => ("working", Some(now.saturating_duration_since(since))),
        State::Quiet(since) => ("quiet", Some(now.saturating_duration_since(since))),
        State::Idle => ("idle", None),
    }
}

/// The longest a last line is shown.
const LAST_LINE: usize = 160;

/// What an agent last said, for the dashboard: from the bottom of its
/// screen up, the first line with some words in it, without the frames
/// and rules agents draw around their input box (box-drawing characters,
/// `│ >` prompts) and their key hints. Agents draw their own screens, so
/// this is a good guess, not a reading of what they mean.
pub fn last_line(screen: &[String]) -> Option<String> {
    screen.iter().rev().find_map(|line| {
        let text: String = line
            .chars()
            .map(|c| {
                if ('\u{2500}'..='\u{259f}').contains(&c) || c.is_control() {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let text = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_start_matches([
                '>', '\u{276f}', '$', '#', '%', '*', '\u{2022}', '\u{25cf}', '\u{23fa}', ' ',
            ])
            .to_owned();
        let words = text.chars().filter(|c| c.is_alphanumeric()).count();
        let hint = text.starts_with("? for shortcuts")
            || text.starts_with("esc to interrupt")
            || text.contains("for shortcuts");
        (words >= 3 && !hint).then(|| text.chars().take(LAST_LINE).collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_unless_configured() {
        assert_eq!(commands(&[]), ["claude", "codex", "aider"]);
        let mine = vec![
            " claude --model opus ".to_owned(),
            String::new(),
            "bad\u{1b}[2J".to_owned(),
            "gemini".to_owned(),
        ];
        assert_eq!(commands(&mine), ["claude --model opus", "gemini"]);
    }

    /// Runs `start_line(command)` through a real sh in `cwd`, with a
    /// stand-in tmux (`fake`, a shell script) that logs its arguments
    /// next to itself, in a fresh directory only this user can reach;
    /// what it logged. The line must succeed.
    #[cfg(unix)]
    fn run_start_line(command: &str, fake: &str, cwd: &std::path::Path) -> Result<String, String> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ferossh-agents-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        // Fails when the name is taken: nobody else's directory is used.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        struct Gone(std::path::PathBuf);
        impl Drop for Gone {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Gone(dir);
        let tmux = dir.0.join("tmux");
        std::fs::write(&tmux, fake).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let path = std::env::join_paths(std::iter::once(dir.0.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .map_err(|e| e.to_string())?;
        // The line ends in `; ` for the attach that follows it.
        let line = start_line(command);
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(line.trim_end().trim_end_matches(';'))
            .env("PATH", path)
            .current_dir(cwd)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("{status}"));
        }
        std::fs::read_to_string(dir.0.join("log")).map_err(|e| e.to_string())
    }

    /// Logs each argument in brackets, a line per call.
    #[cfg(unix)]
    const PRINT: &str = "#!/bin/sh\nlog=\"$(dirname \"$0\")/log\"\n\
        for a in \"$@\"; do printf '[%s]' \"$a\" >>\"$log\"; done; echo >>\"$log\"\n";

    #[test]
    fn start_line_quotes_everything() {
        let command = "claude --append-system-prompt 'it''s' done\\;";
        let line = start_line(command);
        assert!(line.starts_with(" sh -c '"), "{line}");
        assert!(line.ends_with("; "), "{line}");
        #[cfg(unix)]
        {
            let fake = format!(
                "{PRINT}[ \"$1\" = has-session ] && exit 1\n[ \"$1\" = new-session ] && echo %7\nexit 0\n"
            );
            let text = run_start_line(command, &fake, std::path::Path::new("/"));
            let keys: String = command
                .bytes()
                .chain(*b"\r")
                .map(|b| format!("[{b:02x}]"))
                .collect();
            assert_eq!(
                text,
                Ok(format!(
                    "[has-session][-t][=agents]\n\
                     [new-session][-d][-s][agents][-c][/][-n][claude][-P][-F][#{{pane_id}}]\n\
                     [set-option][-w][-t][%7][@ferossh-agent][claude]\n\
                     [send-keys][-t][%7][-H]{keys}\n"
                ))
            );
        }
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::indexing_slicing)]
    fn start_line_escapes_formats_and_survives_a_race() {
        // Another client made the session between has-session and
        // new-session: the window goes into that session.
        let fake = format!(
            "{PRINT}[ \"$1\" = has-session ] && exit 1\n[ \"$1\" = new-session ] && exit 1\n\
             [ \"$1\" = new-window ] && echo %8\nexit 0\n"
        );
        let dir =
            std::env::temp_dir().join(format!("ferossh #{{pane_id}} {}\n\n", std::process::id()));
        let made = std::fs::create_dir_all(&dir);
        assert!(made.is_ok(), "{made:?}");
        let text = run_start_line("codex", &fake, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let text = text.unwrap_or_else(|e| e);
        // The name ends in newlines, which reach tmux as they are.
        let shown = dir.display().to_string().replace('#', "##");
        let calls: Vec<&str> = text.split_terminator("]\n").collect();
        assert_eq!(calls.len(), 5, "{text:?}");
        assert!(calls[1].starts_with("[new-session]"), "{text:?}");
        assert_eq!(
            calls[2],
            format!("[new-window][-d][-t][=agents:][-c][{shown}][-n][codex][-P][-F][#{{pane_id}}")
        );
        assert_eq!(calls[3], "[set-option][-w][-t][%8][@ferossh-agent][codex");
        assert_eq!(calls[4], "[send-keys][-t][%8][-H][63][6f][64][65][78][0d");
    }

    #[test]
    fn says_what_an_agent_does() {
        let now = Instant::now();
        let ago = now - Duration::from_secs(90);
        let asked = now - Duration::from_secs(30);
        assert_eq!(
            status(Some(asked), State::Working(ago), now),
            ("needs you", Some(Duration::from_secs(30)))
        );
        assert_eq!(
            status(None, State::Working(ago), now),
            ("working", Some(Duration::from_secs(90)))
        );
        assert_eq!(status(None, State::Quiet(ago), now).0, "quiet");
        assert_eq!(status(None, State::Idle, now), ("idle", None));
    }

    fn screen(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| (*l).to_owned()).collect()
    }

    #[test]
    fn finds_what_an_agent_last_said() {
        // Claude Code style: the answer, then the input box and its hints.
        let shown = screen(&[
            "\u{23fa} I updated src/main.rs and the tests pass.",
            "",
            "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{2502} >                \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
            "  ? for shortcuts",
            "",
        ]);
        assert_eq!(
            last_line(&shown).as_deref(),
            Some("I updated src/main.rs and the tests pass.")
        );
        // A question in the box is what it last said.
        let asked = screen(&[
            "\u{2502} Do you want to run npm test?  \u{2502}",
            "\u{2502} 1. Yes \u{2502}",
        ]);
        assert_eq!(last_line(&asked).as_deref(), Some("1. Yes"));
        // Nothing with words: nothing.
        assert_eq!(last_line(&screen(&["", "> ", "\u{2500}\u{2500}"])), None);
        // Long lines are cut.
        let long = "word ".repeat(100);
        assert_eq!(
            last_line(&[long]).map(|l| l.chars().count()),
            Some(LAST_LINE)
        );
    }
}
