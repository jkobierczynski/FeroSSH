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
    let script = format!(
        "if tmux has-session -t {exact} 2>/dev/null; \
         then w=$(tmux new-window -d -t {target} -c \"$PWD\" -n {name} -P -F '#{{window_id}}'); \
         else w=$(tmux new-session -d -s {session} -c \"$PWD\" -n {name} -P -F '#{{window_id}}'); \
         fi && tmux set-option -w -t \"$w\" {AGENT_OPTION} {name} \
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

    #[test]
    fn start_line_quotes_everything() {
        let line = start_line("claude --append-system-prompt 'it''s'");
        assert!(line.starts_with(" sh -c '"), "{line}");
        assert!(line.ends_with("; "), "{line}");
        if !cfg!(unix) {
            return;
        }
        // Run the script through a real sh with a stand-in tmux that
        // prints its arguments, to check the quoting end to end.
        let dir = std::env::temp_dir().join(format!("ferossh-agents-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let fake = dir.join("tmux");
        std::fs::write(
            &fake,
            "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]' \"$a\" >&2; done; echo >&2\n\
             [ \"$1\" = has-session ] && exit 1\n[ \"$1\" = new-session ] && echo @7\nexit 0\n",
        )
        .ok();
        let Ok(out) = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "chmod +x {0}/tmux; cd /; PATH={0}:$PATH; {1} true",
                dir.display(),
                line
            ))
            .output()
        else {
            return;
        };
        let text = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            text,
            // has-session's own output goes to /dev/null.
            "[new-session][-d][-s][agents][-c][/][-n][claude][-P][-F][#{window_id}]\n\
             [set-option][-w][-t][@7][@ferossh-agent][claude]\n\
             [send-keys][-t][@7][-l][claude --append-system-prompt 'it''s']\n\
             [send-keys][-t][@7][Enter]\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
