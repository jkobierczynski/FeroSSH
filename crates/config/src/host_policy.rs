//! Security rules by tag, folder or host pattern: what a connection may
//! not do, whatever its own settings say. A rule can only tighten: every
//! rule that matches adds its limits, and none takes one away.
//!
//! ```text
//! SecurityRule
//!     Tags production
//!     Folders Customers/*
//!     Hosts *.prod.example.com
//!     KeysOnly yes
//!     SavePassword no
//!     ForwardAgent no
//!     Clipboard no
//!     ConfirmConnect yes
//!     LogSession yes
//! ```

use crate::password::PasswordSource;
use crate::pattern::{match_host, match_pattern};

/// One rule: which connections, and the limits they get.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityRule {
    /// `Host`-style patterns, matched against the connection's name and
    /// the machine it reaches.
    pub hosts: Vec<String>,
    /// Tag patterns (`prod*`), any case.
    pub tags: Vec<String>,
    /// Folder patterns from the connection manager: a folder matches, and
    /// so does every folder inside it.
    pub folders: Vec<String>,
    pub limits: Policy,
    /// Lines of the rule this FeroSSH does not understand (a keyword
    /// from a newer FeroSSH, a misspelling, a value like `KeysOnly 1`),
    /// as written: kept and written back, never applied, and shown as a
    /// warning so a limit is not lost without a word.
    pub unknown: Vec<String>,
}

/// What a connection may not do. `Default` is no limit at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// Log in with a key, the agent or a security key: no password.
    pub keys_only: bool,
    /// No password kept in, or read from, the Windows Credential Manager.
    pub no_saved_password: bool,
    /// No agent forwarding.
    pub no_agent_forwarding: bool,
    /// The server may not set this PC's clipboard (OSC 52).
    pub no_clipboard: bool,
    /// Ask before connecting.
    pub confirm_connect: bool,
    /// Keep a session log, which cannot be turned off.
    pub log_session: bool,
}

impl Policy {
    /// Whether no limit is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Adds `other`'s limits to these.
    pub fn tighten(&mut self, other: &Policy) {
        self.keys_only |= other.keys_only;
        self.no_saved_password |= other.no_saved_password;
        self.no_agent_forwarding |= other.no_agent_forwarding;
        self.no_clipboard |= other.no_clipboard;
        self.confirm_connect |= other.confirm_connect;
        self.log_session |= other.log_session;
    }

    /// These limits with `other`'s added.
    pub fn tightened(mut self, other: &Policy) -> Self {
        self.tighten(other);
        self
    }

    /// Password source `source`, as these limits allow it: none at all
    /// under `KeysOnly`, and no Windows Credential Manager under
    /// `SavePassword no`.
    pub fn allow_source(&self, source: PasswordSource) -> PasswordSource {
        match source {
            _ if self.keys_only => PasswordSource::Ask,
            PasswordSource::Windows if self.no_saved_password => PasswordSource::Ask,
            other => other,
        }
    }

    /// These limits for a login that has only passwords (WinRM):
    /// `KeysOnly` speaks of SSH logins and is left out.
    pub fn without_keys_only(mut self) -> Self {
        self.keys_only = false;
        self
    }

    /// A connection's clipboard limit (`both`, `to-host`, `from-host`,
    /// `off`; unset is both) as these limits leave it: under `Clipboard
    /// no` nothing comes from the host, so a desktop or remote app may at
    /// most send text to it.
    pub fn clipboard_limit(&self, way: Option<&str>) -> Option<String> {
        if !self.no_clipboard {
            return way.map(str::to_owned);
        }
        let (to_host, _) = crate::settings::clipboard_ways(way);
        Some(if to_host { "to-host" } else { "off" }.to_owned())
    }

    /// The limits in words, for the line a connection shows.
    pub fn describe(&self) -> Vec<&'static str> {
        [
            (self.keys_only, "keys only"),
            (self.no_saved_password, "no saved password"),
            (self.no_agent_forwarding, "no agent forwarding"),
            (self.no_clipboard, "no clipboard"),
            (self.confirm_connect, "confirm before connecting"),
            (self.log_session, "session logged"),
        ]
        .into_iter()
        .filter_map(|(on, what)| on.then_some(what))
        .collect()
    }
}

/// What the rules are matched against.
#[derive(Clone, Copy, Debug, Default)]
pub struct Subject<'a> {
    /// The connection's name.
    pub name: &'a str,
    /// The machine it reaches, when known.
    pub hostname: Option<&'a str>,
    pub tags: &'a [String],
    /// Its folder in the connection manager, `/` between levels.
    pub folder: Option<&'a str>,
}

impl SecurityRule {
    /// Whether the rule names nothing to match, or limits nothing.
    /// A rule with a line FeroSSH does not understand is never inert:
    /// that line may be the one that names its connections or sets its
    /// limit, and dropping the rule would lose it.
    pub fn is_inert(&self) -> bool {
        ((self.hosts.is_empty() && self.tags.is_empty() && self.folders.is_empty())
            || self.limits.is_empty())
            && self.unknown.is_empty()
    }

    /// The rule's lines FeroSSH does not understand, in words for a
    /// warning: `not understood: KeysOnly 1; X11 no`. Empty when none.
    pub fn describe_unknown(&self) -> String {
        if self.unknown.is_empty() {
            return String::new();
        }
        format!("not understood: {}", self.unknown.join("; "))
    }

    pub fn matches(&self, s: &Subject<'_>) -> bool {
        let fold = crate::settings::fold_tag;
        let by_tag = self.tags.iter().any(|p| {
            let p = fold(p);
            s.tags.iter().any(|t| match_pattern(&fold(t), &p))
        });
        let by_folder = s.folder.is_some_and(|f| {
            let f = f.trim_matches('/');
            // The folder itself and each one it sits in.
            let mut paths = vec![f];
            let mut at = f;
            while let Some((up, _)) = at.rsplit_once('/') {
                paths.push(up);
                at = up;
            }
            self.folders.iter().any(|p| {
                let p = p.trim_matches('/').to_lowercase();
                paths
                    .iter()
                    .any(|path| !path.is_empty() && match_pattern(&path.to_lowercase(), &p))
            })
        });
        let by_host = match_host(s.name, self.hosts.iter().map(String::as_str))
            || s.hostname
                .is_some_and(|h| match_host(h, self.hosts.iter().map(String::as_str)));
        by_tag || by_folder || by_host
    }
}

/// Each of the limits by its name in the Security rules manager, with
/// its keyword line in the file: in the order the window shows them.
pub const LIMITS: [(&str, &str); 6] = [
    ("Keys only: no password logins", "KeysOnly yes"),
    ("No saved passwords", "SavePassword no"),
    ("No agent forwarding", "ForwardAgent no"),
    ("No clipboard writes from the server", "Clipboard no"),
    ("Ask before connecting", "ConfirmConnect yes"),
    ("Always log the session", "LogSession yes"),
];

impl Policy {
    /// The limits as on/off, in the order of [`LIMITS`].
    pub fn flags(&self) -> [bool; 6] {
        [
            self.keys_only,
            self.no_saved_password,
            self.no_agent_forwarding,
            self.no_clipboard,
            self.confirm_connect,
            self.log_session,
        ]
    }

    /// Limits from on/off in the order of [`LIMITS`]; missing ones are off.
    pub fn from_flags(flags: &[bool]) -> Self {
        let on = |i: usize| flags.get(i).copied().unwrap_or(false);
        Policy {
            keys_only: on(0),
            no_saved_password: on(1),
            no_agent_forwarding: on(2),
            no_clipboard: on(3),
            confirm_connect: on(4),
            log_session: on(5),
        }
    }
}

impl SecurityRule {
    /// What the rule matches, in words: `tags prod*; folders Customers`.
    pub fn describe_match(&self) -> String {
        [
            ("hosts", &self.hosts),
            ("tags", &self.tags),
            ("folders", &self.folders),
        ]
        .into_iter()
        .filter(|(_, list)| !list.is_empty())
        .map(|(what, list)| format!("{what} {}", words_text(list)))
        .collect::<Vec<_>>()
        .join("; ")
    }
}

/// Patterns as typed in one field: split at spaces, with double quotes
/// around one that has spaces (`"Customers/Big one" Lab`). Control
/// characters are left out.
pub fn words(text: &str) -> Vec<String> {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    crate::ssh_config::split_line(&format!("x = {clean}"))
        .map(|(_, args)| args)
        .unwrap_or_default()
        .into_iter()
        .map(|w| w.trim().to_owned())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Patterns as one field shows them: the other way from [`words`].
pub fn words_text(list: &[String]) -> String {
    list.iter()
        .map(|w| {
            let w: String = w.chars().filter(|c| !c.is_control() && *c != '"').collect();
            if w.contains(char::is_whitespace) {
                format!("\"{w}\"")
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether host names `a` and `b` name the same machine, as written:
/// any case, without a trailing dot or the brackets around an IPv6
/// address. No DNS: an address and a name for it are not matched. This
/// PC's own names (`localhost`, `127.x`, `::1`) are never one machine:
/// tunnels and port forwards reach many through them.
pub fn same_machine(a: &str, b: &str) -> bool {
    let norm = |h: &str| {
        let h = h.trim();
        let h = h
            .strip_prefix('[')
            .and_then(|x| x.strip_suffix(']'))
            .unwrap_or(h);
        h.trim_end_matches('.').to_ascii_lowercase()
    };
    let (a, b) = (norm(a), norm(b));
    let loopback = a == "localhost"
        || a.ends_with(".localhost")
        || a.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    !a.is_empty() && !loopback && a == b
}

/// The machine connection `subject` reaches: its host name, else its
/// name.
pub fn machine<'a>(subject: &Subject<'a>) -> &'a str {
    subject.hostname.unwrap_or(subject.name)
}

/// The limits for `subject`. One that is not itself in `others` (the
/// saved connections: an address typed by hand, an `ssh://` link, an
/// alias the Connection manager does not list) also gets those of every
/// connection in `others` that reaches the same machine
/// ([`same_machine`]), so typing a production host's address does not
/// leave its tag or folder rules behind. A saved connection keeps to its
/// own rules: tagging one of them leaves the others to that host as they
/// are. Also the names of the connections whose rules added something.
pub fn policy_with_peers<'a>(
    rules: &[SecurityRule],
    subject: &Subject<'_>,
    others: &[Subject<'a>],
) -> (Policy, Vec<&'a str>) {
    let mut policy = policy_for(rules, subject);
    let mut from = Vec::new();
    if others
        .iter()
        .any(|o| o.name.eq_ignore_ascii_case(subject.name))
    {
        return (policy, from);
    }
    let here = machine(subject);
    for o in others {
        if o.name.eq_ignore_ascii_case(subject.name) || !same_machine(machine(o), here) {
            continue;
        }
        let theirs = policy_for(rules, o);
        if policy.tightened(&theirs) != policy {
            from.push(o.name);
        }
        policy.tighten(&theirs);
    }
    (policy, from)
}

/// The lines FeroSSH does not understand in the rules that match
/// `subject` (each once): limits it may be missing.
pub fn not_understood<'a>(rules: &'a [SecurityRule], subject: &Subject<'_>) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for line in rules
        .iter()
        .filter(|r| r.matches(subject))
        .flat_map(|r| r.unknown.iter())
    {
        if !out.contains(&line.as_str()) {
            out.push(line);
        }
    }
    out
}

/// The limits every matching rule adds up to.
pub fn policy_for(rules: &[SecurityRule], subject: &Subject<'_>) -> Policy {
    let mut policy = Policy::default();
    for r in rules.iter().filter(|r| r.matches(subject)) {
        policy.tighten(&r.limits);
    }
    policy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_keep_a_leading_equals_sign() {
        assert_eq!(words(&words_text(&["=Prod".to_owned()])), ["=Prod"]);
        assert_eq!(words("=a b"), ["=a", "b"]);
    }

    fn rule() -> SecurityRule {
        SecurityRule::default()
    }

    #[test]
    fn rules_match_by_tag_folder_or_host_and_only_add_limits() {
        let mut keys = rule();
        keys.tags.push("prod*".into());
        keys.limits.keys_only = true;
        let mut folder = rule();
        folder.folders.push("Customers".into());
        folder.limits.no_clipboard = true;
        let mut host = rule();
        host.hosts.push("*.bank.example".into());
        host.limits.confirm_connect = true;
        let rules = [keys, folder, host];
        let tags = vec!["PRODUCTION".to_owned()];
        let p = policy_for(
            &rules,
            &Subject {
                name: "db1",
                hostname: Some("db1.bank.example"),
                tags: &tags,
                folder: Some("Customers/Acme"),
            },
        );
        assert!(p.keys_only && p.no_clipboard && p.confirm_connect);
        assert!(!p.log_session && !p.no_agent_forwarding);
        // Nothing matches: no limits.
        let none = policy_for(
            &rules,
            &Subject {
                name: "lab",
                folder: Some("Lab/Customers"),
                ..Subject::default()
            },
        );
        assert!(none.is_empty());
    }

    #[test]
    fn a_rule_without_hosts_or_limits_is_inert() {
        let mut r = rule();
        assert!(r.is_inert());
        r.tags.push("x".into());
        assert!(r.is_inert());
        r.limits.log_session = true;
        assert!(!r.is_inert());
        assert_eq!(r.limits.describe(), ["session logged"]);
    }

    #[test]
    fn fields_round_trip_patterns_with_spaces() {
        let list = words(r#"prod*  "Customers/Big one" Lab"#);
        assert_eq!(list, ["prod*", "Customers/Big one", "Lab"]);
        assert_eq!(words_text(&list), r#"prod* "Customers/Big one" Lab"#);
        assert!(words("   ").is_empty());
        let mut r = rule();
        r.tags = vec!["prod*".into()];
        r.folders = vec!["Big one".into()];
        assert_eq!(r.describe_match(), r#"tags prod*; folders "Big one""#);
        let p = Policy {
            no_clipboard: true,
            log_session: true,
            ..Policy::default()
        };
        assert_eq!(Policy::from_flags(&p.flags()), p);
    }

    #[test]
    fn limits_take_away_saved_passwords() {
        let cmd = PasswordSource::Command("op read x".into());
        let none = Policy::default();
        assert_eq!(
            none.allow_source(PasswordSource::Windows),
            PasswordSource::Windows
        );
        let unsaved = Policy {
            no_saved_password: true,
            ..Policy::default()
        };
        assert_eq!(
            unsaved.allow_source(PasswordSource::Windows),
            PasswordSource::Ask
        );
        assert_eq!(unsaved.allow_source(cmd.clone()), cmd);
        let keys = Policy {
            keys_only: true,
            ..Policy::default()
        };
        assert_eq!(keys.allow_source(cmd), PasswordSource::Ask);
    }

    #[test]
    fn no_clipboard_leaves_only_text_to_the_host() {
        let free = Policy::default();
        assert_eq!(free.clipboard_limit(None), None);
        assert_eq!(
            free.clipboard_limit(Some("from-host")).as_deref(),
            Some("from-host")
        );
        let rule = Policy {
            no_clipboard: true,
            ..Policy::default()
        };
        assert_eq!(rule.clipboard_limit(None).as_deref(), Some("to-host"));
        assert_eq!(
            rule.clipboard_limit(Some("Both")).as_deref(),
            Some("to-host")
        );
        assert_eq!(
            rule.clipboard_limit(Some("from-host")).as_deref(),
            Some("off")
        );
        assert_eq!(rule.clipboard_limit(Some("off")).as_deref(), Some("off"));
        assert_eq!(
            crate::settings::clipboard_ways(rule.clipboard_limit(Some("to-host")).as_deref()),
            (true, false)
        );
    }

    #[test]
    fn rules_follow_the_machine_not_the_name() {
        let mut keys = rule();
        keys.tags.push("prod".into());
        keys.limits.keys_only = true;
        let mut folder = rule();
        folder.folders.push("Bank".into());
        folder.limits.log_session = true;
        let rules = [keys, folder];
        let prod = vec!["prod".to_owned()];
        let saved = [
            Subject {
                name: "db1",
                hostname: Some("DB1.example.com."),
                tags: &prod,
                folder: None,
            },
            Subject {
                name: "vault",
                hostname: Some("[2001:db8::5]"),
                folder: Some("Bank/eu"),
                ..Subject::default()
            },
            Subject {
                name: "lab",
                hostname: Some("lab.example.com"),
                ..Subject::default()
            },
        ];
        // The address typed: db1's rules come along.
        let typed = Subject {
            name: "db1.example.com",
            hostname: Some("db1.example.com"),
            ..Subject::default()
        };
        let (p, from) = policy_with_peers(&rules, &typed, &saved);
        assert!(p.keys_only && !p.log_session);
        assert_eq!(from, ["db1"]);
        // An IPv6 address, bracketed or not.
        let v6 = Subject {
            name: "2001:db8::5",
            ..Subject::default()
        };
        let (p, from) = policy_with_peers(&rules, &v6, &saved);
        assert!(p.log_session);
        assert_eq!(from, ["vault"]);
        // db1 itself: its own rules, nobody else named.
        let (p, from) = policy_with_peers(&rules, &saved[0], &saved);
        assert!(p.keys_only);
        assert!(from.is_empty());
        // A second saved connection to db1's machine keeps to its own
        // rules: db1's tag does not spread to it.
        let twin = Subject {
            name: "db1-admin",
            hostname: Some("db1.example.com"),
            ..Subject::default()
        };
        let mut with_twin = saved.to_vec();
        with_twin.push(twin);
        let (p, from) = policy_with_peers(&rules, &twin, &with_twin);
        assert!(p.is_empty() && from.is_empty());
        // Another machine: nothing.
        let (p, _) = policy_with_peers(&rules, &saved[2], &saved);
        assert!(p.is_empty());
        assert!(!same_machine("", ""));
        assert!(!same_machine("localhost", "LOCALHOST"));
        assert!(!same_machine("127.0.0.1", "127.0.0.1"));
        assert!(!same_machine("[::1]", "::1"));
    }

    #[test]
    fn a_rule_with_lines_not_understood_is_kept() {
        let mut r = rule();
        r.unknown.push("KeysOnly 1".into());
        assert!(!r.is_inert());
        assert_eq!(r.describe_unknown(), "not understood: KeysOnly 1");
        r.tags.push("prod".into());
        let tags = vec!["prod".to_owned()];
        let subject = Subject {
            name: "db",
            tags: &tags,
            ..Subject::default()
        };
        let rules = [r.clone(), r];
        assert_eq!(not_understood(&rules, &subject), ["KeysOnly 1"]);
        assert!(not_understood(&rules, &Subject::default()).is_empty());
    }
}
