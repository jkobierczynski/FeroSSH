//! Colour themes per host or host group, so a pane shows where it types:
//! production red, staging amber, development Nord.
//!
//! A rule matches a host by `Host`-style patterns or by a group tag. Tags
//! come from ssh_config's `Tag` keyword (OpenSSH 9.4+) once session
//! profiles arrive in milestone 4, so one ssh_config line such as
//! `Tag production` both groups the hosts for OpenSSH's `Match tagged` and
//! colours FeroSSH's panes. The first matching rule wins, like ssh_config.

use crate::pattern::{match_host, match_pattern};

/// One rule: which hosts, and how their panes look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeRule {
    /// `Host`-style patterns; empty matches no host by name.
    pub hosts: Vec<String>,
    /// Group tags (patterns too, so `prod*` works); empty matches no tag.
    pub tags: Vec<String>,
    /// A built-in or user theme name.
    pub theme: String,
    /// Optional background tint over the theme: colour and strength 0-255.
    pub tint: Option<([u8; 3], u8)>,
}

impl ThemeRule {
    pub fn new(theme: &str) -> Self {
        Self {
            hosts: Vec::new(),
            tags: Vec::new(),
            theme: theme.to_owned(),
            tint: None,
        }
    }

    pub fn hosts(mut self, patterns: &[&str]) -> Self {
        self.hosts.extend(patterns.iter().map(|p| (*p).to_owned()));
        self
    }

    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags.extend(tags.iter().map(|t| (*t).to_owned()));
        self
    }

    pub fn tint(mut self, rgb: [u8; 3], amount: u8) -> Self {
        self.tint = Some((rgb, amount));
        self
    }

    fn matches(&self, host: &str, tags: &[&str]) -> bool {
        let by_tag = self
            .tags
            .iter()
            .any(|p| tags.iter().any(|t| match_pattern(t, p)));
        by_tag || match_host(host, self.hosts.iter().map(String::as_str))
    }
}

/// The user's rules, then the built-in tag rules.
#[derive(Clone, Debug, Default)]
pub struct HostThemes {
    rules: Vec<ThemeRule>,
}

impl HostThemes {
    /// The built-in rules: tags `production`/`prod`, `staging`/`stage`,
    /// `development`/`dev` pick the Production, Staging and Nord themes.
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Adds a user rule. User rules are tried in the order added, before
    /// the built-in ones.
    pub fn push(&mut self, rule: ThemeRule) {
        self.rules.push(rule);
    }

    /// The rule for a host and its tags, if any.
    pub fn rule_for(&self, host: &str, tags: &[&str]) -> Option<&ThemeRule> {
        self.rules
            .iter()
            .chain(builtin_rules())
            .find(|r| r.matches(host, tags))
    }
}

fn builtin_rules() -> &'static [ThemeRule] {
    use std::sync::OnceLock;
    static RULES: OnceLock<[ThemeRule; 3]> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            ThemeRule::new("Production").tags(&["production", "prod"]),
            ThemeRule::new("Staging").tags(&["staging", "stage"]),
            ThemeRule::new("Nord").tags(&["development", "dev"]),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme<'a>(t: &'a HostThemes, host: &str, tags: &[&str]) -> Option<&'a str> {
        t.rule_for(host, tags).map(|r| r.theme.as_str())
    }

    #[test]
    fn builtin_tags() {
        let t = HostThemes::new();
        assert_eq!(theme(&t, "db1", &["production"]), Some("Production"));
        assert_eq!(theme(&t, "db1", &["PROD"]), Some("Production"));
        assert_eq!(theme(&t, "web", &["staging"]), Some("Staging"));
        assert_eq!(theme(&t, "box", &["dev"]), Some("Nord"));
        assert_eq!(theme(&t, "box", &[]), None);
        assert_eq!(theme(&t, "box", &["other"]), None);
    }

    #[test]
    fn user_rules_first_match_wins() {
        let mut t = HostThemes::new();
        t.push(ThemeRule::new("Gruvbox Dark").hosts(&["prod-sandbox"]));
        t.push(
            ThemeRule::new("Dracula")
                .hosts(&["prod-*", "*.prod.example.com"])
                .tint([0x80, 0, 0], 60),
        );
        t.push(ThemeRule::new("Staging").tags(&["qa*"]));
        assert_eq!(theme(&t, "prod-sandbox", &[]), Some("Gruvbox Dark"));
        let r = t.rule_for("db.prod.example.com", &[]);
        assert_eq!(r.map(|r| r.tint), Some(Some(([0x80, 0, 0], 60))));
        assert_eq!(theme(&t, "x", &["qa-eu"]), Some("Staging"));
        // A user host rule beats a built-in tag.
        assert_eq!(theme(&t, "prod-web", &["dev"]), Some("Dracula"));
        assert_eq!(theme(&t, "dev-web", &["dev"]), Some("Nord"));
    }
}
