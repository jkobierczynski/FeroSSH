//! Feeds arbitrary bytes, as a hostile server would print them after
//! `mosh-server new`, to the reader of its `MOSH CONNECT <port> <key>`
//! line, and arbitrary text to the settings that make the command line
//! (the Mosh server's program, its ports, a command to run). Nothing may
//! panic; a connect line that reads has a port and a 16-byte key from the
//! line itself; what the server said instead comes back short and
//! without control characters; and a command line, when one is made,
//! names a program the settings allow, quotes the command so the shell
//! reads it as one word, and has no line break.

#![no_main]

use ferossh_config::settings::{mosh_ports, mosh_ports_text, valid_mosh_server};
use ferossh_mosh::bootstrap::{BootError, ServerCommand, parse_connect, valid_program};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    match parse_connect(data) {
        Ok(c) => {
            assert_ne!(c.port, 0);
            // The key is the line's: 22 base64 letters for 16 bytes.
            let text = String::from_utf8_lossy(data);
            assert!(text.contains("MOSH CONNECT "));
            assert_eq!(c.key.len(), 16);
        }
        Err(BootError::NoConnect(said)) => {
            assert!(said.chars().count() <= 400);
            assert!(!said.chars().any(char::is_control));
        }
        Err(BootError::Malformed) => {}
    }

    // The same bytes as the session's Mosh settings: program, ports,
    // command, split on NUL.
    let text = String::from_utf8_lossy(data);
    let mut parts = text.split('\0');
    let program = parts.next().unwrap_or_default();
    let ports = parts.next().unwrap_or_default();
    let command = parts.next();
    assert_eq!(valid_program(program), valid_mosh_server(program));
    let ports = mosh_ports(ports);
    if let Some((low, high)) = ports {
        assert!(low != 0 && low <= high);
        assert_eq!(mosh_ports(&mosh_ports_text((low, high))), Some((low, high)));
    }
    let c = ServerCommand {
        program: program.to_owned(),
        ports,
        command: command.map(str::to_owned),
    };
    match c.line() {
        None => assert!(!valid_program(program)),
        Some(line) => {
            assert!(valid_program(program));
            assert!(line.starts_with(&format!("{program} new ")));
            if let Some(cmd) = command.filter(|c| !c.trim().is_empty()) {
                // One single-quoted word: the shell gives it back as is.
                let (_, quoted) = line.split_once(" -- /bin/sh -c ").expect("command");
                assert_eq!(unquote(quoted).as_deref(), Some(cmd));
            } else {
                assert!(!line.contains(" -- "));
                assert!(!line.contains(['\n', '\r', ';', '&', '|', '`', '$', '\'']));
            }
        }
    }
});

/// What a POSIX shell makes of `s` as one word built only of single
/// quotes and `\'`; `None` if it is anything else.
fn unquote(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("\\'") {
            out.push('\'');
            rest = r;
        } else {
            let r = rest.strip_prefix('\'')?;
            let end = r.find('\'')?;
            out.push_str(r.get(..end)?);
            rest = r.get(end + 1..)?;
        }
    }
    Some(out)
}
