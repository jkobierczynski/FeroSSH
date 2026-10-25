//! One-time import of PuTTY's saved sessions.
//!
//! PuTTY keeps them in the registry under
//! `HKCU\Software\SimonTatham\PuTTY\Sessions`, one key per session with a
//! URL-encoded name. The window reads the values; this module turns them
//! into ssh_config `Host` blocks (written to FeroSSH's own hosts file, never
//! to `~/.ssh/config`) and profiles for the font. Only SSH sessions are
//! imported: telnet, serial and raw sessions have no SSH equivalent.

use std::fmt::Write as _;

use crate::settings::Profile;
use crate::ssh_config::parse_destination;

/// A registry value of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Number(u32),
}

/// What FeroSSH takes from one PuTTY session.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// The session's name as PuTTY shows it.
    pub name: String,
    /// The `Host` name it gets: the name with spaces and odd characters
    /// replaced.
    pub alias: String,
    pub hostname: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    /// A `.ppk` or OpenSSH key file.
    pub key_file: Option<String>,
    pub font: Option<String>,
    pub font_size: Option<f32>,
}

/// Decodes PuTTY's session-name escaping: `%XX` is a byte.
pub fn decode_name(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b, hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A name usable on a `Host` line: letters, digits, `.`, `_` and `-`.
fn alias(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// Builds a session from its values; `None` for non-SSH sessions, the
/// "Default Settings" entry and sessions without a usable host.
pub fn session(raw_name: &str, get: impl Fn(&str) -> Option<Value>) -> Option<Session> {
    let name = decode_name(raw_name);
    if name == "Default Settings" {
        return None;
    }
    let text = |key: &str| match get(key) {
        Some(Value::Text(t)) if !t.trim().is_empty() => Some(t.trim().to_owned()),
        _ => None,
    };
    let number = |key: &str| match get(key) {
        Some(Value::Number(n)) => Some(n),
        _ => None,
    };
    if text("Protocol").is_some_and(|p| !p.eq_ignore_ascii_case("ssh")) {
        return None;
    }
    // HostName may be `user@host`; the destination parser also rejects
    // anything that is not a host name.
    let (user_in_host, hostname, _) = parse_destination(&text("HostName")?)?;
    let alias = alias(&name);
    if alias.is_empty() {
        return None;
    }
    Some(Session {
        alias,
        hostname,
        port: number("PortNumber")
            .and_then(|p| u16::try_from(p).ok())
            .filter(|&p| p != 0 && p != 22),
        user: text("UserName").or(user_in_host),
        key_file: text("PublicKeyFile"),
        font: text("Font"),
        font_size: number("FontHeight")
            .map(|h| h as f32)
            .filter(|h| (4.0..=96.0).contains(h)),
        name,
    })
}

/// Quotes a value for ssh_config if it has spaces.
fn quoted(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .collect();
    if clean.contains([' ', '\t']) {
        format!("\"{clean}\"")
    } else {
        clean
    }
}

/// The hosts file for imported sessions, in ssh_config syntax, under the
/// folder "PuTTY". Sessions whose alias `taken` already names (in the
/// user's ssh_config) are left out, so an import never shadows a host.
pub fn to_config(sessions: &[Session], taken: impl Fn(&str) -> bool) -> String {
    let mut out = String::from(
        "# Imported from PuTTY by FeroSSH. Edit freely; FeroSSH only rewrites it on\n\
         # the next import.\n# folder: PuTTY\n",
    );
    for s in sessions.iter().filter(|s| !taken(&s.alias)) {
        let _ = writeln!(out, "\n# PuTTY session \"{}\"", quoted(&s.name));
        let _ = writeln!(out, "Host {}", s.alias);
        let _ = writeln!(out, "    HostName {}", s.hostname);
        if let Some(port) = s.port {
            let _ = writeln!(out, "    Port {port}");
        }
        if let Some(user) = &s.user {
            let _ = writeln!(out, "    User {}", quoted(user));
        }
        if let Some(key) = &s.key_file {
            let _ = writeln!(out, "    IdentityFile {}", quoted(key));
        }
    }
    out
}

/// The font profile a session brings, if it set a font.
pub fn profile(s: &Session) -> Option<Profile> {
    (s.font.is_some() || s.font_size.is_some()).then(|| Profile {
        host: s.alias.clone(),
        font: s.font.clone(),
        font_size: s.font_size,
        theme: None,
        scrollback: None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::ssh_config;

    fn values<'a>(pairs: &'a [(&'a str, Value)]) -> impl Fn(&str) -> Option<Value> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn decodes_names() {
        assert_eq!(decode_name("Prod%20web%2Fdb"), "Prod web/db");
        assert_eq!(decode_name("100%"), "100%");
        assert_eq!(decode_name("%zz"), "%zz");
        assert_eq!(decode_name("%C3%BC"), "ü");
    }

    #[test]
    fn imports_ssh_sessions() {
        let v = [
            ("Protocol", Value::Text("ssh".into())),
            ("HostName", Value::Text("admin@db.example.org".into())),
            ("PortNumber", Value::Number(2222)),
            ("PublicKeyFile", Value::Text(r"C:\Keys\my key.ppk".into())),
            ("Font", Value::Text("Consolas".into())),
            ("FontHeight", Value::Number(11)),
        ];
        let s = session("Prod%20DB%20(main)", values(&v)).unwrap();
        assert_eq!(s.alias, "Prod-DB-main");
        assert_eq!(s.user.as_deref(), Some("admin"));
        assert_eq!(s.port, Some(2222));
        let text = to_config(std::slice::from_ref(&s), |_| false);
        let cfg = ssh_config::resolve(&text, "Prod-DB-main");
        assert_eq!(cfg.hostname.as_deref(), Some("db.example.org"));
        assert_eq!(cfg.port, Some(2222));
        assert_eq!(cfg.identity_files, [r"C:\Keys\my key.ppk"]);
        let hosts = ssh_config::hosts(&text, &ssh_config::NoIncludes);
        assert_eq!(hosts[0].folder.as_deref(), Some("PuTTY"));
        let p = profile(&s).unwrap();
        assert_eq!(
            (p.font.as_deref(), p.font_size),
            (Some("Consolas"), Some(11.0))
        );
        assert!(!to_config(&[s], |a| a == "Prod-DB-main").contains("Host "));
    }

    #[test]
    fn skips_what_is_not_ssh() {
        let telnet = [
            ("Protocol", Value::Text("telnet".into())),
            ("HostName", Value::Text("router".into())),
        ];
        assert_eq!(session("r", values(&telnet)), None);
        let bad = [("HostName", Value::Text("-oProxyCommand=calc".into()))];
        assert_eq!(session("x", values(&bad)), None);
        let empty = [("HostName", Value::Text(String::new()))];
        assert_eq!(session("x", values(&empty)), None);
        let ok = [("HostName", Value::Text("box".into()))];
        assert_eq!(session("Default%20Settings", values(&ok)), None);
        assert_eq!(session("%%%", values(&ok)), None);
        assert!(session("box", values(&ok)).is_some());
    }
}
