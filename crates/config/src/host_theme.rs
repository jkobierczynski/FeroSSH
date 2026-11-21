//! Colour themes per host or host group, so a pane shows where it types:
//! production red, staging amber, development Nord.
//!
//! A rule matches a host by `Host`-style patterns or by a group tag. Tags
//! come from ssh_config's `Tag` keyword (OpenSSH 9.4+), so one ssh_config
//! line such as
//! `Tag production` both groups the hosts for OpenSSH's `Match tagged` and
//! colours FeroSSH's panes. The first matching rule wins, like ssh_config.

use crate::pattern::{match_host, match_pattern};

/// One rule: which hosts, and how their panes look.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeRule {
    /// `Host`-style patterns; empty matches no host by name.
    pub hosts: Vec<String>,
    /// Group tags (patterns too, so `prod*` works); empty matches no tag.
    pub tags: Vec<String>,
    /// A built-in or user theme name; empty when the rule only sets a
    /// font.
    pub theme: String,
    /// Optional background tint over the theme: colour and strength 0-255.
    pub tint: Option<([u8; 3], u8)>,
    /// The font family its panes use, unless the connection sets one.
    pub font: Option<String>,
    /// The font size in points, unless the connection sets one.
    pub font_size: Option<f32>,
}

impl ThemeRule {
    pub fn new(theme: &str) -> Self {
        Self {
            hosts: Vec::new(),
            tags: Vec::new(),
            theme: theme.to_owned(),
            tint: None,
            font: None,
            font_size: None,
        }
    }

    pub fn font(mut self, family: &str, size: Option<f32>) -> Self {
        self.font = Some(family.to_owned()).filter(|f| !f.is_empty());
        self.font_size = size;
        self
    }

    /// Whether it sets anything at all: a theme, a font or a size.
    pub fn sets_something(&self) -> bool {
        !self.theme.is_empty() || self.font.is_some() || self.font_size.is_some()
    }

    /// What it matches, for a list: `tags prod*; hosts *.corp`.
    pub fn describe_match(&self) -> String {
        let mut parts = Vec::new();
        if !self.tags.is_empty() {
            parts.push(format!("tags {}", self.tags.join(" ")));
        }
        if !self.hosts.is_empty() {
            parts.push(format!("hosts {}", self.hosts.join(" ")));
        }
        parts.join("; ")
    }

    /// What it sets, for a list: `Production, Consolas 12 pt`.
    pub fn describe_look(&self) -> String {
        let mut parts = Vec::new();
        if !self.theme.is_empty() {
            parts.push(self.theme.clone());
        }
        match (&self.font, self.font_size) {
            (Some(f), Some(s)) => parts.push(format!("{f} {s} pt")),
            (Some(f), None) => parts.push(f.clone()),
            (None, Some(s)) => parts.push(format!("{s} pt")),
            (None, None) => {}
        }
        parts.join(", ")
    }

    /// Whether it matches a host by name or one of its tags.
    pub fn applies_to(&self, host: &str, tags: &[&str]) -> bool {
        self.matches(host, tags)
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
            // Case folded in any alphabet, as tags are compared
            // elsewhere (`settings::same_tag`); host patterns stay ASCII.
            .any(|p| {
                let p = crate::settings::fold_tag(p);
                tags.iter()
                    .any(|t| match_pattern(&crate::settings::fold_tag(t), &p))
            });
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

    /// The first rule that sets a theme for a host and its tags, if
    /// any (a rule that only sets a font is passed over).
    pub fn rule_for(&self, host: &str, tags: &[&str]) -> Option<&ThemeRule> {
        self.rule_for_any(&[host], tags)
    }

    /// As [`HostThemes::rule_for`], for a host known by several names
    /// (the alias picked and the host name it goes to): one scan in rule
    /// order, so an earlier rule matching any name wins.
    pub fn rule_for_any(&self, hosts: &[&str], tags: &[&str]) -> Option<&ThemeRule> {
        self.first(hosts, tags, |r| !r.theme.is_empty())
    }

    /// The font family the first matching rule that sets one gives.
    pub fn font_for(&self, hosts: &[&str], tags: &[&str]) -> Option<&str> {
        self.first(hosts, tags, |r| r.font.is_some())
            .and_then(|r| r.font.as_deref())
    }

    /// The font size the first matching rule that sets one gives.
    pub fn font_size_for(&self, hosts: &[&str], tags: &[&str]) -> Option<f32> {
        self.first(hosts, tags, |r| r.font_size.is_some())
            .and_then(|r| r.font_size)
    }

    fn first(
        &self,
        hosts: &[&str],
        tags: &[&str],
        sets: impl Fn(&ThemeRule) -> bool,
    ) -> Option<&ThemeRule> {
        self.rules
            .iter()
            .chain(builtin_rules())
            .find(|r| sets(r) && hosts.iter().any(|h| r.matches(h, tags)))
    }

    /// The built-in tag rules, after the user's.
    pub fn builtin() -> &'static [ThemeRule] {
        builtin_rules()
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
    fn tags_match_whatever_their_case_in_any_alphabet() {
        let mut t = HostThemes::new();
        t.push(ThemeRule::new("Dracula").tags(&["équipe*"]));
        assert_eq!(theme(&t, "box", &["ÉQUIPE-1"]), Some("Dracula"));
        // ? stays one character, even where lower case would make two.
        let mut t = HostThemes::new();
        t.push(ThemeRule::new("Dracula").tags(&["?"]));
        assert_eq!(theme(&t, "box", &["İ"]), Some("Dracula"));
        // Final sigma is the same letter as sigma.
        let mut t = HostThemes::new();
        t.push(ThemeRule::new("Dracula").tags(&["ος"]));
        assert_eq!(theme(&t, "box", &["ΟΣ"]), Some("Dracula"));
        let mut t = HostThemes::new();
        t.push(ThemeRule::new("Dracula").tags(&["S?"]));
        assert_eq!(theme(&t, "box", &["ſİ"]), Some("Dracula"));
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

    #[test]
    fn fonts_and_themes_come_from_the_first_rule_that_sets_them() {
        let mut t = HostThemes::new();
        t.push(ThemeRule::new("").tags(&["lab"]).font("Consolas", None));
        t.push(
            ThemeRule::new("Dracula")
                .tags(&["lab", "web"])
                .font("Hack", Some(13.0)),
        );
        assert_eq!(
            theme(&t, "x", &["lab"]),
            Some("Dracula"),
            "the font-only rule is passed over"
        );
        assert_eq!(t.font_for(&["x"], &["lab"]), Some("Consolas"));
        assert_eq!(t.font_size_for(&["x"], &["lab"]), Some(13.0));
        assert_eq!(t.font_for(&["x"], &["web"]), Some("Hack"));
        assert_eq!(
            t.font_for(&["x"], &["prod"]),
            None,
            "built-in rules set no font"
        );
        assert_eq!(theme(&t, "x", &["prod"]), Some("Production"));
        let r = ThemeRule::new("Nord")
            .tags(&["a"])
            .hosts(&["*.b"])
            .font("Hack", Some(9.5));
        assert_eq!(r.describe_match(), "tags a; hosts *.b");
        assert_eq!(r.describe_look(), "Nord, Hack 9.5 pt");
    }
}
