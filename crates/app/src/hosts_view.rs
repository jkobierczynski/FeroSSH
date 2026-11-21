//! Tools > Hosts: one row per machine, the connections that reach the
//! same address taken together, with what the inventory knows of it
//! (the latest login, its host key, OS and kernel). Searched with the
//! connection manager's search words: a host is shown when one of its
//! connections matches. Platform-neutral, so it is tested on any host; the
//! native window is in `hosts_panel.rs`.

use ferossh_config::SessionKind;
use ferossh_config::host_query::Query;

use crate::inventory_view::Row as Connection;

/// One machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The address as connections resolve it (lower case), or a
    /// connection's name when it does not resolve.
    pub address: String,
    /// The connections that reach it, most recently used first.
    pub connections: Vec<Connection>,
    /// The ports and users they log in with, in order of first use.
    pub ports: Vec<u16>,
    pub users: Vec<String>,
    /// Every tag one of them has.
    pub tags: Vec<String>,
    /// The latest login through any of them.
    pub last_connected: Option<u64>,
    /// The host key seen at that login (or the latest one seen).
    pub key_type: String,
    pub fingerprint: String,
    /// More than one host key was seen: the connections disagree.
    pub keys_differ: bool,
    /// The OS and kernel last read.
    pub os: String,
    pub kernel: String,
}

impl Host {
    /// The connection Connect opens: the one used last, else the first.
    pub fn connect_to(&self) -> Option<&str> {
        self.connections.first().map(|c| c.host.name.as_str())
    }
}

/// What a column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Host,
    Connections,
    Users,
    Ports,
    Tags,
    LastConnected,
    KeyType,
    Fingerprint,
    Os,
}

impl Column {
    /// In the list's order.
    pub const ALL: [Self; 9] = [
        Self::Host,
        Self::Connections,
        Self::Users,
        Self::Ports,
        Self::Tags,
        Self::LastConnected,
        Self::KeyType,
        Self::Fingerprint,
        Self::Os,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Host => "Host",
            Self::Connections => "Connections",
            Self::Users => "Users",
            Self::Ports => "Ports",
            Self::Tags => "Tags",
            Self::LastConnected => "Last connected",
            Self::KeyType => "Host key",
            Self::Fingerprint => "Fingerprint",
            Self::Os => "OS",
        }
    }

    /// The column's width in the list, in DIPs.
    pub fn width(self) -> i32 {
        match self {
            Self::Host => 170,
            Self::Connections => 160,
            Self::Users => 90,
            Self::Ports => 60,
            Self::Tags => 110,
            Self::LastConnected => 120,
            Self::KeyType => 80,
            Self::Fingerprint => 200,
            Self::Os => 190,
        }
    }
}

fn push_new<T: PartialEq>(list: &mut Vec<T>, v: T) {
    if !list.contains(&v) {
        list.push(v);
    }
}

/// The inventory's connection rows taken together by the machine they
/// reach, sorted by address.
pub fn hosts(rows: &[Connection]) -> Vec<Host> {
    let mut out: Vec<Host> = Vec::new();
    for r in rows {
        let address = r
            .host
            .hostname
            .as_deref()
            .filter(|h| !h.is_empty())
            .unwrap_or(&r.host.name)
            .to_ascii_lowercase();
        let at = match out.iter().position(|h| h.address == address) {
            Some(i) => i,
            None => {
                out.push(Host {
                    address,
                    connections: Vec::new(),
                    ports: Vec::new(),
                    users: Vec::new(),
                    tags: Vec::new(),
                    last_connected: None,
                    key_type: String::new(),
                    fingerprint: String::new(),
                    keys_differ: false,
                    os: String::new(),
                    kernel: String::new(),
                });
                out.len() - 1
            }
        };
        let Some(h) = out.get_mut(at) else {
            continue;
        };
        h.connections.push(r.clone());
    }
    for h in &mut out {
        // The latest login first; never used ones after, by name.
        h.connections.sort_by(|a, b| {
            b.record
                .last_connected
                .cmp(&a.record.last_connected)
                .then_with(|| a.host.name.to_lowercase().cmp(&b.host.name.to_lowercase()))
        });
        let mut keys: Vec<&str> = Vec::new();
        for c in &h.connections {
            if let Some(p) = c.host.port {
                push_new(&mut h.ports, p);
            }
            if let Some(u) = c.host.user.as_deref().filter(|u| !u.is_empty()) {
                push_new(&mut h.users, u.to_owned());
            }
            for t in &c.host.tags {
                if !h.tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                    h.tags.push(t.clone());
                }
            }
            if !c.record.fingerprint.is_empty() {
                push_new(&mut keys, c.record.fingerprint.as_str());
                if h.fingerprint.is_empty() {
                    h.key_type.clone_from(&c.record.key_type);
                    h.fingerprint.clone_from(&c.record.fingerprint);
                }
            }
        }
        h.keys_differ = keys.len() > 1;
        h.last_connected = h
            .connections
            .iter()
            .filter_map(|c| c.record.last_connected)
            .max();
        // The OS and kernel read last, from whichever connection read it.
        if let Some(c) = h
            .connections
            .iter()
            .filter(|c| !c.record.os.is_empty() || !c.record.kernel.is_empty())
            .max_by_key(|c| c.record.system_read)
        {
            h.os.clone_from(&c.record.os);
            h.kernel.clone_from(&c.record.kernel);
        }
    }
    out.sort_by(|a, b| a.address.cmp(&b.address));
    out
}

/// The hosts a connection manager search keeps: one of its connections
/// matches, or a plain word is in its OS, kernel or a host key.
pub fn filter<'a>(hosts: &'a [Host], query: &str) -> Vec<&'a Host> {
    let q = Query::parse(query);
    hosts
        .iter()
        .filter(|h| {
            h.connections.iter().any(|c| {
                let more = [
                    h.address.as_str(),
                    h.os.as_str(),
                    h.kernel.as_str(),
                    c.record.fingerprint.as_str(),
                ];
                q.matches_with(&c.host.facts(), &more)
            })
        })
        .collect()
}

/// Sorts by `column`, newest first for the last login; ties by address.
pub fn sort(hosts: &mut [&Host], column: Column, descending: bool) {
    let key = |h: &Host| -> String { text(h, column, |_| String::new()) };
    hosts.sort_by(|a, b| {
        let o = match column {
            Column::LastConnected => a.last_connected.cmp(&b.last_connected),
            Column::Connections => a.connections.len().cmp(&b.connections.len()),
            _ => key(a).to_lowercase().cmp(&key(b).to_lowercase()),
        };
        let o = if descending { o.reverse() } else { o };
        o.then_with(|| a.address.cmp(&b.address))
    });
}

/// The column's text; `when` shows a time.
pub fn text(h: &Host, column: Column, when: impl Fn(u64) -> String) -> String {
    match column {
        Column::Host => h.address.clone(),
        Column::Connections => h
            .connections
            .iter()
            .map(|c| c.host.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        Column::Users => h.users.join(", "),
        Column::Ports => h
            .ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        Column::Tags => h.tags.join(" "),
        Column::LastConnected => h.last_connected.map(when).unwrap_or_default(),
        Column::KeyType => h.key_type.clone(),
        Column::Fingerprint if h.keys_differ => format!("{} (keys differ)", h.fingerprint),
        Column::Fingerprint => h.fingerprint.clone(),
        Column::Os => h.os.clone(),
    }
}

/// What the line above the buttons says: "8 of 12 hosts, 20 connections;
/// 1 with different host keys".
pub fn summary(shown: &[&Host], all: usize) -> String {
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut out = if shown.len() == all {
        plural(all, "host")
    } else {
        format!("{} of {}", shown.len(), plural(all, "host"))
    };
    let connections: usize = shown.iter().map(|h| h.connections.len()).sum();
    out.push_str(&format!(", {}", plural(connections, "connection")));
    let never = shown
        .iter()
        .filter(|h| {
            h.last_connected.is_none()
                && h.connections
                    .iter()
                    .all(|c| c.host.kind == SessionKind::Ssh)
        })
        .count();
    if never > 0 {
        out.push_str(&format!("; {never} not connected to yet"));
    }
    let differ = shown.iter().filter(|h| h.keys_differ).count();
    if differ > 0 {
        out.push_str(&format!("; {differ} with different host keys"));
    }
    out.push('.');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use ferossh_config::inventory::Record;

    use super::*;
    use crate::sessions::HostItem;

    fn conn(
        name: &str,
        user: &str,
        host: &str,
        port: u16,
        tags: &[&str],
        rec: Record,
    ) -> Connection {
        Connection {
            host: HostItem {
                name: name.into(),
                folder: None,
                detail: format!("{user}@{host}:{port}"),
                kind: SessionKind::Ssh,
                tags: tags.iter().map(|t| (*t).to_owned()).collect(),
                user: Some(user.into()),
                hostname: Some(host.into()),
                port: Some(port),
            },
            record: rec,
            system_info: false,
        }
    }

    fn rec(last: Option<u64>, fp: &str, os: &str, read: Option<u64>) -> Record {
        Record {
            last_connected: last,
            key_type: if fp.is_empty() {
                String::new()
            } else {
                "ED25519".into()
            },
            fingerprint: fp.into(),
            os: os.into(),
            kernel: String::new(),
            system_read: read,
        }
    }

    fn sample() -> Vec<Host> {
        hosts(&[
            conn(
                "web",
                "root",
                "Web.example",
                22,
                &["prod"],
                rec(Some(100), "SHA256:a", "Debian 12", Some(100)),
            ),
            conn(
                "web-admin",
                "admin",
                "web.example",
                2222,
                &["admin"],
                rec(Some(200), "SHA256:a", "", None),
            ),
            conn(
                "db",
                "root",
                "db.example",
                22,
                &["prod"],
                rec(None, "", "", None),
            ),
            conn(
                "old",
                "root",
                "db.example",
                22,
                &[],
                rec(Some(50), "SHA256:b", "Debian 11", Some(50)),
            ),
        ])
    }

    #[test]
    fn groups_connections_by_address() {
        let h = sample();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].address, "db.example");
        assert_eq!(h[1].address, "web.example");
        let web = &h[1];
        // The latest login first, and Connect opens that one.
        assert_eq!(
            text(web, Column::Connections, |_| String::new()),
            "web-admin, web"
        );
        assert_eq!(web.connect_to(), Some("web-admin"));
        assert_eq!(text(web, Column::Users, |_| String::new()), "admin, root");
        assert_eq!(text(web, Column::Ports, |_| String::new()), "2222, 22");
        assert_eq!(text(web, Column::Tags, |_| String::new()), "admin prod");
        assert_eq!(web.last_connected, Some(200));
        assert_eq!(web.os, "Debian 12");
        assert!(!web.keys_differ);
        let db = &h[0];
        assert_eq!(db.connect_to(), Some("old"));
        assert_eq!(db.fingerprint, "SHA256:b");
    }

    #[test]
    fn notes_disagreeing_host_keys() {
        let h = hosts(&[
            conn(
                "a",
                "root",
                "x",
                22,
                &[],
                rec(Some(2), "SHA256:new", "", None),
            ),
            conn(
                "b",
                "root",
                "x",
                22,
                &[],
                rec(Some(1), "SHA256:old", "", None),
            ),
        ]);
        assert!(h[0].keys_differ);
        assert_eq!(
            text(&h[0], Column::Fingerprint, |_| String::new()),
            "SHA256:new (keys differ)"
        );
        assert!(summary(&h.iter().collect::<Vec<_>>(), 1).contains("1 with different host keys"));
    }

    #[test]
    fn searches_through_the_connections() {
        let h = sample();
        let names =
            |q: &str| -> Vec<String> { filter(&h, q).iter().map(|h| h.address.clone()).collect() };
        assert_eq!(names("tag:admin"), ["web.example"]);
        assert_eq!(names("tag:prod"), ["db.example", "web.example"]);
        assert_eq!(names("debian 11"), ["db.example"]);
        assert_eq!(names("-tag:prod"), ["db.example", "web.example"]);
        assert!(names("nothing-like-this").is_empty());
    }

    #[test]
    fn sorts_and_sums_up() {
        let h = sample();
        let mut shown: Vec<&Host> = h.iter().collect();
        sort(&mut shown, Column::LastConnected, true);
        assert_eq!(shown[0].address, "web.example");
        let s = summary(&shown, 2);
        assert_eq!(s, "2 hosts, 4 connections.");
        let one = summary(&shown[..1], 2);
        assert_eq!(one, "1 of 2 hosts, 2 connections.");
    }
}
