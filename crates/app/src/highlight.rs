//! Keyword highlighting and stop patterns for a pane, from the settings:
//! the rules that apply to its host compiled into what the terminal
//! draws with and stops at.

use std::rc::Rc;

use ferossh_config::highlight::rules_for;
use ferossh_config::{HighlightColour, HighlightRule, Settings};
use ferossh_render::{Color, Rgb};
use ferossh_term::highlight::{Highlighter, Rule, Style, check};

/// What a pane of one host gets.
#[derive(Debug, Default)]
pub struct ForHost {
    pub highlighter: Option<Rc<Highlighter>>,
    /// Patterns of rules with `Stop yes`.
    pub stops: Vec<String>,
    /// Rules left out, as messages naming them.
    pub errors: Vec<String>,
}

/// The highlighting of a pane of `host` (alias and host name), or of a
/// pane without one.
pub fn for_host(settings: &Settings, host: Option<(&str, &str)>) -> ForHost {
    let Some(rules) = rules_for(settings, host) else {
        return ForHost::default();
    };
    let mut out = ForHost::default();
    let mut styled = Vec::new();
    for rule in &rules {
        let Some(pattern) = rule.regex() else {
            continue;
        };
        if let Err(e) = check(&pattern) {
            out.errors.push(format!("{}: {e}", describe(rule)));
            continue;
        }
        if rule.stop {
            out.stops.push(pattern.clone());
        }
        if rule.styled() {
            styled.push(Rule {
                pattern,
                style: style(rule),
            });
        }
    }
    let (highlighter, errors) = Highlighter::new(&styled);
    out.errors.extend(errors.into_iter().map(|(_, e)| e));
    out.highlighter = highlighter.map(Rc::new);
    out
}

fn style(rule: &HighlightRule) -> Style {
    Style {
        fg: rule.colour.map(colour),
        bg: rule.background.map(colour),
        bold: rule.bold,
        underline: rule.underline,
    }
}

fn colour(c: HighlightColour) -> Color {
    match c {
        HighlightColour::Index(i) => Color::Indexed(i),
        HighlightColour::Rgb([r, g, b]) => Color::Rgb(Rgb::new(r, g, b)),
    }
}

/// A rule as the user wrote it, for an error message.
fn describe(rule: &HighlightRule) -> String {
    match (&rule.pattern, rule.words.first()) {
        (Some(p), _) => format!("Highlight pattern {p}"),
        (None, Some(w)) => format!("Highlight words {w}…"),
        (None, None) => "A highlight rule".into(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_render::{Cell, Flags};

    fn painted(h: &Highlighter, text: &str) -> String {
        let mut cells: Vec<Cell> = text
            .chars()
            .map(|c| Cell::new(c, Color::Default, Color::Default, Flags::empty()))
            .collect();
        let mut cache = h.create_cache();
        h.apply(&mut cache, &mut cells);
        cells
            .iter()
            .map(|c| match c.fg {
                Color::Default => '.',
                Color::Indexed(9) => 'r',
                Color::Indexed(11) => 'y',
                Color::Indexed(10) => 'g',
                Color::Indexed(13) => 'm',
                Color::Indexed(14) => 'c',
                _ => '?',
            })
            .collect()
    }

    #[test]
    fn the_standard_set_compiles_and_colours() {
        let got = for_host(&Settings::default(), None);
        assert!(got.errors.is_empty(), "{:?}", got.errors);
        let h = got.highlighter.unwrap();
        assert_eq!(painted(&h, "Error: x"), "rrrrr...");
        assert_eq!(painted(&h, "errorless"), ".........");
        assert_eq!(painted(&h, "WARNING ok"), "yyyyyyy.gg");
        assert_eq!(
            painted(&h, "to 192.168.1.10/24 now"),
            "...ccccccccccccccc...."
        );
        assert_eq!(painted(&h, "999.1.1.1"), ".........");
        assert_eq!(painted(&h, "at 12:34:56"), "...........");
        assert_eq!(
            painted(&h, "ether 00:1a:2B:3c:4d:5e"),
            "......mmmmmmmmmmmmmmmmm"
        );
        assert_eq!(painted(&h, "inet6 fe80::1/64"), "......ccccccc...");
        assert_eq!(painted(&h, "lo ::1"), "...ccc");
        assert_eq!(
            painted(&h, "Active: active (running)"),
            "........gggggggggggggggg"
        );
        assert_eq!(
            painted(&h, "ls: x: No such file or directory"),
            ".......rrrrrrrrrrrrrrrrrrrrrrrrr"
        );
        assert!(got.stops.is_empty());
    }

    #[test]
    fn user_rules_come_first_and_can_stop() {
        let s = Settings::parse(
            "HighlightRule\n    Words ok\n    Colour bright-magenta\n    Stop yes\n\
             HighlightRule\n    Pattern (\n    Colour red\n",
        );
        let got = for_host(&s, Some(("web", "web")));
        assert_eq!(got.errors.len(), 1, "{:?}", got.errors);
        assert!(got.errors[0].contains("Highlight pattern ("));
        assert_eq!(painted(got.highlighter.as_ref().unwrap(), "ok"), "mm");
        assert_eq!(got.stops, [r"(?i)(?:\bok\b)"]);
        let off = Settings::parse("Highlight no\n");
        let got = for_host(&off, None);
        assert!(got.highlighter.is_none() && got.stops.is_empty());
    }
}
