//! Keyword highlighting rules, as the settings file holds them:
//!
//! ```text
//! Highlight yes
//! HighlightStandard yes
//! HighlightRule
//!     Words error failed "permission denied"
//!     Colour bright-red
//!     Bold yes
//! HighlightRule
//!     Pattern \bOOM-killer\b
//!     CaseSensitive yes
//!     Colour ffffff
//!     Background red
//!     Hosts db-* !db-test
//!     Stop yes
//! Profile lab-switch
//!     Highlight no
//! ```
//!
//! A rule matches a list of words (whole words, any case unless
//! `CaseSensitive yes`) or a regular expression. `Stop yes` also makes
//! the terminal stop following its output at a match. The user's rules
//! come before the standard set, so where two match the user's wins.
//!
//! The terminal crate compiles the patterns, with a linear-time engine
//! and size caps; this module only builds their text.

use crate::pattern::match_host;

/// A highlight colour: one of the theme's 16 colours (or another of the
/// 256), or a fixed RGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightColour {
    Index(u8),
    Rgb([u8; 3]),
}

const NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

impl HighlightColour {
    /// `red`, `bright-red` (or `brightred`), a number 0-255, or `rrggbb`
    /// (`#` optional).
    pub fn parse(text: &str) -> Option<Self> {
        let lower = text.trim().to_ascii_lowercase();
        let (bright, name) = match lower
            .strip_prefix("bright-")
            .or_else(|| lower.strip_prefix("bright"))
        {
            Some(rest) => (true, rest),
            None => (false, lower.as_str()),
        };
        if let Some(i) = NAMES.iter().position(|n| *n == name) {
            return Some(Self::Index(i as u8 + if bright { 8 } else { 0 }));
        }
        let hex = lower.strip_prefix('#').unwrap_or(&lower);
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let v = u32::from_str_radix(hex, 16).ok()?;
            return Some(Self::Rgb([(v >> 16) as u8, (v >> 8) as u8, v as u8]));
        }
        lower.parse::<u8>().ok().map(Self::Index)
    }

    /// The text [`Self::parse`] reads back.
    pub fn text(self) -> String {
        match self {
            Self::Index(i) => match NAMES.get(usize::from(i % 8)) {
                Some(name) if i < 8 => (*name).to_owned(),
                Some(name) if i < 16 => format!("bright-{name}"),
                _ => i.to_string(),
            },
            Self::Rgb([r, g, b]) => format!("{r:02x}{g:02x}{b:02x}"),
        }
    }
}

/// One highlighting rule.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HighlightRule {
    /// Whole words or phrases to match.
    pub words: Vec<String>,
    /// A regular expression to match instead of (or as well as) the words.
    pub pattern: Option<String>,
    pub case_sensitive: bool,
    pub colour: Option<HighlightColour>,
    pub background: Option<HighlightColour>,
    pub bold: bool,
    pub underline: bool,
    /// `Host`-style patterns; empty means every host and local shells.
    pub hosts: Vec<String>,
    /// Stop following the output at a match.
    pub stop: bool,
}

impl HighlightRule {
    /// Whether the rule shows anything.
    pub fn styled(&self) -> bool {
        self.colour.is_some() || self.background.is_some() || self.bold || self.underline
    }

    /// Whether the rule applies to panes of `alias` (or `hostname`); a
    /// pane without a host takes only rules for every host.
    pub fn applies_to(&self, host: Option<(&str, &str)>) -> bool {
        if self.hosts.is_empty() {
            return true;
        }
        let patterns = || self.hosts.iter().map(String::as_str);
        host.is_some_and(|(alias, hostname)| {
            match_host(alias, patterns()) || match_host(hostname, patterns())
        })
    }

    /// The regular expression the rule matches, or `None` for a rule
    /// with neither words nor a pattern.
    pub fn regex(&self) -> Option<String> {
        let mut parts: Vec<String> = self
            .words
            .iter()
            .filter(|w| !w.is_empty())
            .map(|w| word_regex(w))
            .collect();
        if let Some(p) = self.pattern.as_ref().filter(|p| !p.is_empty()) {
            parts.push(format!("(?:{p})"));
        }
        if parts.is_empty() {
            return None;
        }
        let flags = if self.case_sensitive { "" } else { "(?i)" };
        Some(format!("{flags}(?:{})", parts.join("|")))
    }
}

/// A word or phrase as a regular expression: literal, with a word
/// boundary on each side that starts or ends with a word character.
fn word_regex(word: &str) -> String {
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let mut out = String::new();
    if is_word(word.chars().next()) {
        out.push_str(r"\b");
    }
    for c in word.chars() {
        if c.is_ascii_punctuation() && c != '<' && c != '>' {
            out.push('\\');
        }
        out.push(c);
    }
    if is_word(word.chars().last()) {
        out.push_str(r"\b");
    }
    out
}

/// The standard set, used unless `HighlightStandard no`: trouble in red,
/// warnings in yellow, success in green, addresses in cyan and magenta.
pub fn standard_rules() -> Vec<HighlightRule> {
    let words = |words: &[&str], colour: u8, bold: bool| HighlightRule {
        words: words.iter().map(|w| (*w).to_owned()).collect(),
        colour: Some(HighlightColour::Index(colour)),
        bold,
        ..HighlightRule::default()
    };
    let pattern = |p: &str, colour: u8| HighlightRule {
        pattern: Some(p.to_owned()),
        colour: Some(HighlightColour::Index(colour)),
        ..HighlightRule::default()
    };
    vec![
        words(
            &[
                "error",
                "errors",
                "fail",
                "failed",
                "failure",
                "fatal",
                "critical",
                "panic",
                "denied",
                "refused",
                "unreachable",
                "segfault",
                "segmentation fault",
                "no such file or directory",
                "not found",
            ],
            9,
            true,
        ),
        words(
            &[
                "warn",
                "warning",
                "warnings",
                "deprecated",
                "timeout",
                "timed out",
                "retrying",
            ],
            11,
            true,
        ),
        words(
            &[
                "ok",
                "success",
                "successful",
                "successfully",
                "succeeded",
                "passed",
                "active (running)",
            ],
            10,
            false,
        ),
        // MAC addresses, before IPv6 so 6 groups of 2 are not read as one.
        pattern(r"\b[0-9a-fA-F]{2}(?:[:-][0-9a-fA-F]{2}){5}\b", 13),
        // IPv4 with an optional prefix length.
        pattern(
            r"\b(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(?:/[0-9]{1,2})?\b",
            14,
        ),
        // IPv6: eight groups, or a shortened one with `::`.
        pattern(
            r"\b(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}\b|(?:\b[0-9a-fA-F]{1,4}:){1,6}:(?:[0-9a-fA-F]{1,4}\b(?::[0-9a-fA-F]{1,4}\b)*)?|::(?:[0-9a-fA-F]{1,4}\b(?::[0-9a-fA-F]{1,4}\b)*)",
            14,
        ),
    ]
}

/// The rules a pane of `host` uses, the user's first: `None` when
/// highlighting is off for it.
pub fn rules_for(
    settings: &crate::Settings,
    host: Option<(&str, &str)>,
) -> Option<Vec<HighlightRule>> {
    let profile_on = host
        .and_then(|(alias, _)| settings.profile(alias))
        .and_then(|p| p.highlight);
    if !profile_on.unwrap_or(settings.highlight) {
        return None;
    }
    let mut rules: Vec<HighlightRule> = settings
        .highlight_rules
        .iter()
        .filter(|r| r.applies_to(host))
        .cloned()
        .collect();
    if settings.highlight_standard {
        rules.extend(standard_rules());
    }
    Some(rules)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn colours_round_trip() {
        for text in [
            "red",
            "bright-cyan",
            "200",
            "ff8000",
            "black",
            "bright-white",
        ] {
            let c = HighlightColour::parse(text).unwrap();
            assert_eq!(HighlightColour::parse(&c.text()), Some(c), "{text}");
        }
        assert_eq!(
            HighlightColour::parse("BrightRed"),
            Some(HighlightColour::Index(9))
        );
        assert_eq!(
            HighlightColour::parse("#FF0000"),
            Some(HighlightColour::Rgb([255, 0, 0]))
        );
        assert_eq!(HighlightColour::parse("purple"), None);
        assert_eq!(HighlightColour::parse("256"), None);
    }

    #[test]
    fn words_become_a_literal_pattern() {
        let rule = HighlightRule {
            words: vec!["error".into(), "a.b".into(), "active (running)".into()],
            ..HighlightRule::default()
        };
        assert_eq!(
            rule.regex().unwrap(),
            r"(?i)(?:\berror\b|\ba\.b\b|\bactive \(running\))"
        );
        let rule = HighlightRule {
            pattern: Some("x+".into()),
            case_sensitive: true,
            ..HighlightRule::default()
        };
        assert_eq!(rule.regex().unwrap(), "(?:(?:x+))");
        assert_eq!(HighlightRule::default().regex(), None);
    }

    #[test]
    fn hosts_limit_a_rule() {
        let rule = HighlightRule {
            hosts: vec!["db-*".into(), "!db-test".into()],
            ..HighlightRule::default()
        };
        assert!(rule.applies_to(Some(("db-1", "10.0.0.1"))));
        assert!(!rule.applies_to(Some(("db-test", "db-test"))));
        assert!(!rule.applies_to(Some(("web", "web.example.com"))));
        assert!(!rule.applies_to(None));
        assert!(HighlightRule::default().applies_to(None));
    }

    #[test]
    fn rules_for_a_host() {
        let text = "Highlight yes\nHighlightRule\n    Words boom\n    Colour red\n    Hosts lab-*\n\
                    Profile quiet\n    Highlight no\n";
        let s = crate::Settings::parse(text);
        let lab = rules_for(&s, Some(("lab-1", "lab-1"))).unwrap();
        assert_eq!(lab[0].words, vec!["boom".to_owned()]);
        assert_eq!(lab.len(), 1 + standard_rules().len());
        let web = rules_for(&s, Some(("web", "web"))).unwrap();
        assert_eq!(web.len(), standard_rules().len());
        assert!(rules_for(&s, Some(("quiet", "quiet"))).is_none());
        let off = crate::Settings::parse("Highlight no\nProfile loud\n    Highlight yes\n");
        assert!(rules_for(&off, None).is_none());
        assert!(rules_for(&off, Some(("loud", "loud"))).is_some());
    }
}
