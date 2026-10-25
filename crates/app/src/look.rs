//! How a pane connected to a host looks: its session profile first, then
//! the colour theme rules (host patterns and ssh_config `Tag`), then the
//! defaults.

use ferossh_config::Settings;
use ferossh_render::{FontSpec, Rgb, Theme};

/// The font and theme for a pane on `alias` (what the user picked) and
/// `hostname` (where it really goes), tagged `tag` in ssh_config.
/// `default_theme` is the window's (`--theme`, or Nord).
pub fn for_host(
    settings: &Settings,
    alias: &str,
    hostname: &str,
    tag: Option<&str>,
    default_font: &FontSpec,
    default_theme: &Theme,
) -> (FontSpec, Theme) {
    let profile = settings.profile(alias);
    let font = FontSpec::new(
        profile
            .and_then(|p| p.font.as_deref())
            .unwrap_or(&default_font.family),
        profile
            .and_then(|p| p.font_size)
            .unwrap_or(default_font.size_pt),
    );
    let tags: Vec<&str> = tag.into_iter().collect();
    let rules = settings.host_themes();
    let theme = profile
        .and_then(|p| p.theme.as_deref())
        .and_then(Theme::by_name)
        .or_else(|| {
            let rule = rules
                .rule_for(alias, &tags)
                .or_else(|| rules.rule_for(hostname, &tags))?;
            let theme = Theme::by_name(&rule.theme)?;
            Some(match rule.tint {
                Some(([r, g, b], amount)) => theme.tinted(Rgb::new(r, g, b), amount),
                None => theme,
            })
        })
        .unwrap_or_else(|| default_theme.clone());
    (font, theme)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const CONF: &str = "\
ThemeRule
    Hosts *.corp.example
    Theme Dracula
Profile web
    Font Consolas
    FontSize 13
    Theme Solarized Dark
Profile db
    FontSize 9
";

    fn look(alias: &str, hostname: &str, tag: Option<&str>) -> (FontSpec, Theme) {
        let s = Settings::parse(CONF);
        for_host(
            &s,
            alias,
            hostname,
            tag,
            &FontSpec::default(),
            &Theme::default(),
        )
    }

    #[test]
    fn profile_then_rules_then_default() {
        let (font, theme) = look("web", "10.0.0.1", Some("production"));
        assert_eq!((font.family.as_str(), font.size_pt), ("Consolas", 13.0));
        assert_eq!(theme.name, "Solarized Dark", "the profile beats the tag");
        let (font, theme) = look("db", "db.corp.example", None);
        assert_eq!(font.family, FontSpec::default().family);
        assert_eq!(font.size_pt, 9.0);
        assert_eq!(theme.name, "Dracula", "matched by host name");
        assert_eq!(look("x", "y", Some("staging")).1.name, "Staging");
        assert_eq!(look("x", "y", None).1, Theme::default());
        // An unknown theme name in a profile falls through to the rules.
        let s = Settings::parse("Profile p\n Theme Nope\n");
        let (_, t) = for_host(
            &s,
            "p",
            "p",
            Some("prod"),
            &FontSpec::default(),
            &Theme::default(),
        );
        assert_eq!(t.name, "Production");
    }
}
