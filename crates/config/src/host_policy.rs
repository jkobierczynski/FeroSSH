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
    pub fn is_inert(&self) -> bool {
        (self.hosts.is_empty() && self.tags.is_empty() && self.folders.is_empty())
            || self.limits.is_empty()
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
}
