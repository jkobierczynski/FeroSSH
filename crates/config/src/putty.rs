//! Saved sessions of PuTTY and KiTTY, which store them the same way.
//!
//! PuTTY keeps them in the registry under
//! `HKCU\Software\SimonTatham\PuTTY\Sessions`, KiTTY under
//! `HKCU\Software\9bis.com\KiTTY\Sessions` or, portable, as one file per
//! session in a `Sessions` folder, with `Key\Value\` lines. Names are
//! `%XX`-escaped. The window reads the values; this module makes them
//! [`Session`]s. Only SSH sessions are imported: telnet, serial and raw
//! sessions have no SSH equivalent. Passwords (KiTTY can store one) are
//! never read, only noticed.

use crate::forward::{ForwardKind, parse_config};
pub use crate::import::{Session, Value, decode as decode_name};
use crate::ssh_config::parse_destination;

/// The values FeroSSH reads from each session. `Password` is only
/// checked for being there.
pub const VALUES: [&str; 10] = [
    "Protocol",
    "HostName",
    "PortNumber",
    "UserName",
    "PublicKeyFile",
    "Font",
    "FontHeight",
    "PortForwardings",
    "Folder",
    "Password",
];

/// PuTTY's forwards: `L8080=db:5432,R2222=localhost:22,D1080`, each
/// optionally led by `4` or `6`, and the listening side optionally with
/// an address (`L127.0.0.1:8080=...`). Forwards FeroSSH cannot take are
/// left out.
fn forwards(text: &str) -> (Vec<crate::ForwardSpec>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0;
    for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let entry = entry.trim_start_matches(['4', '6']);
        let mut chars = entry.chars();
        let kind = match chars.next() {
            Some('L') => ForwardKind::Local,
            Some('R') => ForwardKind::Remote,
            Some('D') => ForwardKind::Dynamic,
            _ => {
                dropped += 1;
                continue;
            }
        };
        let rest = chars.as_str();
        let (listen, target) = rest.split_once('=').unwrap_or((rest, ""));
        let mut args = vec![listen.to_owned()];
        if !target.is_empty() {
            args.push(target.to_owned());
        }
        match parse_config(kind, &args) {
            Some(spec) if !out.contains(&spec) => out.push(spec),
            Some(_) => {}
            None => dropped += 1,
        }
    }
    (out, dropped)
}

/// Builds a session from its values; `None` for non-SSH sessions, the
/// "Default Settings" entry and sessions without a usable host.
pub fn session(raw_name: &str, get: impl Fn(&str) -> Option<Value>) -> Option<Session> {
    let name = decode_name(raw_name);
    if name == "Default Settings" {
        return None;
    }
    let text = |key: &str| get(key).and_then(|v| v.text().map(str::to_owned));
    let number = |key: &str| get(key).and_then(|v| v.number());
    if text("Protocol").is_some_and(|p| !p.eq_ignore_ascii_case("ssh")) {
        return None;
    }
    // HostName may be `user@host`; the destination parser also rejects
    // anything that is not a host name.
    let (user_in_host, hostname, _) = parse_destination(&text("HostName")?)?;
    // KiTTY's folders; "Default" is where sessions without one are.
    let folder = text("Folder").filter(|f| f != "Default");
    let name = match folder {
        Some(f) => format!("{}/{name}", f.trim_matches(['/', '\\'])),
        None => name,
    };
    let mut s = Session::new(name, None, hostname);
    if s.host.alias.is_empty() {
        return None;
    }
    s.host.port = number("PortNumber")
        .and_then(|p| u16::try_from(p).ok())
        .filter(|&p| p != 0 && p != 22);
    s.host.user = text("UserName").or(user_in_host);
    s.host.identity_file = text("PublicKeyFile");
    if let Some(f) = text("PortForwardings") {
        let (specs, dropped) = forwards(&f);
        s.host.forwards = specs;
        if dropped > 0 {
            s.notes.push(format!(
                "{dropped} port forward{} FeroSSH cannot use",
                if dropped == 1 { "" } else { "s" }
            ));
        }
    }
    s.had_password = get("Password").is_some();
    s.font = text("Font");
    s.font_size = number("FontHeight")
        .map(|h| h as f32)
        .filter(|h| (4.0..=96.0).contains(h));
    Some(s)
}

/// The values in one of KiTTY's portable session files: `Key\Value\`
/// lines. Values may be `%XX`-escaped. `Password` is kept only as being
/// there.
pub fn file_values(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for line in text.lines().take(4096) {
        let line = line.trim_end_matches(['\r', '\n']);
        let line = line.strip_suffix('\\').unwrap_or(line);
        let Some((key, value)) = line.split_once('\\') else {
            continue;
        };
        if !VALUES.contains(&key) {
            continue;
        }
        let value = if key == "Password" {
            Value::Number(1)
        } else {
            Value::Text(decode_name(value))
        };
        out.push((key.to_owned(), value));
    }
    out
}

/// A session from one of KiTTY's portable files, named by its file name.
pub fn file_session(file_name: &str, text: &str) -> Option<Session> {
    let values = file_values(text);
    session(file_name, |k| {
        values
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::import::{Client, profile, to_config};
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
    fn imports_ssh_sessions() {
        let v = [
            ("Protocol", Value::Text("ssh".into())),
            ("HostName", Value::Text("admin@db.example.org".into())),
            ("PortNumber", Value::Number(2222)),
            ("PublicKeyFile", Value::Text(r"C:\Keys\my key.ppk".into())),
            ("Font", Value::Text("Consolas".into())),
            ("FontHeight", Value::Number(11)),
            (
                "PortForwardings",
                Value::Text(
                    "L8080=localhost:80,4R2222=localhost:22,D1080,Lx=y,L127.0.0.1:5433=db:5432"
                        .into(),
                ),
            ),
        ];
        let s = session("Prod%20DB%20(main)", values(&v)).unwrap();
        assert_eq!(s.host.alias, "Prod-DB-main");
        assert_eq!(s.host.user.as_deref(), Some("admin"));
        assert_eq!(s.host.port, Some(2222));
        assert!(!s.had_password);
        let lines: Vec<String> = s.host.forwards.iter().map(|f| f.config_line()).collect();
        assert_eq!(
            lines,
            [
                "LocalForward 8080 localhost:80",
                "RemoteForward 2222 localhost:22",
                "DynamicForward 1080",
                "LocalForward 127.0.0.1:5433 db:5432",
            ]
        );
        assert_eq!(s.notes, ["1 port forward FeroSSH cannot use"]);
        let text = to_config(Client::Putty, &[&s]);
        let cfg = ssh_config::resolve(&text, "Prod-DB-main");
        assert_eq!(cfg.hostname.as_deref(), Some("db.example.org"));
        assert_eq!(cfg.port, Some(2222));
        assert_eq!(cfg.identity_files, [r"C:\Keys\my key.ppk"]);
        assert_eq!(cfg.forwards.len(), 4);
        let hosts = ssh_config::hosts(&text, &ssh_config::NoIncludes);
        assert_eq!(hosts[0].folder.as_deref(), Some("PuTTY"));
        let p = profile(&s).unwrap();
        assert_eq!(
            (p.font.as_deref(), p.font_size),
            (Some("Consolas"), Some(11.0))
        );
    }

    #[test]
    fn reads_kitty_portable_files() {
        let text = "HostName\\web.example.org\\\r\nPortNumber\\2200\\\r\nUserName\\me\\\r\n\
                    PublicKeyFile\\C:%5CKeys%5Cid.ppk\\\r\nFolder\\Work\\\r\nPassword\\0a1b2c\\\r\n\
                    Protocol\\ssh\\\r\n";
        let s = file_session("my%20web", text).unwrap();
        assert_eq!(s.name, "Work/my web");
        assert_eq!(s.host.alias, "my-web");
        assert_eq!(s.host.port, Some(2200));
        assert_eq!(s.host.identity_file.as_deref(), Some(r"C:\Keys\id.ppk"));
        assert!(s.had_password);
        // The stored password is never kept.
        assert!(
            file_values(text)
                .iter()
                .all(|(_, v)| *v != Value::Text("0a1b2c".into()))
        );
        let raw =
            file_session("box", "HostName\\box\\\nPublicKeyFile\\C:\\Keys\\a.ppk\\\n").unwrap();
        assert_eq!(raw.host.identity_file.as_deref(), Some(r"C:\Keys\a.ppk"));
        let folder = to_config(Client::Kitty, &[&s]);
        let hosts = ssh_config::hosts(&folder, &ssh_config::NoIncludes);
        assert_eq!(hosts[0].folder.as_deref(), Some("KiTTY/Work"));
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
