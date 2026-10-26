//! Saved sessions brought over from other SSH clients: PuTTY and KiTTY
//! (registry, or KiTTY's portable `Sessions` folder) and WinSCP (registry
//! or `WinSCP.ini`).
//!
//! The window reads what the client stored; [`crate::putty`] and
//! [`crate::winscp`] turn it into [`Session`]s, and [`to_config`] writes
//! the ones the user ticked to that client's own file in FeroSSH's folder,
//! in ssh_config syntax. Each client has one file, so importing again
//! replaces the last import cleanly; `~/.ssh/config` is never written.
//!
//! Passwords are never read. A session that had one stored is only
//! flagged, so the user can set up a password source for it.

use crate::saved::{SavedHost, valid_alias, write_host};
use crate::settings::Profile;

/// A client FeroSSH imports from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Client {
    Putty,
    Kitty,
    WinScp,
}

impl Client {
    pub const ALL: [Client; 3] = [Client::Putty, Client::Kitty, Client::WinScp];

    /// The name the user knows it by.
    pub fn name(self) -> &'static str {
        match self {
            Client::Putty => "PuTTY",
            Client::Kitty => "KiTTY",
            Client::WinScp => "WinSCP",
        }
    }

    /// The file its sessions go to, in FeroSSH's folder.
    pub fn file_name(self) -> &'static str {
        match self {
            // The name the first PuTTY import used.
            Client::Putty => "putty-sessions.conf",
            Client::Kitty => "imported-kitty.conf",
            Client::WinScp => "imported-winscp.conf",
        }
    }
}

/// A stored value of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Number(u32),
}

impl Value {
    /// The text, trimmed; `None` when empty or a number.
    pub fn text(&self) -> Option<&str> {
        match self {
            Value::Text(t) if !t.trim().is_empty() => Some(t.trim()),
            _ => None,
        }
    }

    /// The number; files keep numbers as decimal text.
    pub fn number(&self) -> Option<u32> {
        match self {
            Value::Number(n) => Some(*n),
            Value::Text(t) => t.trim().parse().ok(),
        }
    }
}

/// One session as it would be imported.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// The name the client shows, with its folders (`Work/web`).
    pub name: String,
    /// What goes to the file; the folder is set by [`to_config`].
    pub host: SavedHost,
    /// The client had a password stored for it (not read).
    pub had_password: bool,
    /// What the client had that does not come over.
    pub notes: Vec<String>,
    pub font: Option<String>,
    pub font_size: Option<f32>,
}

impl Session {
    pub(crate) fn new(name: String, folder: Option<String>, hostname: String) -> Self {
        let leaf = name.rsplit('/').next().unwrap_or(&name);
        Self {
            host: SavedHost {
                alias: alias(leaf),
                folder,
                hostname: Some(hostname),
                ..SavedHost::default()
            },
            name,
            had_password: false,
            notes: Vec::new(),
            font: None,
            font_size: None,
        }
    }
}

/// Decodes `%XX` escapes, as PuTTY and WinSCP write names (and WinSCP its
/// values). Text starting with an encoded UTF-8 byte order mark is UTF-8;
/// other text is read as UTF-8 when it is, else as Latin-1.
pub fn decode(raw: &str) -> String {
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
    let out = out
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .map(<[u8]>::to_vec)
        .unwrap_or(out);
    match String::from_utf8(out) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| char::from(b)).collect(),
    }
}

/// A name usable on a `Host` line: letters, digits, `.`, `_` and `-`.
pub fn alias(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(60).collect();
    out.trim_end_matches('-').to_owned()
}

/// Gives the sessions of one import distinct aliases. A name used twice
/// (`Work/web` and `Home/web`) takes its folders into the alias, then a
/// number.
pub fn distinct(sessions: &mut [Session]) {
    let mut used: Vec<String> = Vec::new();
    for s in sessions.iter_mut() {
        let mut a = s.host.alias.clone();
        if used.contains(&a.to_ascii_lowercase()) {
            a = alias(&s.name.replace('/', "-"));
        }
        let base = a.clone();
        let mut n = 2;
        while used.contains(&a.to_ascii_lowercase()) {
            a = format!("{base}-{n}");
            n += 1;
        }
        used.push(a.to_ascii_lowercase());
        s.host.alias = a;
    }
}

/// The text of `client`'s file with `sessions`, in folders under the
/// client's name.
pub fn to_config(client: Client, sessions: &[&Session]) -> String {
    let mut out = format!(
        "# Imported from {} by FeroSSH. A new import from {0} replaces this file.\n\
         # OpenSSH does not read it.\n",
        client.name()
    );
    for s in sessions.iter().filter(|s| valid_alias(&s.host.alias)) {
        let mut host = s.host.clone();
        host.folder = Some(match s.name.rsplit_once('/') {
            Some((folders, _)) if !folders.trim().is_empty() => {
                format!("{}/{}", client.name(), folders.trim())
            }
            _ => client.name().to_owned(),
        });
        let comment = format!("{} session \"{}\"", client.name(), s.name);
        write_host(&mut out, &host, Some(&comment));
    }
    out
}

/// The font profile a session brings, if it set a font.
pub fn profile(s: &Session) -> Option<Profile> {
    (s.font.is_some() || s.font_size.is_some()).then(|| Profile {
        host: s.host.alias.clone(),
        font: s.font.clone(),
        font_size: s.font_size,
        ..Profile::default()
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::saved;
    use crate::ssh_config;

    #[test]
    fn decodes_escapes() {
        assert_eq!(decode("Prod%20web%2Fdb"), "Prod web/db");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("%C3%BC"), "ü");
        assert_eq!(decode("%EF%BB%BF%C3%BC"), "ü");
        assert_eq!(decode("caf%E9"), "café");
    }

    #[test]
    fn makes_aliases_distinct() {
        let mk = |n: &str| Session::new(n.into(), None, "h".into());
        let mut s = vec![mk("Work/web"), mk("Home/web"), mk("web"), mk("Work-web")];
        distinct(&mut s);
        let a: Vec<&str> = s.iter().map(|s| s.host.alias.as_str()).collect();
        assert_eq!(a, ["web", "Home-web", "web-2", "Work-web"]);
        assert_eq!(alias("  ünïcode box  "), "n-code-box");
        assert_eq!(alias("%%%"), "");
    }

    #[test]
    fn writes_folders_under_the_client() {
        let mut a = Session::new("Work/Prod/web".into(), None, "web.example.org".into());
        a.host.user = Some("admin".into());
        let b = Session::new("x\n# folder: y/Host evil".into(), None, "b".into());
        let text = to_config(Client::WinScp, &[&a, &b]);
        let hosts = ssh_config::hosts(&text, &ssh_config::NoIncludes);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].folder.as_deref(), Some("WinSCP/Work/Prod"));
        assert_eq!(hosts[1].folder.as_deref(), Some("WinSCP/x# folder: y"));
        let back = saved::parse(&text);
        assert_eq!(back[0].alias, "web");
        assert_eq!(back[0].user.as_deref(), Some("admin"));
        assert_eq!(back[1].alias, "Host-evil");
        assert_eq!(back.len(), 2);
    }
}
