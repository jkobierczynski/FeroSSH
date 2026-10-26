//! Sessions of MobaXterm, from the bookmarks in its `MobaXterm.ini` (or
//! an exported `.mxtsessions` file, which has the same sections):
//!
//! ```text
//! [Bookmarks_1]
//! SubRep=Work\Prod
//! ImgNum=41
//! web=#109#0%web.lan%2222%admin%%-1%-1%%%%%0%0%0%_ProfileDir_\.ssh\id%%...#MobaFont%10...#0# #-1
//! ```
//!
//! After the icon number comes a `%`-separated group whose first field is
//! the session type: 0 SSH, 4 RDP, 5 VNC, 7 SFTP. Host, port and user
//! follow; an SSH session's key file is field 14. SSH, SFTP, RDP and VNC
//! sessions are imported. MobaXterm keeps passwords elsewhere; they are
//! never read.

use crate::import::{Session, clean, port};
use crate::settings::SessionKind;
use crate::ssh_config::parse_destination;

/// MobaXterm's escapes for characters its format uses itself.
fn unescape(text: &str) -> String {
    text.replace("__PTVIRG__", ";")
        .replace("__DIEZE__", "#")
        .replace("__DBLQUO__", "\"")
        .replace("__PIPE__", "|")
        .replace("__PERCENT__", "%")
}

/// One bookmark line, `name=#icon#fields#...`, in `folder`.
fn bookmark(folder: &str, name: &str, value: &str) -> Option<Session> {
    let (_, rest) = value.split_once('#')?;
    let mut groups = rest.split('#');
    let _icon = groups.next()?;
    let fields: Vec<String> = groups.next()?.split('%').map(unescape).collect();
    let field = |i: usize| fields.get(i).map(|f| f.trim()).filter(|f| !f.is_empty());
    let (kind, gateway) = match field(0)? {
        "0" | "7" => (SessionKind::Ssh, 8),
        "4" => (SessionKind::Rdp, 13),
        "5" => (SessionKind::Vnc, 5),
        _ => return None,
    };
    let leaf = unescape(name.trim()).replace('/', "-");
    let name = if folder.is_empty() {
        leaf
    } else {
        format!("{folder}/{leaf}")
    };
    let (user_in_host, hostname, _) = parse_destination(field(1)?)?;
    let mut s = Session::new(name, None, hostname);
    if s.host.alias.is_empty() {
        return None;
    }
    s.kind = kind;
    s.host.port = port(kind, field(2).and_then(|p| p.parse().ok()));
    s.host.user = if kind == SessionKind::Vnc {
        None
    } else {
        field(3).map(str::to_owned).or(user_in_host)
    };
    if kind == SessionKind::Ssh
        && let Some(key) = field(14)
    {
        // `_ProfileDir_` is the user's home folder; MobaXterm's other
        // place names have no equivalent here.
        let key = key.replace("_ProfileDir_", "~");
        if key.contains("_CurrentDrive_") || key.starts_with('_') {
            s.notes
                .push(format!("its key file {} is not imported", clean(&key)));
        } else {
            s.host.identity_file = Some(key);
        }
    }
    if let Some(via) = field(gateway) {
        let via = clean(&via.replace('|', ", "));
        s.notes.push(match kind {
            SessionKind::Ssh => format!("its jump host {via} is not imported"),
            _ => format!("it went through {via}: pick Via SSH with F2"),
        });
    } else if kind == SessionKind::Vnc {
        s.notes
            .push("VNC goes through SSH: pick Via SSH with F2".into());
    }
    Some(s)
}

/// The SSH, SFTP, RDP and VNC sessions in a `MobaXterm.ini`.
pub fn from_ini(text: &str) -> Vec<Session> {
    let mut out = Vec::new();
    let mut in_bookmarks = false;
    let mut folder = String::new();
    for line in text.lines().take(1_000_000) {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_bookmarks = section == "Bookmarks" || section.starts_with("Bookmarks_");
            folder.clear();
            continue;
        }
        if !in_bookmarks {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "SubRep" => {
                folder = unescape(value.trim())
                    .split('\\')
                    .map(|f| f.trim().replace('/', "-"))
                    .filter(|f| !f.is_empty())
                    .collect::<Vec<_>>()
                    .join("/");
            }
            "ImgNum" => {}
            name => {
                if let Some(s) = bookmark(&folder, name, value) {
                    out.push(s);
                }
            }
        }
        if out.len() >= 10_000 {
            break;
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::import::{Client, to_config};
    use crate::{saved, ssh_config};

    const INI: &str = "\
[Misc]\r
web=#109#0%not-a-bookmark%22%x\r
[Bookmarks]\r
SubRep=\r
ImgNum=42\r
web=#109#0%web.lan%2222%admin%%-1%-1%%%22%%0%0%0%_ProfileDir_\\.ssh\\id_ed25519%%-1%0%0%0%%1080%%0%0%1#MobaFont%10%0#0# #-1\r
[Bookmarks_1]\r
SubRep=Work\\Prod\r
ImgNum=41\r
win srv=#91#4%10.0.0.5%3389%Administrator%0%-1%-1%-1%-1%0%0%-1%%%%%%%%%%#MobaFont%10#0# #-1\r
desk=#128#5%desk.lan%5901%-1%0%bastion.lan%22%me%%0%#MobaFont#0# #-1\r
inner=#109#0%inner.lan%22%%%-1%-1%%jump1.lan__PIPE__jump2.lan%22__PIPE__22%%%0%0%%#MobaFont#0# #-1\r
tel=#98#1%router%23%%%2%%#MobaFont#0# #-1\r
bad=#109#0%-oProxyCommand=calc%22%%#x#0#\r
";

    #[test]
    fn reads_bookmarks_in_folders() {
        let s = from_ini(INI);
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "web",
                "Work/Prod/win srv",
                "Work/Prod/desk",
                "Work/Prod/inner"
            ]
        );
        assert_eq!(s[0].kind, SessionKind::Ssh);
        assert_eq!(s[0].host.port, Some(2222));
        assert_eq!(s[0].host.user.as_deref(), Some("admin"));
        assert_eq!(
            s[0].host.identity_file.as_deref(),
            Some(r"~\.ssh\id_ed25519")
        );
        assert!(s[0].notes.is_empty());
        assert_eq!(s[1].kind, SessionKind::Rdp);
        assert_eq!(s[1].host.port, None);
        assert_eq!(s[1].host.user.as_deref(), Some("Administrator"));
        assert_eq!(s[2].kind, SessionKind::Vnc);
        assert_eq!(s[2].host.port, Some(5901));
        assert_eq!(
            s[2].notes,
            ["it went through bastion.lan: pick Via SSH with F2"]
        );
        assert_eq!(
            s[3].notes,
            ["its jump host jump1.lan, jump2.lan is not imported"]
        );
        let all: Vec<&Session> = s.iter().collect();
        let text = to_config(Client::MobaXterm, &all);
        let hosts = ssh_config::hosts(&text, &ssh_config::NoIncludes);
        assert_eq!(hosts[1].folder.as_deref(), Some("MobaXterm/Work/Prod"));
        assert_eq!(saved::parse(&text).len(), 4);
    }
}
