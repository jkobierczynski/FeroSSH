//! Sessions of SecureCRT: one `.ini` file per session under its
//! `Config\Sessions` folder, in sub-folders for its folders, with typed
//! lines:
//!
//! ```text
//! S:"Protocol Name"=SSH2
//! S:"Hostname"=web.lan
//! S:"Username"=admin
//! D:"[SSH2] Port"=000008ae
//! S:"Identity Filename V2"=C:\Users\me\.ssh\id_ed25519.pub::rawkey
//! S:"Password V2"=02:...
//! ```
//!
//! `D:` numbers are hexadecimal. Only SSH sessions are imported. Saved
//! passwords are never read, only noticed.

use crate::import::{Session, clean, port};
use crate::settings::SessionKind;
use crate::ssh_config::parse_destination;

/// Whether a file name is one of SecureCRT's own, not a session.
pub fn is_session_file(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".ini") && lower != "__folderdata__.ini" && lower != "default.ini"
}

/// The values in a session file, by name, text as written.
fn values(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines().take(10_000) {
        let line = line.trim_start_matches('\u{feff}');
        let Some(rest) = line
            .strip_prefix("S:\"")
            .or_else(|| line.strip_prefix("D:\""))
        else {
            continue;
        };
        let Some((name, value)) = rest.split_once("\"=") else {
            continue;
        };
        out.push((name.to_owned(), value.trim().to_owned()));
    }
    out
}

/// A session from its file: `name` is its path under `Sessions`, without
/// `.ini`, with `/` between folders.
pub fn session(name: &str, text: &str) -> Option<Session> {
    let values = values(text);
    let get = |key: &str| {
        values
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    };
    let protocol = get("Protocol Name").unwrap_or("SSH2");
    if !protocol.eq_ignore_ascii_case("SSH2") && !protocol.eq_ignore_ascii_case("SSH1") {
        return None;
    }
    let (user_in_host, hostname, _) = parse_destination(get("Hostname")?)?;
    let mut s = Session::new(name.trim_matches('/').to_owned(), None, hostname);
    if s.host.alias.is_empty() {
        return None;
    }
    let port_key = format!("[{}] Port", protocol.to_ascii_uppercase());
    s.host.port = port(
        SessionKind::Ssh,
        get(&port_key).and_then(|p| u32::from_str_radix(p, 16).ok()),
    );
    s.host.user = get("Username").map(str::to_owned).or(user_in_host);
    if let Some(key) = get("Identity Filename V2").or_else(|| get("Identity Filename")) {
        // `path::rawkey` names the key file; a `.pub` beside it is fine.
        let key = key.split("::").next().unwrap_or(key);
        let key = key.strip_suffix(".pub").unwrap_or(key);
        if key.contains("${") {
            s.notes
                .push(format!("its key file {} is not imported", clean(key)));
        } else if !key.is_empty() {
            s.host.identity_file = Some(key.to_owned());
        }
    }
    s.had_password = get("Password V2").or_else(|| get("Password")).is_some();
    if let Some(via) = get("Firewall Name").filter(|f| !f.eq_ignore_ascii_case("None")) {
        s.notes.push(format!(
            "its firewall or jump host {} is not imported",
            clean(via)
        ));
    }
    Some(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::import::{Client, to_config};
    use crate::{saved, ssh_config};

    #[test]
    fn reads_a_session_file() {
        let text = "\u{feff}S:\"Protocol Name\"=SSH2\r\nS:\"Hostname\"=web.lan\r\n\
                    S:\"Username\"=admin\r\nD:\"[SSH2] Port\"=000008ae\r\n\
                    S:\"Identity Filename V2\"=C:\\Keys\\id_ed25519.pub::rawkey\r\n\
                    S:\"Password V2\"=02:abcdef\r\nS:\"Firewall Name\"=None\r\n\
                    Z:\"Port Forward Table V2\"=00000000\r\n";
        let s = session("Work/Prod/web", text).unwrap();
        assert_eq!(s.host.alias, "web");
        assert_eq!(s.host.port, Some(2222));
        assert_eq!(s.host.user.as_deref(), Some("admin"));
        assert_eq!(s.host.identity_file.as_deref(), Some(r"C:\Keys\id_ed25519"));
        assert!(s.had_password);
        assert!(s.notes.is_empty());
        let text = to_config(Client::SecureCrt, &[&s]);
        assert!(!text.contains("abcdef"));
        let hosts = ssh_config::hosts(&text, &ssh_config::NoIncludes);
        assert_eq!(hosts[0].folder.as_deref(), Some("SecureCRT/Work/Prod"));
        assert_eq!(saved::parse(&text)[0].port, Some(2222));
    }

    #[test]
    fn skips_what_is_not_ssh() {
        let telnet = "S:\"Protocol Name\"=Telnet\nS:\"Hostname\"=router\n";
        assert_eq!(session("r", telnet), None);
        assert_eq!(session("x", "S:\"Hostname\"=-oProxyCommand=calc\n"), None);
        let plain = session("box", "S:\"Hostname\"=box\nD:\"[SSH2] Port\"=00000016\n").unwrap();
        assert_eq!(plain.host.port, None);
        let via = session(
            "v",
            "S:\"Hostname\"=v\nS:\"Firewall Name\"=Session:bastion\n",
        )
        .unwrap();
        assert_eq!(
            via.notes,
            ["its firewall or jump host Session:bastion is not imported"]
        );
        assert!(is_session_file("web.ini"));
        assert!(!is_session_file("__FolderData__.ini"));
        assert!(!is_session_file("Default.ini"));
        assert!(!is_session_file("notes.txt"));
    }
}
