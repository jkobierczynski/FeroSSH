//! OpenSSH's escape sequences in an SSH pane: the escape character (`~`
//! unless `EscapeChar` says otherwise) typed first on a line, then a
//! command character. `~.` ends the connection, `~C` opens a small
//! command line for adding and cancelling forwards, `~#` lists them, `~R`
//! asks for a new key exchange, `~?` lists the sequences and `~~` types
//! one `~`.
//!
//! Only keys the user types are read here, never a paste, a script or a
//! suggestion, and the sequence is taken before the key is encoded for
//! the terminal, so the kitty keyboard protocol and tmux's control mode
//! (where the pane is a tmux window) change nothing. Whether a key comes
//! "first on a line" follows everything that went to the host, typed or
//! not ([`Escapes::sent`]), as in ssh, which reads all of its input.
//!
//! The window carries out what [`Escapes::key`] returns; nothing here
//! touches a pane or a connection, so it is tested on any host.

/// The longest `~C` command line.
const MAX_COMMAND: usize = 256;

/// A key typed in the pane, as far as escapes care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed {
    Char(char),
    Enter,
    Backspace,
    Escape,
    /// Any other key: an arrow, a function key, a Ctrl key with no
    /// character.
    Other,
}

/// What an escape sequence asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escape {
    /// `~.`: end the connection.
    Disconnect,
    /// `~?`: list the escape sequences.
    Help,
    /// `~#`: list the forwards and their open connections.
    Forwards,
    /// `~C`: the command line opened.
    CommandLine,
    /// `~R`: a new key exchange.
    Rekey,
    /// `~B`: send a BREAK.
    Break,
    /// `~V` and `~v`: change the log level.
    Verbosity,
    /// `~&` and `~^Z`: send ssh to the background, or suspend it.
    Background,
}

/// What to do with a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not part of an escape: the key goes to the host as usual.
    Pass,
    /// Taken: nothing goes to the host, nothing shows.
    Taken,
    /// The escape character was not followed by a command: it goes to
    /// the host, then the key as usual.
    EscapeThenPass,
    /// `~~`: the escape character goes to the host on its own; the key
    /// is taken.
    SendEscape,
    /// The key is taken; carry out this escape.
    Run(Escape),
    /// The command line shows this (an echoed character, a rub-out).
    Echo(String),
    /// The command line ended: run this text. Empty when it was left
    /// with Escape or Enter on an empty line.
    Command(String),
}

/// One pane's escape state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escapes {
    /// The last thing sent to the host ended a line (or nothing was sent
    /// yet).
    line_start: bool,
    /// The escape character was typed first on a line and is held back.
    pending: bool,
    /// The `~C` command line being typed.
    command: Option<String>,
}

impl Default for Escapes {
    fn default() -> Self {
        Self {
            line_start: true,
            pending: false,
            command: None,
        }
    }
}

impl Escapes {
    /// A fresh connection: the first key is first on a line.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The `~C` command line is open: typing goes to it.
    pub fn in_command(&self) -> bool {
        self.command.is_some()
    }

    /// The escape character is held back, waiting for the next key.
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// Bytes went to the host: the next key is first on a line when they
    /// ended one.
    pub fn sent(&mut self, bytes: &[u8]) {
        if let Some(&last) = bytes.last() {
            self.line_start = matches!(last, b'\r' | b'\n');
        }
    }

    /// Lets go of a held escape character because something other than a
    /// typed key goes to the host (a paste, a script): `true` when the
    /// caller is to send the escape character first, as ssh would.
    pub fn take_pending(&mut self) -> bool {
        std::mem::take(&mut self.pending)
    }

    /// A key typed in the pane, with `escape` the escape character
    /// (`None`: escapes are off).
    pub fn key(&mut self, escape: Option<u8>, typed: Typed) -> Outcome {
        if let Some(line) = &mut self.command {
            return match typed {
                Typed::Char(c) if !c.is_control() => {
                    // One character past the limit is kept, so the line
                    // is refused as too long rather than run cut short.
                    if line.len() > MAX_COMMAND {
                        return Outcome::Taken;
                    }
                    line.push(c);
                    Outcome::Echo(c.to_string())
                }
                Typed::Backspace | Typed::Char('\x08' | '\x7f') => {
                    if line.pop().is_some() {
                        Outcome::Echo("\x08 \x08".into())
                    } else {
                        Outcome::Taken
                    }
                }
                Typed::Enter | Typed::Char('\r' | '\n') => {
                    let line = std::mem::take(line);
                    self.command = None;
                    Outcome::Command(line)
                }
                // Escape and Ctrl+C leave it, as ssh's Ctrl+C does.
                Typed::Escape | Typed::Char('\x03') => {
                    self.command = None;
                    Outcome::Command(String::new())
                }
                _ => Outcome::Taken,
            };
        }
        let Some(escape) = escape else {
            self.pending = false;
            return Outcome::Pass;
        };
        if std::mem::take(&mut self.pending) {
            let Typed::Char(c) = typed else {
                return Outcome::EscapeThenPass;
            };
            if c == char::from(escape) {
                self.line_start = false;
                return Outcome::SendEscape;
            }
            return Outcome::Run(match c {
                '.' => Escape::Disconnect,
                '?' => Escape::Help,
                '#' => Escape::Forwards,
                'C' => {
                    self.command = Some(String::new());
                    Escape::CommandLine
                }
                'R' => Escape::Rekey,
                'B' => Escape::Break,
                'V' | 'v' => Escape::Verbosity,
                '&' | '\x1a' => Escape::Background,
                _ => return Outcome::EscapeThenPass,
            });
        }
        if self.line_start && typed == Typed::Char(char::from(escape)) {
            self.pending = true;
            return Outcome::Taken;
        }
        Outcome::Pass
    }
}

/// `~?`'s list, for the escape character written `e` (`~`, `^]`).
pub fn help(e: &str) -> String {
    let rows = [
        (".", "end the connection"),
        (
            "C",
            "open a command line (-L, -R, -D to add a forward, -KL, -KR, -KD to cancel one)",
        ),
        ("#", "list forwards and their open connections"),
        ("R", "ask the server for a new key exchange"),
        ("?", "this list"),
        ("~", "type the escape character itself (type it twice)"),
    ];
    let mut out = String::from("\r\nSupported escape sequences:\r\n");
    for (key, what) in rows {
        let key = if key == "~" {
            format!("{e}{e}")
        } else {
            format!("{e}{key}")
        };
        out.push_str(&format!(" {key:<4} - {what}\r\n"));
    }
    out.push_str(
        "(Escapes are only recognised right after a new line. EscapeChar in ssh_config or \
         ferossh.conf changes the character; none turns them off.)\r\n",
    );
    out
}

/// What a `~C` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLine {
    /// Nothing typed.
    Nothing,
    /// `?` or `help`.
    Help,
    /// `-L`, `-R` or `-D`: add this forward.
    Add(ferossh_config::ForwardSpec),
    /// `-KL`, `-KR` or `-KD`: cancel the forward it names.
    Cancel(ferossh_config::forward::CancelSpec),
    /// Something else; the text says what is wrong.
    Bad(String),
}

/// Reads a `~C` command line.
pub fn parse_command(line: &str) -> CommandLine {
    if line.len() > MAX_COMMAND {
        return CommandLine::Bad(format!(
            "The command is longer than {MAX_COMMAND} characters."
        ));
    }
    let line = line.trim();
    if line.is_empty() {
        return CommandLine::Nothing;
    }
    if line == "?" || line.eq_ignore_ascii_case("help") || line == "-h" {
        return CommandLine::Help;
    }
    if !line.starts_with('-') {
        return CommandLine::Bad(
            "Commands start with -L, -R, -D, -KL, -KR or -KD (? lists them).".into(),
        );
    }
    if let Some(cancel) = ferossh_config::forward::parse_cancel(line) {
        return CommandLine::Cancel(cancel);
    }
    match ferossh_config::forward::parse_option(line) {
        Some(spec) => CommandLine::Add(spec),
        None => CommandLine::Bad(format!(
            "Not a forward FeroSSH reads: {line} (? lists the commands)."
        )),
    }
}

/// The `~C` command line's own help.
pub const COMMAND_HELP: &str = "Commands:\r\n\
    \x20     -L[bind_address:]port:host:hostport    add a local forward\r\n\
    \x20     -R[bind_address:]port:host:hostport    add a remote forward\r\n\
    \x20     -D[bind_address:]port                  add a dynamic (SOCKS) forward\r\n\
    \x20     -KL[bind_address:]port                 cancel a local forward\r\n\
    \x20     -KR[bind_address:]port                 cancel a remote forward\r\n\
    \x20     -KD[bind_address:]port                 cancel a dynamic forward\r\n";

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const TILDE: Option<u8> = Some(b'~');

    fn typed(e: &mut Escapes, text: &str) -> Vec<Outcome> {
        text.chars()
            .map(|c| match c {
                '\r' => e.key(TILDE, Typed::Enter),
                c => e.key(TILDE, Typed::Char(c)),
            })
            .collect()
    }

    #[test]
    fn tilde_dot_first_on_a_line_disconnects() {
        let mut e = Escapes::default();
        assert_eq!(
            typed(&mut e, "~."),
            [Outcome::Taken, Outcome::Run(Escape::Disconnect)]
        );
    }

    #[test]
    fn only_after_a_new_line() {
        let mut e = Escapes::default();
        assert_eq!(e.key(TILDE, Typed::Char('l')), Outcome::Pass);
        e.sent(b"l");
        assert_eq!(e.key(TILDE, Typed::Char('~')), Outcome::Pass);
        e.sent(b"~");
        assert_eq!(e.key(TILDE, Typed::Char('.')), Outcome::Pass);
        e.sent(b".\r");
        assert_eq!(e.key(TILDE, Typed::Char('~')), Outcome::Taken);
        assert!(e.pending());
        assert_eq!(e.key(TILDE, Typed::Char('?')), Outcome::Run(Escape::Help));
        // Still first on the line: another escape follows without Enter.
        assert_eq!(
            typed(&mut e, "~#"),
            [Outcome::Taken, Outcome::Run(Escape::Forwards)]
        );
        // A pasted line counts too, as ssh reads all its input.
        e.sent(b"echo hi\n");
        assert_eq!(e.key(TILDE, Typed::Char('~')), Outcome::Taken);
    }

    #[test]
    fn double_escape_types_one() {
        let mut e = Escapes::default();
        assert_eq!(typed(&mut e, "~~"), [Outcome::Taken, Outcome::SendEscape]);
        // The line no longer starts here.
        assert_eq!(e.key(TILDE, Typed::Char('~')), Outcome::Pass);
    }

    #[test]
    fn unknown_command_sends_both() {
        let mut e = Escapes::default();
        assert_eq!(
            typed(&mut e, "~x"),
            [Outcome::Taken, Outcome::EscapeThenPass]
        );
        e.sent(b"~x");
        assert!(!e.pending());
        let mut e = Escapes::default();
        e.key(TILDE, Typed::Char('~'));
        assert_eq!(e.key(TILDE, Typed::Other), Outcome::EscapeThenPass);
        let mut e = Escapes::default();
        e.key(TILDE, Typed::Char('~'));
        assert_eq!(e.key(TILDE, Typed::Enter), Outcome::EscapeThenPass);
    }

    #[test]
    fn paste_lets_go_of_a_held_escape() {
        let mut e = Escapes::default();
        e.key(TILDE, Typed::Char('~'));
        assert!(e.take_pending());
        assert!(!e.take_pending());
    }

    #[test]
    fn other_escape_characters_and_none() {
        let mut e = Escapes::default();
        assert_eq!(e.key(Some(0x1d), Typed::Char('~')), Outcome::Pass);
        let mut e = Escapes::default();
        assert_eq!(e.key(Some(0x1d), Typed::Char('\x1d')), Outcome::Taken);
        assert_eq!(
            e.key(Some(0x1d), Typed::Char('.')),
            Outcome::Run(Escape::Disconnect)
        );
        let mut e = Escapes::default();
        assert_eq!(e.key(None, Typed::Char('~')), Outcome::Pass);
        assert_eq!(e.key(None, Typed::Char('.')), Outcome::Pass);
    }

    #[test]
    fn command_line_edits_and_runs() {
        let mut e = Escapes::default();
        assert_eq!(
            typed(&mut e, "~C"),
            [Outcome::Taken, Outcome::Run(Escape::CommandLine)]
        );
        assert!(e.in_command());
        assert_eq!(e.key(TILDE, Typed::Char('-')), Outcome::Echo("-".into()));
        typed(&mut e, "L8080:db:5433");
        assert_eq!(
            e.key(TILDE, Typed::Backspace),
            Outcome::Echo("\x08 \x08".into())
        );
        typed(&mut e, "2");
        assert_eq!(
            e.key(TILDE, Typed::Enter),
            Outcome::Command("-L8080:db:5432".into())
        );
        assert!(!e.in_command());
        // Escape leaves it with nothing to run.
        typed(&mut e, "~C-KL");
        assert_eq!(e.key(TILDE, Typed::Escape), Outcome::Command(String::new()));
        // An empty line rubs out nothing.
        typed(&mut e, "~C");
        assert_eq!(e.key(TILDE, Typed::Backspace), Outcome::Taken);
        assert_eq!(e.key(TILDE, Typed::Other), Outcome::Taken);
        let long = "x".repeat(MAX_COMMAND + 10);
        let echoed = typed(&mut e, &long)
            .into_iter()
            .filter(|o| matches!(o, Outcome::Echo(_)))
            .count();
        assert_eq!(echoed, MAX_COMMAND + 1);
        let Outcome::Command(line) = e.key(TILDE, Typed::Enter) else {
            unreachable!("Enter runs the line");
        };
        assert!(matches!(parse_command(&line), CommandLine::Bad(_)));
        // A command that fits exactly still runs.
        let fits = format!("-L 8080:db:5432{}", " ".repeat(MAX_COMMAND - 15));
        assert_eq!(fits.len(), MAX_COMMAND);
        assert!(matches!(parse_command(&fits), CommandLine::Add(_)));
    }

    #[test]
    fn parses_command_lines() {
        assert_eq!(parse_command("  "), CommandLine::Nothing);
        assert_eq!(parse_command("?"), CommandLine::Help);
        assert!(matches!(
            parse_command("-L 8080:db:5432"),
            CommandLine::Add(spec) if spec.port == 8080
        ));
        assert!(matches!(
            parse_command("-KR 9000"),
            CommandLine::Cancel(c) if c.port == 9000
        ));
        assert!(matches!(
            parse_command("L 8080:db:5432"),
            CommandLine::Bad(_)
        ));
        assert!(matches!(parse_command("-L nonsense"), CommandLine::Bad(_)));
    }

    #[test]
    fn help_names_the_escape_character() {
        let h = help("%");
        assert!(h.contains(" %.   - end the connection"));
        assert!(h.contains(" %%   - type"));
    }
}
