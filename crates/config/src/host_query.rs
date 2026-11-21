//! The connection manager's search language. Plain words match a host's
//! name, its `user@host:port` detail, its folder or a tag, as they always
//! did; a word with a field name in front matches that field only:
//!
//! ```text
//! tag:prod folder:db -tag:legacy user:root type:ssh port:2222 host:10.0.
//! ```
//!
//! - `name:`, `host:`, `user:` and `folder:` match part of the field.
//! - `addr:` matches the whole host name (an IP address however it is
//!   written: `2001:db8::1` is `2001:0db8:0:0:0:0:0:1`); `*` and `?` work.
//! - `tag:` and `type:` match a whole tag or connection type; `*` and `?`
//!   work as in `Host` patterns (`tag:prod*`).
//! - `port:` matches the port number.
//! - `-` in front of a word leaves out what it matches.
//! - `"double quotes"` keep spaces in a word: `folder:"Home lab"`.
//!
//! Every word must hold. Case never matters. A field name FeroSSH does
//! not know (`foo:bar`) is searched for as plain text, so a host name
//! with a colon still matches.

use crate::pattern::match_pattern;

/// The fields a word can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Host,
    /// The whole host name, as [`canonical_address`] writes it.
    Addr,
    User,
    Port,
    Folder,
    Tag,
    Type,
}

impl Field {
    fn parse(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "name" => Self::Name,
            "host" | "hostname" => Self::Host,
            "addr" | "address" => Self::Addr,
            "user" => Self::User,
            "port" => Self::Port,
            "folder" => Self::Folder,
            "tag" | "tags" => Self::Tag,
            "type" | "kind" => Self::Type,
            _ => return None,
        })
    }
}

/// One word of a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    /// `-` in front: the host must not match.
    pub negate: bool,
    /// The field it names; `None` for a plain word.
    pub field: Option<Field>,
    /// What to look for, lower-cased (tags and types folded as
    /// [`crate::settings::fold_tag`] does).
    pub value: String,
}

/// A parsed search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<Term>,
}

/// What a search looks at in one host.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostFacts<'a> {
    pub name: &'a str,
    pub folder: Option<&'a str>,
    pub user: Option<&'a str>,
    pub hostname: Option<&'a str>,
    pub port: Option<u16>,
    /// The connection type's keyword (`ssh`, `rdp`, ...).
    pub kind: &'a str,
    pub tags: &'a [String],
    /// What the list shows beside the name, searched by plain words.
    pub detail: &'a str,
}

/// Splits a search into words: spaces separate them, double quotes keep
/// spaces inside one (and are dropped).
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c if c.is_control() => {}
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out
}

impl Query {
    pub fn parse(text: &str) -> Self {
        let mut terms = Vec::new();
        for w in words(text) {
            let (negate, rest) = match w.strip_prefix('-') {
                Some(r) if !r.is_empty() => (true, r),
                _ => (false, w.as_str()),
            };
            let (field, value) = match rest.split_once(':') {
                Some((name, value)) => match Field::parse(name) {
                    Some(f) => (Some(f), value),
                    None => (None, rest),
                },
                None => (None, rest),
            };
            // `tag:` with nothing after it matches nothing useful: skip it
            // while it is being typed.
            if value.is_empty() {
                continue;
            }
            // Tags and types compare as tags do, a character for a
            // character (`İ` stays one); the rest as lower case.
            let value = match field {
                Some(Field::Tag | Field::Type) => crate::settings::fold_tag(value),
                _ => value.to_lowercase(),
            };
            terms.push(Term {
                negate,
                field,
                value,
            });
        }
        Self { terms }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether the host matches every word.
    pub fn matches(&self, h: &HostFacts<'_>) -> bool {
        self.matches_with(h, &[])
    }

    /// [`Self::matches`], with `more` text a plain word may also be found
    /// in (a `-` word then must be in none of it); a field word looks only
    /// at its field.
    pub fn matches_with(&self, h: &HostFacts<'_>, more: &[&str]) -> bool {
        self.terms.iter().all(|t| {
            let hit = t.hit(h) || (t.field.is_none() && more.iter().any(|m| has(m, &t.value)));
            hit != t.negate
        })
    }
}

fn has(text: &str, value: &str) -> bool {
    text.to_lowercase().contains(value)
}

/// A whole-value match, with `*` and `?` as in `Host` patterns.
fn whole(text: &str, value: &str) -> bool {
    if value.contains(['*', '?']) {
        match_pattern(
            &crate::settings::fold_tag(text),
            &crate::settings::fold_tag(value),
        )
    } else {
        crate::settings::same_tag(text, value)
    }
}

impl Term {
    fn hit(&self, h: &HostFacts<'_>) -> bool {
        let v = self.value.as_str();
        match self.field {
            None => {
                let tag_word = v.strip_prefix('#').unwrap_or(v);
                has(h.name, v)
                    || has(h.detail, v)
                    || h.folder.is_some_and(|f| has(f, v))
                    || (!tag_word.is_empty() && h.tags.iter().any(|t| has(t, tag_word)))
            }
            Some(Field::Name) => has(h.name, v),
            Some(Field::Host) => h.hostname.is_some_and(|x| has(x, v)),
            Some(Field::Addr) => {
                let at = h.hostname.filter(|x| !x.is_empty()).unwrap_or(h.name);
                let at = canonical_address(at);
                if v.contains(['*', '?']) {
                    match_pattern(&at, v)
                } else {
                    at == canonical_address(v)
                }
            }
            Some(Field::User) => h.user.is_some_and(|x| has(x, v)),
            Some(Field::Port) => h.port.is_some_and(|p| p.to_string() == v),
            Some(Field::Folder) => h.folder.is_some_and(|x| has(x, v)),
            Some(Field::Tag) => {
                let v = v.strip_prefix('#').unwrap_or(v);
                h.tags.iter().any(|t| whole(t, v))
            }
            Some(Field::Type) => whole(h.kind, v),
        }
    }
}

/// A host name as one machine's address: an IP address in its usual
/// form (no brackets, IPv6 shortened), anything else in lower case.
pub fn canonical_address(host: &str) -> String {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => host.to_lowercase(),
    }
}

/// The longest saved search, in characters (as long as the search box
/// takes).
pub const MAX_SAVED_LEN: usize = 64;

/// A search as kept in a `SavedSearch` line: one line, trimmed, at most
/// [`MAX_SAVED_LEN`] characters; `None` when nothing is left.
pub fn saved_text(text: &str) -> Option<String> {
    let t: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_SAVED_LEN)
        .collect();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| (*t).to_owned()).collect()
    }

    fn found(query: &str, hosts: &[HostFacts<'_>]) -> Vec<String> {
        let q = Query::parse(query);
        hosts
            .iter()
            .filter(|h| q.matches(h))
            .map(|h| h.name.to_owned())
            .collect()
    }

    #[test]
    fn addr_matches_the_whole_address() {
        let none = Vec::new();
        let host = |name, hostname| HostFacts {
            name,
            hostname: Some(hostname),
            kind: "ssh",
            tags: &none,
            ..HostFacts::default()
        };
        let hosts = [
            host("db", "db.example"),
            host("backup", "db.example.backup"),
            host("v6", "[2001:0db8:0:0:0:0:0:1]"),
        ];
        assert_eq!(found("addr:db.example", &hosts), ["db"]);
        assert_eq!(found("addr:DB.example*", &hosts), ["db", "backup"]);
        assert_eq!(found("addr:2001:db8::1", &hosts), ["v6"]);
        assert_eq!(found("host:db.example", &hosts), ["db", "backup"]);
        assert_eq!(canonical_address("[2001:DB8:0::1]"), "2001:db8::1");
        assert_eq!(canonical_address("Web.Example"), "web.example");
    }

    #[test]
    fn fields_negation_patterns_and_quotes() {
        let prod = tags(&["prod", "web"]);
        let legacy = tags(&["prod", "legacy"]);
        let none = Vec::new();
        let hosts = [
            HostFacts {
                name: "web1",
                folder: Some("Home lab"),
                user: Some("admin"),
                hostname: Some("10.0.0.5"),
                port: Some(22),
                kind: "ssh",
                tags: &prod,
                detail: "admin@10.0.0.5",
            },
            HostFacts {
                name: "db-old",
                folder: Some("Databases"),
                user: Some("root"),
                hostname: Some("db.example"),
                port: Some(2222),
                kind: "ssh",
                tags: &legacy,
                detail: "root@db.example:2222",
            },
            HostFacts {
                name: "desk",
                kind: "rdp",
                tags: &none,
                ..HostFacts::default()
            },
        ];
        assert_eq!(found("tag:prod", &hosts), ["web1", "db-old"]);
        assert_eq!(found("tag:prod -tag:legacy", &hosts), ["web1"]);
        assert_eq!(found("TAG:#Leg*", &hosts), ["db-old"]);
        assert_eq!(found("tag:pro", &hosts), Vec::<String>::new(), "whole tags");
        assert_eq!(found("user:ro port:2222", &hosts), ["db-old"]);
        assert_eq!(found("host:10.0.", &hosts), ["web1"]);
        assert_eq!(found("type:rdp", &hosts), ["desk"]);
        assert_eq!(found("-type:ssh", &hosts), ["desk"]);
        assert_eq!(found("folder:\"home lab\"", &hosts), ["web1"]);
        assert_eq!(found("folder:\"home", &hosts), ["web1"], "an open quote");
        // Plain words as before, tags included; unknown fields are text.
        assert_eq!(found("#web", &hosts), ["web1"]);
        assert_eq!(found("legacy", &hosts), ["db-old"]);
        assert_eq!(found("foo:bar", &hosts), Vec::<String>::new());
        assert_eq!(found("tag:", &hosts), ["web1", "db-old", "desk"]);
        let dotted = tags(&["İzmir"]);
        let tr = [HostFacts {
            name: "tr",
            tags: &dotted,
            ..HostFacts::default()
        }];
        assert_eq!(found("tag:İzmir", &tr), ["tr"]);
        assert_eq!(found("tag:izmir", &tr), ["tr"]);
        assert_eq!(found("tag:?zmir", &tr), ["tr"], "one character");
        assert_eq!(found("tag:İz*", &tr), ["tr"]);
        assert!(Query::parse("  ").is_empty());
    }

    #[test]
    fn saved_text_is_one_trimmed_line() {
        assert_eq!(saved_text("  tag:prod\n "), Some("tag:prod".into()));
        assert_eq!(saved_text(" \t"), None);
        assert_eq!(saved_text(&"x".repeat(80)).unwrap().len(), MAX_SAVED_LEN);
    }
}
