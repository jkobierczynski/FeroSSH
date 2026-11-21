//! How a pane connected to a host looks: its session profile first, then
//! the theme rules (host patterns, ssh_config `Tag` and the connection's
//! own `Tags`; Tools > Theme manager), then the defaults. A rule may set a
//! theme, a font, a size or any of them; each comes from the first rule
//! that sets it.

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
    let (family, size) = own_font(settings, alias, hostname, tag);
    let font = FontSpec::new(
        family.as_deref().unwrap_or(&default_font.family),
        size.unwrap_or(default_font.size_pt),
    );
    let all = ferossh_config::settings::host_tags(tag, profile);
    let tags: Vec<&str> = all.iter().map(String::as_str).collect();
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

/// The font family and size a host's connection or a theme rule sets:
/// the connection's own first, then the first rule that sets each.
/// `None` where neither does (the theme's or the window's then).
pub fn own_font(
    settings: &Settings,
    alias: &str,
    hostname: &str,
    tag: Option<&str>,
) -> (Option<String>, Option<f32>) {
    let profile = settings.profile(alias);
    let all = ferossh_config::settings::host_tags(tag, profile);
    let tags: Vec<&str> = all.iter().map(String::as_str).collect();
    let rules = settings.host_themes();
    let family = profile.and_then(|p| p.font.clone()).or_else(|| {
        rules
            .font_for(alias, &tags)
            .or_else(|| rules.font_for(hostname, &tags))
            .map(str::to_owned)
    });
    let size = profile.and_then(|p| p.font_size).or_else(|| {
        rules
            .font_size_for(alias, &tags)
            .or_else(|| rules.font_size_for(hostname, &tags))
    });
    (family, size)
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

    #[test]
    fn the_connections_own_tags_pick_a_theme() {
        let s = Settings::parse("Profile box\n    Tags web prod\n");
        let theme = |tag| {
            for_host(
                &s,
                "box",
                "10.0.0.9",
                tag,
                &FontSpec::default(),
                &Theme::default(),
            )
            .1
        };
        assert_eq!(theme(None).name, "Production");
        assert_eq!(
            theme(Some("staging")).name,
            "Production",
            "the first rule matching any tag wins, as with Tag alone"
        );
    }

    #[test]
    fn a_rule_sets_the_font_unless_the_connection_does() {
        let s = Settings::parse(
            "ThemeRule\n    Tags lab\n    Font Hack\n    FontSize 14\n\
             Profile box\n    Tags lab\n\
             Profile own\n    Tags lab\n    Font Consolas\n",
        );
        let look = |alias| {
            for_host(
                &s,
                alias,
                "10.0.0.9",
                None,
                &FontSpec::default(),
                &Theme::default(),
            )
        };
        let (font, theme) = look("box");
        assert_eq!((font.family.as_str(), font.size_pt), ("Hack", 14.0));
        assert_eq!(theme, Theme::default(), "a font-only rule leaves the theme");
        let (font, _) = look("own");
        assert_eq!((font.family.as_str(), font.size_pt), ("Consolas", 14.0));
        assert_eq!(own_font(&s, "other", "x", None), (None, None));
    }
}
