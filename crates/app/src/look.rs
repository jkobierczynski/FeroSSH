//! How a pane connected to a host looks: enforced theme rules first (see
//! [`Lock`]), then its session profile, then the theme rules (host patterns, ssh_config `Tag` and the connection's
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
    let lock = lock_for(settings, alias, hostname, tag);
    let theme = lock
        .theme
        .clone()
        .or_else(|| {
            profile
                .and_then(|p| p.theme.as_deref())
                .and_then(Theme::by_name)
        })
        .or_else(|| {
            let rule = rules.rule_for_any(&[alias, hostname], &tags)?;
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
/// an enforced rule's first, then the connection's own, then the first
/// rule that sets each.
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
    // One scan in rule order over both names: the first rule that sets
    // each wins, whichever name it matched.
    let names = [alias, hostname];
    let enforced = rules.enforced(&names, &tags);
    let family = enforced
        .font
        .or_else(|| profile.and_then(|p| p.font.clone()))
        .or_else(|| rules.font_for(&names, &tags).map(str::to_owned));
    let size = enforced
        .size
        .or_else(|| profile.and_then(|p| p.font_size))
        .or_else(|| rules.font_size_for(&names, &tags));
    (family, size)
}

/// What enforced theme rules fix on a pane: its theme, font family and
/// size stay these whatever asks for another (Settings > Theme and Font,
/// zoom, a workspace or tmux look, the theme manager). Its tabs and panes
/// opened from it, and its tmux windows, get the same lock.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lock {
    pub theme: Option<Theme>,
    pub font: Option<String>,
    pub size: Option<f32>,
    /// What the rule matches (`tags prod`), to name it.
    pub rule: Option<String>,
    /// The theme is a built-in tag rule's, which nothing changes.
    pub builtin: bool,
}

impl Lock {
    /// `theme`, unless the lock fixes another.
    pub fn theme(&self, theme: Theme) -> Theme {
        self.theme.clone().unwrap_or(theme)
    }

    /// `font`, with the family and size the lock fixes.
    pub fn font(&self, font: FontSpec) -> FontSpec {
        FontSpec::new(
            self.font.as_deref().unwrap_or(&font.family),
            self.size.unwrap_or(font.size_pt),
        )
    }

    /// Why a change to the pane's look is refused.
    pub fn why(&self) -> String {
        let font = self.font.is_some() || self.size.is_some();
        let what = match (self.theme.is_some(), font) {
            (true, true) => "theme and font are",
            (true, false) => "theme is",
            _ => "font is",
        };
        let rule = self.rule.as_deref().unwrap_or("Theme rules");
        if self.builtin {
            let font = if font {
                " Its font is fixed by an enforced rule of your own, which Tools > Theme rules \
                 changes."
            } else {
                " Your own theme rules can still set its font and size."
            };
            return format!(
                "This connection's theme is set by a built-in theme rule ({rule}), which always \
                 applies and cannot be changed.{font}"
            );
        }
        format!(
            "This connection's {what} set by an enforced theme rule ({rule}), so it cannot be \
             changed here. Tools > Theme rules changes the rule."
        )
    }
}

/// The lock enforced theme rules put on a pane on `alias` and `hostname`
/// tagged `tag`; empty when none matches.
pub fn lock_for(settings: &Settings, alias: &str, hostname: &str, tag: Option<&str>) -> Lock {
    let profile = settings.profile(alias);
    let all = ferossh_config::settings::host_tags(tag, profile);
    let tags: Vec<&str> = all.iter().map(String::as_str).collect();
    let e = settings.host_themes().enforced(&[alias, hostname], &tags);
    let theme = e.theme.as_ref().and_then(|(name, tint)| {
        let theme = Theme::by_name(name)?;
        Some(match tint {
            Some(([r, g, b], amount)) => theme.tinted(Rgb::new(*r, *g, *b), *amount),
            None => theme,
        })
    });
    Lock {
        theme,
        font: e.font,
        size: e.size,
        rule: e.rule,
        builtin: e.builtin,
    }
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
        assert_eq!(
            theme.name, "Production",
            "nothing overrides a built-in tag rule, not even the profile"
        );
        assert_eq!(look("web", "10.0.0.1", None).1.name, "Solarized Dark");
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
    fn an_enforced_rule_beats_the_connections_own_look() {
        let s = Settings::parse(
            "ThemeRule\n    Tags prod\n    Theme Production\n    FontSize 14\n    Enforce yes\n\
             Profile web\n    Tags prod\n    Theme Dracula\n    Font Consolas\n    FontSize 9\n",
        );
        let (font, theme) = for_host(
            &s,
            "web",
            "10.0.0.1",
            None,
            &FontSpec::default(),
            &Theme::default(),
        );
        assert_eq!(theme.name, "Production");
        assert_eq!(font.family, "Consolas", "the rule does not fix the family");
        assert_eq!(font.size_pt, 14.0);
        let lock = lock_for(&s, "web", "10.0.0.1", None);
        assert_eq!(lock.theme.as_ref().map(|t| t.name), Some("Production"));
        assert_eq!(lock.theme(Theme::default()).name, "Production");
        let f = lock.font(FontSpec::new("Hack", 20.0));
        assert_eq!((f.family.as_str(), f.size_pt), ("Hack", 14.0));
        // The theme is the built-in prod rule's; the size the user's.
        assert!(lock.builtin);
        assert!(lock.why().contains("built-in theme rule"), "{}", lock.why());
        assert!(lock.why().contains("font is fixed"), "{}", lock.why());
        assert!(lock.why().contains("tags prod"));
        let own = Settings::parse(
            "ThemeRule\n    Tags pci\n    Theme Dracula\n    FontSize 14\n    Enforce yes\n",
        );
        let lock = lock_for(&own, "web", "x", Some("pci"));
        assert!(!lock.builtin);
        assert!(
            lock.why().contains("theme and font are set"),
            "{}",
            lock.why()
        );
        assert!(lock.why().contains("changes the rule"));
        assert_eq!(lock_for(&s, "other", "x", None), Lock::default());
        // A built-in tag rule's lock says it cannot be changed at all.
        let prod = lock_for(&Settings::parse(""), "web", "x", Some("prod"));
        assert!(prod.builtin);
        assert!(prod.why().contains("built-in theme rule"), "{}", prod.why());
        assert!(!prod.why().contains("changes the rule"), "{}", prod.why());
        // Not enforced: the connection's own look wins as before.
        let s = Settings::parse(
            "ThemeRule\n    Tags pci\n    Theme Production\n\
             Profile web\n    Tags pci\n    Theme Dracula\n",
        );
        let (_, theme) = for_host(
            &s,
            "web",
            "x",
            None,
            &FontSpec::default(),
            &Theme::default(),
        );
        assert_eq!(theme.name, "Dracula");
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

    #[test]
    fn the_first_rule_wins_whichever_name_or_tag_it_matched() {
        let s = Settings::parse(
            "ThemeRule\n    Hosts *.corp.example\n    Theme Dracula\n    Font Hack\n    FontSize 14\n\
             ThemeRule\n    Tags lab\n    Theme Nord\n    Font Consolas\n    FontSize 12\n\
             Profile box\n    Tags lab\n",
        );
        let look = |alias, hostname| {
            for_host(
                &s,
                alias,
                hostname,
                None,
                &FontSpec::default(),
                &Theme::default(),
            )
        };
        let (font, theme) = look("box", "db.corp.example");
        assert_eq!((font.family.as_str(), font.size_pt), ("Hack", 14.0));
        assert_eq!(theme.name, "Dracula", "rule 1 matched the host name first");
        let (font, theme) = look("box", "10.0.0.1");
        assert_eq!((font.family.as_str(), theme.name), ("Consolas", "Nord"));
        let (font, theme) = look("x.corp.example", "10.0.0.1");
        assert_eq!(
            (font.family.as_str(), theme.name),
            ("Hack", "Dracula"),
            "by alias"
        );
    }
}
