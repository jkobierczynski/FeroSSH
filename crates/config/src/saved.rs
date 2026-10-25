//! The sessions made in FeroSSH's session form, kept in
//! `%APPDATA%\FeroSSH\sessions.conf` in ssh_config syntax:
//!
//! ```text
//! # folder: Work
//! Host web
//!     HostName web.example.org
//!     User admin
//!     Port 2222
//!     IdentityFile "C:\Keys\web key"
//!     LocalForward 8080 localhost:80
//! ```
//!
//! FeroSSH owns this file and rewrites it whole; `~/.ssh/config` is never
//! written. What only FeroSSH understands (the session type, the SSH host
//! a desktop goes through, the look) lives in the host's profile in
//! `ferossh.conf`.

use std::fmt::Write as _;

use crate::forward::{ForwardKind, ForwardSpec, parse_config};
use crate::ssh_config::{folder_comment, parse_destination, split_line};

/// One saved session's connection settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedHost {
    /// The `Host` name, which the session manager lists.
    pub alias: String,
    pub folder: Option<String>,
    /// The machine to reach; the alias when unset.
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub forwards: Vec<ForwardSpec>,
}

/// Whether `name` can be a session's `Host` name: letters, digits, `.`,
/// `_` and `-`, not starting with `-`.
pub fn valid_alias(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether `name` can go on a `HostName` line.
pub fn valid_hostname(name: &str) -> bool {
    !name.contains('@')
        && parse_destination(name).is_some_and(|(_, host, port)| host == name && port.is_none())
}

/// Drops characters that would break the line, quoting what has spaces.
fn quoted(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .collect();
    if clean.contains([' ', '\t', '#', '=']) {
        format!("\"{clean}\"")
    } else {
        clean
    }
}

/// A folder name for a `# folder:` comment: one line, trimmed.
fn folder_text(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Reads the file. Hosts with a name FeroSSH would not write, lines it
/// does not know and `Host` lines with patterns are skipped.
pub fn parse(text: &str) -> Vec<SavedHost> {
    let mut out: Vec<SavedHost> = Vec::new();
    let mut folder = None;
    let mut current: Option<usize> = None;
    for line in text.lines() {
        if let Some(name) = folder_comment(line) {
            folder = (!name.is_empty()).then_some(name);
            continue;
        }
        let Some((keyword, args)) = split_line(line) else {
            continue;
        };
        let first = args.first().cloned();
        if keyword == "host" {
            current = None;
            let [alias] = args.as_slice() else {
                continue;
            };
            if !valid_alias(alias) || out.iter().any(|h| h.alias.eq_ignore_ascii_case(alias)) {
                continue;
            }
            out.push(SavedHost {
                alias: alias.clone(),
                folder: folder.clone(),
                ..SavedHost::default()
            });
            current = Some(out.len() - 1);
            continue;
        }
        let Some(h) = current.and_then(|i| out.get_mut(i)) else {
            continue;
        };
        match keyword.as_str() {
            "hostname" => h.hostname = first.filter(|n| valid_hostname(n)),
            "user" => h.user = first.filter(|u| !u.is_empty()),
            "port" => {
                h.port = first
                    .and_then(|p| p.parse::<u16>().ok())
                    .filter(|&p| p != 0);
            }
            "identityfile" => h.identity_file = first.filter(|f| !f.is_empty()),
            "localforward" | "remoteforward" | "dynamicforward" => {
                let kind = match keyword.as_str() {
                    "localforward" => ForwardKind::Local,
                    "remoteforward" => ForwardKind::Remote,
                    _ => ForwardKind::Dynamic,
                };
                if let Some(spec) = parse_config(kind, &args)
                    && !h.forwards.contains(&spec)
                {
                    h.forwards.push(spec);
                }
            }
            _ => {}
        }
    }
    out
}

/// The file's text.
pub fn to_text(hosts: &[SavedHost]) -> String {
    let mut out = String::from(
        "# Sessions made in FeroSSH's session manager. FeroSSH rewrites this file;\n\
         # comments are not kept. OpenSSH does not read it.\n",
    );
    for h in hosts.iter().filter(|h| valid_alias(&h.alias)) {
        let folder = h.folder.as_deref().map(folder_text).unwrap_or_default();
        let _ = writeln!(out, "\n# folder: {folder}");
        let _ = writeln!(out, "Host {}", h.alias);
        if let Some(name) = h.hostname.as_deref().filter(|n| valid_hostname(n)) {
            let _ = writeln!(out, "    HostName {name}");
        }
        if let Some(user) = &h.user {
            let _ = writeln!(out, "    User {}", quoted(user));
        }
        if let Some(port) = h.port {
            let _ = writeln!(out, "    Port {port}");
        }
        if let Some(key) = &h.identity_file {
            let _ = writeln!(out, "    IdentityFile {}", quoted(key));
        }
        for f in &h.forwards {
            let _ = writeln!(out, "    {}", f.config_line());
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::forward::parse_option;
    use crate::ssh_config;

    fn sample() -> Vec<SavedHost> {
        vec![
            SavedHost {
                alias: "web".into(),
                folder: Some("Work".into()),
                hostname: Some("web.example.org".into()),
                user: Some("admin".into()),
                port: Some(2222),
                identity_file: Some(r"C:\Keys\web key".into()),
                forwards: vec![
                    parse_option("L 8080 localhost:80").unwrap(),
                    parse_option("-D 1080").unwrap(),
                ],
            },
            SavedHost {
                alias: "pi".into(),
                ..SavedHost::default()
            },
        ]
    }

    #[test]
    fn round_trips_and_reads_as_ssh_config() {
        let text = to_text(&sample());
        assert_eq!(parse(&text), sample());
        let cfg = ssh_config::resolve(&text, "web");
        assert_eq!(cfg.hostname.as_deref(), Some("web.example.org"));
        assert_eq!(cfg.port, Some(2222));
        assert_eq!(cfg.identity_files, [r"C:\Keys\web key"]);
        assert_eq!(cfg.forwards, sample()[0].forwards);
        let hosts = ssh_config::hosts(&text, &ssh_config::NoIncludes);
        assert_eq!(hosts[0].folder.as_deref(), Some("Work"));
        assert_eq!(hosts[1].folder, None, "an empty folder comment ends it");
    }

    #[test]
    fn never_writes_a_line_that_could_do_more() {
        let evil = SavedHost {
            alias: "x\nProxyCommand calc".into(),
            ..SavedHost::default()
        };
        assert!(!to_text(&[evil]).contains("Host "));
        let h = SavedHost {
            alias: "ok".into(),
            folder: Some("a\nHost evil".into()),
            hostname: Some("-oProxyCommand=calc".into()),
            user: Some("me\"\nProxyCommand calc".into()),
            identity_file: Some("k\r\nProxyCommand x".into()),
            ..SavedHost::default()
        };
        let text = to_text(&[h]);
        assert!(!text.contains("HostName"));
        assert!(
            text.lines()
                .all(|l| !l.trim_start().starts_with("ProxyCommand"))
        );
        let back = parse(&text);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].folder.as_deref(), Some("aHost evil"));
    }

    #[test]
    fn skips_what_it_does_not_write() {
        let text = "Host *\n User x\nHost a b\n User y\nHost ok\n HostName bad@host\n \
                    Port 0\n Unknown z\nHost OK\n";
        let hosts = parse(text);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "ok");
        assert_eq!(hosts[0].hostname, None);
        assert_eq!(hosts[0].port, None);
        assert!(valid_hostname("10.0.0.1"));
        assert!(valid_hostname("fe80::1"));
        assert!(!valid_hostname("host:22"));
        assert!(!valid_hostname("a b"));
        assert!(!valid_alias("-x"));
        assert!(!valid_alias("a b"));
    }
}
