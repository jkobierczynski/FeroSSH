//! Starting `mosh-server` over SSH, as the `mosh` script does: run
//! `mosh-server new` on an exec channel and read the port and session key
//! from the `MOSH CONNECT <port> <key>` line it prints before it detaches.

use zeroize::Zeroizing;

use crate::crypto::{CryptoError, parse_key};

/// The most output read while waiting for the `MOSH CONNECT` line.
pub const MAX_OUTPUT: usize = 64 * 1024;

/// What to start on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommand {
    /// The program: `mosh-server`, or a path to it.
    pub program: String,
    /// The UDP port, or the range of ports, it may listen on.
    pub ports: Option<(u16, u16)>,
    /// A command to run instead of the login shell.
    pub command: Option<String>,
}

impl Default for ServerCommand {
    fn default() -> Self {
        Self {
            program: "mosh-server".into(),
            ports: None,
            command: None,
        }
    }
}

/// Whether `program` is a plain program name or path, safe to put on a
/// shell command line unquoted.
pub fn valid_program(program: &str) -> bool {
    !program.is_empty()
        && program.len() <= 256
        && program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'~' | b'+'))
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl ServerCommand {
    /// The command line to run over SSH, or `None` for an unsafe program
    /// name. `-s` binds to the address the SSH connection came in on;
    /// 256 colours; a UTF-8 locale when the server's own is not one.
    pub fn line(&self) -> Option<String> {
        if !valid_program(&self.program) {
            return None;
        }
        let mut line = format!("{} new -s -c 256 -l LANG=C.UTF-8", self.program);
        if let Some((low, high)) = self.ports {
            if low == high {
                line.push_str(&format!(" -p {low}"));
            } else {
                line.push_str(&format!(" -p {low}:{high}"));
            }
        }
        if let Some(cmd) = self.command.as_deref().filter(|c| !c.trim().is_empty()) {
            line.push_str(" -- /bin/sh -c ");
            line.push_str(&quote(cmd));
        }
        Some(line)
    }
}

/// The server's answer to `mosh-server new`.
#[derive(Debug)]
pub struct Connect {
    pub port: u16,
    pub key: Zeroizing<[u8; 16]>,
}

/// Why `mosh-server` did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootError {
    /// No `MOSH CONNECT` line: what the server printed instead (trimmed).
    NoConnect(String),
    /// The line's port or key is malformed.
    Malformed,
}

impl From<CryptoError> for BootError {
    fn from(_: CryptoError) -> Self {
        BootError::Malformed
    }
}

/// Finds the `MOSH CONNECT <port> <key>` line in `mosh-server`'s output.
pub fn parse_connect(output: &[u8]) -> Result<Connect, BootError> {
    let text = String::from_utf8_lossy(output);
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some(rest) = line.strip_prefix("MOSH CONNECT ") else {
            continue;
        };
        let mut parts = rest.split(' ');
        let (Some(port), Some(key), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(BootError::Malformed);
        };
        let port: u16 = port.parse().map_err(|_| BootError::Malformed)?;
        if port == 0 {
            return Err(BootError::Malformed);
        }
        let key = parse_key(key)?;
        return Ok(Connect { port, key });
    }
    // The first lines of what it said, for the user.
    let said: String = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(400)
        .collect();
    Err(BootError::NoConnect(said))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn command_lines() {
        let mut c = ServerCommand::default();
        assert_eq!(c.line().unwrap(), "mosh-server new -s -c 256 -l LANG=C.UTF-8");
        c.ports = Some((60001, 60010));
        c.command = Some("tmux new -A -s it's".into());
        assert_eq!(
            c.line().unwrap(),
            "mosh-server new -s -c 256 -l LANG=C.UTF-8 -p 60001:60010 -- /bin/sh -c 'tmux new -A -s it'\\''s'"
        );
        c.program = "/opt/mosh/bin/mosh-server; rm -rf".into();
        assert_eq!(c.line(), None);
        c.program = "~/bin/mosh-server".into();
        assert!(c.line().is_some());
    }

    #[test]
    fn reads_the_connect_line() {
        let out = b"\r\n\r\nMOSH CONNECT 60001 AAECAwQFBgcICQoLDA0ODw\r\n\r\nmosh-server (mosh 1.4.0)\r\n";
        let c = parse_connect(out).unwrap();
        assert_eq!(c.port, 60001);
        assert_eq!(c.key[15], 15);
        assert_eq!(
            parse_connect(b"MOSH CONNECT 0 AAECAwQFBgcICQoLDA0ODw\n").unwrap_err(),
            BootError::Malformed
        );
        assert_eq!(
            parse_connect(b"MOSH CONNECT 60001 short\n").unwrap_err(),
            BootError::Malformed
        );
        assert_eq!(
            parse_connect(b"bash: mosh-server: command not found\n").unwrap_err(),
            BootError::NoConnect("bash: mosh-server: command not found".into())
        );
    }
}
