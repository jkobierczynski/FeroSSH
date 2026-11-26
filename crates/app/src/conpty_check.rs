//! `ferossh.exe --check-conpty`: starts PowerShell on a pseudo console
//! (ConPTY) with FeroSSH's shell integration ([`crate::shell_hook`]), the
//! way a local PowerShell pane runs, types three commands and checks that
//! the OSC 133 marks came through, in order with the text: each command's
//! end, its exit code, its line, and its output between its marks.
//!
//! Older Windows consoles drop the marks or send them ahead of the text
//! they belong to; this says which, on the PC it runs on
//! (`ferossh.exe --check-conpty | more`; the exe has no console of its
//! own). Exit code 0: all marks right; 2: marks came, some in the wrong
//! place; 1: no marks, or PowerShell did not run.
//!
//! [`Check`] does the reading and deciding, so it is tested here against
//! scripted output; [`run`] drives it with a real ConPTY on Windows.

use ferossh_term::{Action, Settings, Terminal};

/// What the check types, the exit code each must end with, and what it
/// must print.
const STEPS: [(&str, i32, &str); 3] = [
    ("Write-Output ferossh-check-one", 0, "ferossh-check-one"),
    ("cmd /c exit 3", 3, ""),
    ("Write-Output ferossh-check-two", 0, "ferossh-check-two"),
];

pub const COLS: usize = 120;
pub const ROWS: usize = 40;

/// The check's reading of the shell's output.
pub struct Check {
    term: Terminal,
    /// The step whose command was typed and whose end is awaited; `None`
    /// before the first prompt.
    step: Option<usize>,
    lines: Vec<String>,
    /// Some mark was missing or out of place.
    wrong: bool,
    /// Every command has ended (or the check gave up).
    done: bool,
}

impl Default for Check {
    fn default() -> Self {
        Self::new()
    }
}

impl Check {
    pub fn new() -> Self {
        Self {
            term: Terminal::new(COLS, ROWS, &Settings::new(1000)),
            step: None,
            lines: Vec::new(),
            wrong: false,
            done: false,
        }
    }

    pub fn done(&self) -> bool {
        self.done
    }

    /// Output from the shell. Returns what to type: the terminal's answers
    /// to the console's queries, and the next command once the one before
    /// ended.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.term.feed(bytes);
        let mut typed = Vec::new();
        let mut ended = Vec::new();
        for action in self.term.take_actions() {
            match action {
                Action::Reply(r) => typed.extend_from_slice(&r),
                Action::CommandFinished(end) => ended.push(end),
                _ => {}
            }
        }
        if self.done {
            return typed;
        }
        let Some(step) = self.step else {
            if self.term.shell_integration() {
                self.lines
                    .push("Prompt marks (OSC 133 A): arrive.".to_owned());
                self.step = Some(0);
                typed.extend_from_slice(Self::typing(0).as_bytes());
            }
            return typed;
        };
        let Some(end) = ended.into_iter().next() else {
            return typed;
        };
        let (line, exit, output) = STEPS.get(step).copied().unwrap_or_default();
        let mut notes = Vec::new();
        if end.exit != Some(exit) {
            notes.push(format!("exit code {:?}, not {exit}", end.exit));
        }
        if end.command.as_deref().map(str::trim) != Some(line) {
            notes.push(format!(
                "command line read as {:?}",
                end.command.as_deref().unwrap_or("nothing")
            ));
        }
        let got = if self.term.select_command_output(true) {
            self.term.selection_text().unwrap_or_default()
        } else {
            String::new()
        };
        self.term.select_clear();
        if got.trim() != output {
            notes.push(format!("output read as {:?}, not {output:?}", got.trim()));
        }
        if notes.is_empty() {
            self.lines
                .push(format!("  {line}: end, exit code, line and output right."));
        } else {
            self.wrong = true;
            self.lines.push(format!("  {line}: {}.", notes.join("; ")));
        }
        let next = step + 1;
        if next < STEPS.len() {
            self.step = Some(next);
            typed.extend_from_slice(Self::typing(next).as_bytes());
        } else {
            self.done = true;
            typed.extend_from_slice(b"exit\r");
        }
        typed
    }

    fn typing(step: usize) -> String {
        format!("{}\r", STEPS.get(step).map_or("", |s| s.0))
    }

    /// The report and the exit code: 0 all right, 2 marks in the wrong
    /// place, 1 no marks (or not all commands ended).
    pub fn report(&self) -> (Vec<String>, i32) {
        let mut lines = self.lines.clone();
        let code = match self.step {
            None => {
                lines.push(
                    "Prompt marks (OSC 133 A): NONE arrived. This Windows' console layer \
                     drops them, so command marks stay off in local PowerShell panes."
                        .to_owned(),
                );
                1
            }
            Some(_) if !self.done => {
                lines.push(
                    "  Command end marks (OSC 133 D): did not all arrive in time.".to_owned(),
                );
                1
            }
            Some(_) if self.wrong => {
                lines.push(
                    "Some marks came out of place: Previous command and Select command \
                     output may land a line off in local PowerShell panes."
                        .to_owned(),
                );
                2
            }
            Some(_) => {
                lines.push("Shell integration works in local PowerShell panes.".to_owned());
                0
            }
        };
        (lines, code)
    }
}

/// Runs the check with a real ConPTY and prints the report.
#[cfg(windows)]
pub fn run() -> i32 {
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    use crate::conpty::{Output, Shell};

    let pwsh = crate::program::find("pwsh.exe", &crate::program::search_dirs()).is_some();
    let base = format!("{} -NoProfile", crate::shell_hook::powershell_command(pwsh));
    let command = crate::shell_hook::with_hook(&base);
    let mut out = std::io::stdout();
    let _ = writeln!(
        out,
        "Checking OSC 133 marks through ConPTY with {}.",
        crate::program::program(&base)
    );
    let shell = match Shell::spawn(&command, COLS, ROWS, None, || {}) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(out, "PowerShell did not start: {}", e.message());
            return 1;
        }
    };
    let mut check = Check::new();
    let mut last = Instant::now();
    let mut exited = false;
    // Each step may take a while on a cold PowerShell start.
    while !exited && last.elapsed() < Duration::from_secs(30) {
        match shell.try_read() {
            Some(Output::Data(bytes)) => {
                let before = check.lines.len();
                let typed = check.feed(&bytes);
                if !typed.is_empty() {
                    shell.write(typed);
                }
                if check.lines.len() != before {
                    last = Instant::now();
                }
            }
            Some(Output::Exited(_)) => exited = true,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
        if check.done() && last.elapsed() > Duration::from_secs(3) {
            break;
        }
    }
    let (lines, code) = check.report();
    for line in &lines {
        let _ = writeln!(out, "{line}");
    }
    let _ = out.flush();
    code
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const E: &str = "\x1b]133;";
    const BEL: &str = "\x07";

    fn prompt(done: Option<i32>) -> String {
        let d = done.map_or(String::new(), |c| format!("{E}D;{c}{BEL}"));
        format!("{d}{E}A{BEL}PS C:\\Users\\me> {E}B{BEL}")
    }

    /// What PowerShell sends for `line` printing `output`, with the marks
    /// in order (`early`: the end mark and the next prompt's marks come
    /// before the output, as an old ConPTY may send them).
    fn command(line: &str, output: &str, exit: i32, early: bool) -> String {
        let out = if output.is_empty() {
            String::new()
        } else {
            format!("{output}\r\n")
        };
        if early {
            format!("{line}\r\n{E}C{BEL}{}{out}", prompt(Some(exit)))
        } else {
            format!("{line}\r\n{E}C{BEL}{out}{}", prompt(Some(exit)))
        }
    }

    fn drive(early: bool) -> (Vec<String>, i32, Vec<u8>) {
        let mut c = Check::new();
        let mut typed = c.feed(b"Windows PowerShell\r\n");
        assert!(typed.is_empty());
        typed = c.feed(prompt(None).as_bytes());
        assert_eq!(typed, b"Write-Output ferossh-check-one\r");
        let mut all = Vec::new();
        for (line, exit, output) in STEPS {
            all = c.feed(command(line, output, exit, early).as_bytes());
        }
        assert!(c.done());
        let (lines, code) = c.report();
        (lines, code, all)
    }

    #[test]
    fn marks_in_order_pass() {
        let (lines, code, typed) = drive(false);
        assert_eq!(code, 0, "{lines:#?}");
        assert_eq!(typed, b"exit\r");
        assert!(lines.last().unwrap().contains("works"));
    }

    #[test]
    fn marks_ahead_of_the_text_are_caught() {
        let (lines, code, _) = drive(true);
        assert_eq!(code, 2, "{lines:#?}");
        assert!(
            lines.iter().any(|l| l.contains("output read as")),
            "{lines:#?}"
        );
    }

    #[test]
    fn no_marks_at_all() {
        let mut c = Check::new();
        assert!(c.feed(b"PS C:\\> ").is_empty());
        let (lines, code) = c.report();
        assert_eq!(code, 1);
        assert!(lines.last().unwrap().contains("NONE"));
    }

    #[test]
    fn a_wrong_exit_code_is_reported() {
        let mut c = Check::new();
        c.feed(prompt(None).as_bytes());
        c.feed(command(STEPS[0].0, STEPS[0].2, 0, false).as_bytes());
        // cmd's exit code lost: PowerShell said 1.
        c.feed(command(STEPS[1].0, "", 1, false).as_bytes());
        c.feed(command(STEPS[2].0, STEPS[2].2, 0, false).as_bytes());
        let (lines, code) = c.report();
        assert_eq!(code, 2);
        assert!(lines.iter().any(|l| l.contains("exit code Some(1), not 3")));
    }

    #[test]
    fn the_terminal_answers_the_console() {
        let mut c = Check::new();
        // ConPTY asks where the cursor is.
        let typed = c.feed(b"\x1b[6n");
        assert_eq!(typed, b"\x1b[1;1R");
    }
}
