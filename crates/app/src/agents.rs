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
    let line = sh_quote(command.trim());
    let session = sh_quote(SESSION);
    let target = sh_quote(&format!("={SESSION}:"));
    let exact = sh_quote(&format!("={SESSION}"));
    // tmux expands formats in `-c`: a `#` in the directory is doubled.
    // The `_` keeps newlines at the end of the name from `$(…)`.
    // When another client made the session meanwhile, new-session fails
    // and the window goes into that one.
    let script = format!(
        "d=$(printf %s_ \"$PWD\" | sed \"s/#/##/g\"); d=${{d%_}}; w=; \
         tmux has-session -t {exact} 2>/dev/null || \
         w=$(tmux new-session -d -s {session} -c \"$d\" -n {name} -P -F '#{{window_id}}' 2>/dev/null); \
         [ -n \"$w\" ] || w=$(tmux new-window -d -t {target} -c \"$d\" -n {name} -P -F '#{{window_id}}'); \
         [ -n \"$w\" ] && tmux set-option -w -t \"$w\" {AGENT_OPTION} {name} \
         && tmux send-keys -t \"$w\" -l {line} && tmux send-keys -t \"$w\" Enter"
    );
    format!(" sh -c {}; ", sh_quote(&script))
}

/// What the session manager says an agent is doing, and since when.
pub fn status(wants_you: bool, state: State, now: Instant) -> (&'static str, Option<Duration>) {
    if wants_you {
        return ("needs you", None);
    }
    match state {
        State::Working(since) => ("working", Some(now.saturating_duration_since(since))),
        State::Quiet(since) => ("quiet", Some(now.saturating_duration_since(since))),
        State::Idle => ("idle", None),
    }
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
    /// next to itself; what it logged.
    #[cfg(unix)]
    fn run_start_line(command: &str, fake: &str, cwd: &std::path::Path) -> Option<String> {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "ferossh-agents-{}-{}",
            std::process::id(),
            fake.len()
        ));
        std::fs::create_dir_all(&dir).ok()?;
        let tmux = dir.join("tmux");
        std::fs::write(&tmux, fake).ok()?;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).ok()?;
        let path = std::env::join_paths(std::iter::once(dir.clone()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .ok()?;
        let ran = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{} true", start_line(command)))
            .env("PATH", path)
            .current_dir(cwd)
            .output()
            .is_ok();
        let log = std::fs::read_to_string(dir.join("log")).ok();
        let _ = std::fs::remove_dir_all(&dir);
        ran.then_some(log).flatten()
    }

    /// Logs each argument in brackets, a line per call.
    #[cfg(unix)]
    const PRINT: &str = "#!/bin/sh\nlog=\"$(dirname \"$0\")/log\"\n\
        for a in \"$@\"; do printf '[%s]' \"$a\" >>\"$log\"; done; echo >>\"$log\"\n";

    #[test]
    fn start_line_quotes_everything() {
        let line = start_line("claude --append-system-prompt 'it''s'");
        assert!(line.starts_with(" sh -c '"), "{line}");
        assert!(line.ends_with("; "), "{line}");
        #[cfg(unix)]
        {
            let fake = format!(
                "{PRINT}[ \"$1\" = has-session ] && exit 1\n[ \"$1\" = new-session ] && echo @7\nexit 0\n"
            );
            let Some(text) = run_start_line(
                "claude --append-system-prompt 'it''s'",
                &fake,
                std::path::Path::new("/"),
            ) else {
                return;
            };
            assert_eq!(
                text,
                "[has-session][-t][=agents]\n\
                 [new-session][-d][-s][agents][-c][/][-n][claude][-P][-F][#{window_id}]\n\
                 [set-option][-w][-t][@7][@ferossh-agent][claude]\n\
                 [send-keys][-t][@7][-l][claude --append-system-prompt 'it''s']\n\
                 [send-keys][-t][@7][Enter]\n"
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
             [ \"$1\" = new-window ] && echo @8\nexit 0\n"
        );
        let dir =
            std::env::temp_dir().join(format!("ferossh #{{pane_id}} {}\n\n", std::process::id()));
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let Some(text) = run_start_line("codex", &fake, &dir) else {
            return;
        };
        let _ = std::fs::remove_dir_all(&dir);
        // The name ends in newlines, which reach tmux as they are.
        let shown = dir.display().to_string().replace('#', "##");
        let calls: Vec<&str> = text.split_terminator("]\n").collect();
        assert_eq!(calls.len(), 6, "{text:?}");
        assert!(calls[1].starts_with("[new-session]"), "{text:?}");
        assert_eq!(
            calls[2],
            format!(
                "[new-window][-d][-t][=agents:][-c][{shown}][-n][codex][-P][-F][#{{window_id}}"
            )
        );
        assert_eq!(calls[3], "[set-option][-w][-t][@8][@ferossh-agent][codex");
    }

    #[test]
    fn says_what_an_agent_does() {
        let now = Instant::now();
        let ago = now - Duration::from_secs(90);
        assert_eq!(status(true, State::Working(ago), now), ("needs you", None));
        assert_eq!(
            status(false, State::Working(ago), now),
            ("working", Some(Duration::from_secs(90)))
        );
        assert_eq!(status(false, State::Quiet(ago), now).0, "quiet");
        assert_eq!(status(false, State::Idle, now), ("idle", None));
    }
}
