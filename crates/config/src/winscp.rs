//! Saved sites of WinSCP, from the registry
//! (`HKCU\Software\Martin Prikryl\WinSCP 2\Sessions`) or a `WinSCP.ini`
//! (`[Sessions\Folder/Site]` sections).
//!
//! Site names and text values are `%XX`-escaped, folders are separated by
//! `/`. Only SFTP and SCP sites are imported: FTP, WebDAV and S3 have no
//! SSH login. Workspaces and site shortcuts are skipped. Passwords are
//! never read, only noticed.

use crate::import::{Session, Value, decode};
use crate::ssh_config::parse_destination;

/// The values FeroSSH reads from each site. The password values are only
/// checked for being there.
pub const VALUES: [&str; 12] = [
    "HostName",
    "UserName",
    "PortNumber",
    "FSProtocol",
    "PublicKeyFile",
    "Tunnel",
    "TunnelHostName",
    "TunnelPortNumber",
    "IsWorkspace",
    "Link",
    "Password",
    "PasswordPlain",
];

/// WinSCP's protocols that log in over SSH: SCP, SFTP with SCP fallback
/// (its default) and SFTP.
const SSH_PROTOCOLS: [u32; 3] = [0, 1, 2];

/// Builds a session from a site's values; `None` for what is not an SSH
/// site, the default settings, workspaces and site shortcuts.
pub fn session(raw_name: &str, get: impl Fn(&str) -> Option<Value>) -> Option<Session> {
    let name = decode(raw_name).trim_matches('/').to_owned();
    if name.is_empty() || name == "Default Settings" {
        return None;
    }
    let text = |key: &str| {
        get(key)
            .and_then(|v| v.text().map(decode))
            .filter(|t| !t.trim().is_empty())
    };
    let number = |key: &str| get(key).and_then(|v| v.number());
    if number("IsWorkspace").is_some_and(|w| w != 0) || text("Link").is_some() {
        return None;
    }
    if !SSH_PROTOCOLS.contains(&number("FSProtocol").unwrap_or(1)) {
        return None;
    }
    // HostName may be `user@host`.
    let (user_in_host, hostname, _) = parse_destination(text("HostName")?.trim())?;
    let mut s = Session::new(name, None, hostname);
    if s.host.alias.is_empty() {
        return None;
    }
    s.host.port = number("PortNumber")
        .and_then(|p| u16::try_from(p).ok())
        .filter(|&p| p != 0 && p != 22);
    s.host.user = text("UserName").or(user_in_host);
    s.host.identity_file = text("PublicKeyFile");
    s.had_password = get("Password").is_some() || get("PasswordPlain").is_some();
    if number("Tunnel").is_some_and(|t| t != 0)
        && let Some(via) = text("TunnelHostName")
    {
        let via: String = via.chars().filter(|c| !c.is_control()).take(100).collect();
        s.notes
            .push(format!("its tunnel through {via} is not imported"));
    }
    Some(s)
}

/// The SSH sites in a `WinSCP.ini`.
pub fn from_ini(text: &str) -> Vec<Session> {
    let mut out = Vec::new();
    let mut current: Option<(String, Vec<(String, Value)>)> = None;
    let finish = |site: Option<(String, Vec<(String, Value)>)>, out: &mut Vec<Session>| {
        if let Some((name, values)) = site
            && let Some(s) = session(&name, |k| {
                values
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.clone())
            })
        {
            out.push(s);
        }
    };
    for line in text.lines().take(1_000_000) {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            finish(current.take(), &mut out);
            current = section
                .strip_prefix("Sessions\\")
                .map(|name| (name.to_owned(), Vec::new()));
            if out.len() >= 10_000 {
                break;
            }
            continue;
        }
        let Some((_, values)) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !VALUES.contains(&key) || values.len() >= VALUES.len() {
            continue;
        }
        let value = if key.starts_with("Password") {
            Value::Number(1)
        } else {
            Value::Text(value.trim().to_owned())
        };
        values.push((key.to_owned(), value));
    }
    finish(current, &mut out);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::import::{Client, to_config};
    use crate::{saved, ssh_config};

    const INI: &str = "\
[Configuration\\Interface]\r
HostName=not-a-site\r
\r
[Sessions\\Default%20Settings]\r
HostName=defaults\r
\r
[Sessions\\Work/Prod%20web]\r
HostName=web.example.org\r
UserName=admin\r
PortNumber=2222\r
PublicKeyFile=C:%5CKeys%5Cweb%20key.ppk\r
Password=A35C755E6D\r
Tunnel=1\r
TunnelHostName=bastion.example.org\r
\r
[Sessions\\ftp]\r
HostName=ftp.example.org\r
FSProtocol=5\r
\r
[Sessions\\scp%20box]\r
HostName=root@box.lan\r
FSProtocol=0\r
\r
[Sessions\\My%20workspace/0000]\r
HostName=w\r
IsWorkspace=1\r
\r
[Sessions\\bad]\r
HostName=-oProxyCommand=calc\r
";

    #[test]
    fn reads_ssh_sites_from_the_ini() {
        let s = from_ini(INI);
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Work/Prod web", "scp box"]);
        let web = &s[0];
        assert_eq!(web.host.alias, "Prod-web");
        assert_eq!(web.host.hostname.as_deref(), Some("web.example.org"));
        assert_eq!(web.host.user.as_deref(), Some("admin"));
        assert_eq!(web.host.port, Some(2222));
        assert_eq!(
            web.host.identity_file.as_deref(),
            Some(r"C:\Keys\web key.ppk")
        );
        assert!(web.had_password);
        assert_eq!(
            web.notes,
            ["its tunnel through bastion.example.org is not imported"]
        );
        assert_eq!(s[1].host.user.as_deref(), Some("root"));
        assert!(!s[1].had_password);
        let text = to_config(Client::WinScp, &[&s[0], &s[1]]);
        assert!(!text.contains("A35C"));
        let hosts = ssh_config::hosts(&text, &ssh_config::NoIncludes);
        assert_eq!(hosts[0].folder.as_deref(), Some("WinSCP/Work"));
        assert_eq!(hosts[1].folder.as_deref(), Some("WinSCP"));
        let back = saved::parse(&text);
        assert_eq!(
            back[0].identity_file.as_deref(),
            Some(r"C:\Keys\web key.ppk")
        );
    }

    #[test]
    fn reads_registry_values() {
        let get = |k: &str| match k {
            "HostName" => Some(Value::Text("files.lan".into())),
            "PortNumber" => Some(Value::Number(22)),
            "FSProtocol" => Some(Value::Number(2)),
            "PasswordPlain" => Some(Value::Number(1)),
            _ => None,
        };
        let s = session("Home/files", get).unwrap();
        assert_eq!((s.name.as_str(), s.host.port), ("Home/files", None));
        assert!(s.had_password);
        let webdav = |k: &str| match k {
            "HostName" => Some(Value::Text("dav".into())),
            "FSProtocol" => Some(Value::Number(6)),
            _ => None,
        };
        assert_eq!(session("dav", webdav), None);
    }
}
