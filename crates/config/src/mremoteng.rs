//! Connections of mRemoteNG, from its `confCons.xml`:
//!
//! ```text
//! <mrng:Connections Name="Connections" FullFileEncryption="false" ...>
//!   <Node Name="Work" Type="Container" Username="admin" ...>
//!     <Node Name="web" Type="Connection" Protocol="SSH2" Hostname="web.lan"
//!           Port="22" Username="" InheritUsername="true" Password="..." />
//!   </Node>
//! </mrng:Connections>
//! ```
//!
//! SSH, RDP and VNC connections are imported, in their folders. A file
//! encrypted whole with a master password cannot be read. Passwords are
//! encrypted per connection; they are never read, only noticed.
//!
//! The file is read with a small tag scanner rather than an XML library:
//! only start and end tags and their attributes matter here.

use crate::import::{Session, clean, port};
use crate::settings::SessionKind;
use crate::ssh_config::parse_destination;

/// One tag: its name, its attributes, and whether it closes (`</x>`) or
/// is empty (`<x/>`).
#[derive(Debug, Default)]
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    end: bool,
    empty: bool,
}

impl Tag {
    fn get(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn yes(&self, name: &str) -> bool {
        self.get(name)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }
}

/// Replaces the five named entities and numeric character references.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or_default());
        rest = rest.get(at..).unwrap_or_default();
        let Some(end) = rest.find(';').filter(|&e| e <= 12) else {
            out.push('&');
            rest = rest.get(1..).unwrap_or_default();
            continue;
        };
        let entity = rest.get(1..end).unwrap_or_default();
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e => e
                .strip_prefix("#x")
                .or_else(|| e.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| e.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = rest.get(end + 1..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = rest.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parses what is between `<` and `>`.
fn tag(inner: &str) -> Tag {
    let mut t = Tag::default();
    let mut s = inner.trim();
    if let Some(r) = s.strip_prefix('/') {
        t.end = true;
        s = r;
    }
    if let Some(r) = s.strip_suffix('/') {
        t.empty = true;
        s = r;
    }
    let name_end = s.find(char::is_whitespace).unwrap_or(s.len());
    t.name = s.get(..name_end).unwrap_or_default().to_owned();
    let mut rest = s.get(name_end..).unwrap_or_default();
    while t.attrs.len() < 256 {
        rest = rest.trim_start();
        let Some(eq) = rest.find('=') else {
            break;
        };
        let key = rest.get(..eq).unwrap_or_default().trim().to_owned();
        rest = rest.get(eq + 1..).unwrap_or_default().trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        rest = rest.get(1..).unwrap_or_default();
        let Some(close) = rest.find(quote) else {
            break;
        };
        let value = unescape(rest.get(..close).unwrap_or_default());
        rest = rest.get(close + 1..).unwrap_or_default();
        t.attrs.push((key, value));
    }
    t
}

/// The tags of a document, comments, declarations and text left out.
fn tags(text: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        rest = rest.get(open + 1..).unwrap_or_default();
        let skip_to = if rest.starts_with("!--") {
            Some("-->")
        } else if rest.starts_with("![CDATA[") {
            Some("]]>")
        } else if rest.starts_with('?') || rest.starts_with('!') {
            Some(">")
        } else {
            None
        };
        if let Some(end) = skip_to {
            match rest.find(end) {
                Some(at) => rest = rest.get(at + end.len()..).unwrap_or_default(),
                None => break,
            }
            continue;
        }
        // Attribute values may hold `>`; only one outside quotes ends it.
        let mut quote = None;
        let close = rest.char_indices().find_map(|(i, c)| {
            match (quote, c) {
                (None, '"' | '\'') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, '>') => return Some(i),
                _ => {}
            }
            None
        });
        let Some(close) = close else {
            break;
        };
        out.push(tag(rest.get(..close).unwrap_or_default()));
        rest = rest.get(close + 1..).unwrap_or_default();
        if out.len() >= 100_000 {
            break;
        }
    }
    out
}

/// A folder on the way to a connection.
struct Folder {
    /// `None` for a connection that has children.
    name: Option<String>,
    user: Option<String>,
}

/// The SSH, RDP and VNC connections in a `confCons.xml`.
pub fn from_xml(text: &str) -> Result<Vec<Session>, String> {
    let all = tags(text);
    let root = all
        .iter()
        .find(|t| !t.end && (t.name == "Connections" || t.name.ends_with(":Connections")))
        .ok_or("This is not an mRemoteNG connections file.")?;
    if root.yes("FullFileEncryption") {
        return Err(
            "This mRemoteNG file is encrypted with a master password. In mRemoteNG, turn off \
             \"Encrypt complete connection file\" or export the connections, then import \
             again."
                .into(),
        );
    }
    let mut out = Vec::new();
    let mut stack: Vec<Folder> = Vec::new();
    for t in all.iter().filter(|t| t.name == "Node") {
        if t.end {
            stack.pop();
            continue;
        }
        let container = t.get("Type").is_some_and(|k| k == "Container");
        let inherited = stack.iter().rev().find_map(|f| f.user.clone());
        let user = if t.yes("InheritUsername") {
            inherited
        } else {
            t.get("Username")
                .filter(|u| !u.trim().is_empty())
                .map(|u| u.trim().to_owned())
        };
        let pushed = !t.empty && stack.len() < 64;
        if pushed {
            stack.push(Folder {
                name: container.then(|| t.get("Name").unwrap_or_default().trim().to_owned()),
                user: user.clone(),
            });
        }
        if container {
            continue;
        }
        let folders: Vec<&str> = stack
            .iter()
            .take(stack.len() - usize::from(pushed))
            .filter_map(|f| f.name.as_deref())
            .filter(|n| !n.is_empty())
            .collect();
        if let Some(s) = connection(t, &folders, user) {
            out.push(s);
        }
    }
    Ok(out)
}

fn connection(t: &Tag, folders: &[&str], user: Option<String>) -> Option<Session> {
    let kind = match t.get("Protocol").unwrap_or("RDP") {
        "SSH1" | "SSH2" => SessionKind::Ssh,
        "RDP" => SessionKind::Rdp,
        "VNC" => SessionKind::Vnc,
        "Telnet" => SessionKind::Telnet,
        _ => return None,
    };
    let leaf = t.get("Name")?.trim().replace('/', "-");
    let name = folders
        .iter()
        .map(|f| f.replace('/', "-"))
        .chain([leaf])
        .collect::<Vec<_>>()
        .join("/");
    let (user_in_host, hostname, _) = parse_destination(t.get("Hostname")?.trim())?;
    let mut s = Session::new(name, None, hostname);
    if s.host.alias.is_empty() {
        return None;
    }
    s.kind = kind;
    s.host.port = port(kind, t.get("Port").and_then(|p| p.trim().parse().ok()));
    s.host.user = if kind == SessionKind::Telnet {
        None
    } else {
        user.or(user_in_host)
    };
    s.had_password = t.get("Password").is_some_and(|p| !p.is_empty());
    if kind == SessionKind::Ssh
        && let Some(p) = t
            .get("PuttySession")
            .filter(|p| !p.is_empty() && *p != "Default Settings")
    {
        s.notes
            .push(format!("its PuTTY session {} is not imported", clean(p)));
    }
    if kind == SessionKind::Vnc {
        s.notes
            .push("VNC goes through SSH: pick Via SSH with F2".into());
    }
    Some(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::import::{Client, profile, to_config};
    use crate::{saved, ssh_config};

    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<mrng:Connections xmlns:mrng="http://mremoteng.org" Name="Connections" Export="false"
    EncryptionEngine="AES" FullFileEncryption="false" Protected="abc" ConfVersion="2.6">
  <!-- <Node Name="commented" Hostname="x" Protocol="SSH2" /> -->
  <Node Name="Work &amp; Lab" Type="Container" Expanded="true" Username="admin">
    <Node Name="web" Type="Connection" Protocol="SSH2" Hostname="web.lan" Port="2222"
          Username="" InheritUsername="true" Password="" PuttySession="Default Settings" />
    <Node Name="win &gt; srv" Type="Connection" Protocol="RDP" Hostname="10.0.0.5" Port="3389"
          Username="Administrator" Password="9fZ1+encrypted==" />
    <Node Name="Inner" Type="Container" Username="">
      <Node Name="desk" Type="Connection" Protocol="VNC" Hostname="desk.lan" Port="5901" />
    </Node>
  </Node>
  <Node Name="telnet box" Type="Connection" Protocol="Telnet" Hostname="router" />
  <Node Name="evil" Type="Connection" Protocol="SSH2" Hostname="-oProxyCommand=calc" />
  <Node Name="old" Type="Connection" Hostname="old.lan"></Node>
</mrng:Connections>"#;

    #[test]
    fn reads_ssh_rdp_and_vnc_in_folders() {
        let s = from_xml(XML).unwrap();
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Work & Lab/web",
                "Work & Lab/win > srv",
                "Work & Lab/Inner/desk",
                "telnet box",
                "old"
            ]
        );
        assert_eq!(s[3].kind, SessionKind::Telnet);
        assert_eq!(s[3].host.alias, "telnet-box");
        assert_eq!(s[3].host.port, None);
        assert_eq!(s[0].kind, SessionKind::Ssh);
        assert_eq!(s[0].host.user.as_deref(), Some("admin"));
        assert_eq!(s[0].host.port, Some(2222));
        assert!(s[0].notes.is_empty());
        assert_eq!(s[1].kind, SessionKind::Rdp);
        assert_eq!(s[1].host.alias, "win-srv");
        assert_eq!(s[1].host.port, None);
        assert!(s[1].had_password);
        assert_eq!(s[2].kind, SessionKind::Vnc);
        assert_eq!(s[2].host.port, Some(5901));
        assert_eq!(s[2].host.user.as_deref(), None);
        // No protocol: mRemoteNG's default, RDP.
        assert_eq!(s[4].kind, SessionKind::Rdp);
        let all: Vec<&Session> = s.iter().collect();
        let text = to_config(Client::MRemoteNg, &all);
        let hosts = ssh_config::hosts(&text, &ssh_config::NoIncludes);
        assert_eq!(
            hosts[2].folder.as_deref(),
            Some("mRemoteNG/Work & Lab/Inner")
        );
        assert_eq!(saved::parse(&text).len(), 5);
        assert!(!text.contains("9fZ1"));
        let p = profile(&s[1]).unwrap();
        assert_eq!((p.host.as_str(), p.kind), ("win-srv", SessionKind::Rdp));
        assert!(profile(&s[0]).is_none());
    }

    #[test]
    fn refuses_an_encrypted_or_foreign_file() {
        let enc = r#"<Connections Name="Connections" FullFileEncryption="true">QUJD</Connections>"#;
        assert!(from_xml(enc).unwrap_err().contains("master password"));
        assert!(from_xml("<html><body/></html>").is_err());
        assert!(from_xml("").is_err());
    }

    #[test]
    fn scans_tags_and_entities() {
        assert_eq!(unescape("a&amp;b&lt;&#65;&#x42;&bogus;&"), "a&b<AB&bogus;&");
        let t = tags(r#"<a x="1>2" y='q"'/><!-- <b/> --><c></c>"#);
        let names: Vec<(&str, bool, bool)> = t
            .iter()
            .map(|t| (t.name.as_str(), t.end, t.empty))
            .collect();
        assert_eq!(
            names,
            [("a", false, true), ("c", false, false), ("c", true, false)]
        );
        assert_eq!(t[0].get("x"), Some("1>2"));
        assert_eq!(t[0].get("y"), Some("q\""));
    }
}
