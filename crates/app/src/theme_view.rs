//! Tools > Theme manager: what the page lists. Four groups: the theme
//! rules (tags or host patterns that pick a theme and a font), the
//! built-in themes, the user's own themes (the themes folder) and the
//! terminal fonts, each with the saved connections that use it. Pure, so
//! it is tested on any platform; `theme_panel.rs` draws it.

use ferossh_config::ThemeRule;

/// A saved connection, as the theme manager sees it: what matches a
/// rule, and the look it gets when opened.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Connection {
    pub name: String,
    pub hostname: Option<String>,
    pub tags: Vec<String>,
    /// The theme it opens with.
    pub theme: String,
    /// The font family it opens with.
    pub font: String,
}

/// One of the terminal fonts offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Font {
    pub family: String,
    /// Installed on this PC.
    pub installed: bool,
    /// Comes with Windows (else a free download).
    pub windows: bool,
}

/// Which group a row is in, in the order shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Rules,
    BuiltIn,
    Custom,
    Fonts,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Rules, Group::BuiltIn, Group::Custom, Group::Fonts];

    pub fn title(self) -> &'static str {
        match self {
            Group::Rules => "Theme rules (by tag or host)",
            Group::BuiltIn => "Built-in themes",
            Group::Custom => "Your themes",
            Group::Fonts => "Fonts",
        }
    }
}

/// One row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub group: Group,
    /// Its place: in the settings' rules, or in its own list.
    pub index: usize,
    /// The rule, for a rule row.
    pub rule: Option<ThemeRule>,
    /// The theme's or font's name; a rule's match.
    pub name: String,
    pub details: String,
    /// The connections it covers or that use it, by name, sorted.
    pub used_by: Vec<String>,
    /// Shown grey: a font that is not installed.
    pub dim: bool,
}

impl Row {
    /// The theme a new rule made from this row starts with.
    pub fn preset_theme(&self) -> Option<&str> {
        matches!(self.group, Group::BuiltIn | Group::Custom).then_some(self.name.as_str())
    }

    /// The font a new rule made from this row starts with.
    pub fn preset_font(&self) -> Option<&str> {
        (self.group == Group::Fonts).then_some(self.name.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Name,
    Details,
    Connections,
}

impl Column {
    pub const ALL: [Column; 3] = [Column::Name, Column::Details, Column::Connections];

    pub fn title(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Details => "Sets / kind",
            Column::Connections => "Connections",
        }
    }

    /// Width in DIPs.
    pub fn width(self) -> i32 {
        match self {
            Column::Name => 220,
            Column::Details => 240,
            Column::Connections => 320,
        }
    }
}

fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort_by_key(|n| n.to_lowercase());
    names
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Whether `rule` matches connection `c`, by its name or host name.
fn covers(rule: &ThemeRule, c: &Connection) -> bool {
    let tags: Vec<&str> = c.tags.iter().map(String::as_str).collect();
    rule.applies_to(&c.name, &tags)
        || c.hostname
            .as_deref()
            .is_some_and(|h| rule.applies_to(h, &tags))
}

/// The rows: `rules` (the user's) with the connections each matches,
/// then the built-in themes (`builtin`: name and kind, such as Dark),
/// the user's own themes and `fonts`, each with the connections that
/// open with it.
pub fn rows(
    rules: &[ThemeRule],
    builtin: &[(&str, &str)],
    custom: &[&str],
    fonts: &[Font],
    all: &[Connection],
) -> Vec<Row> {
    let mut out = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        out.push(Row {
            group: Group::Rules,
            index,
            rule: Some(rule.clone()),
            name: rule.describe_match(),
            details: rule.describe_look(),
            used_by: sorted(
                all.iter()
                    .filter(|c| covers(rule, c))
                    .map(|c| c.name.clone())
                    .collect(),
            ),
            dim: false,
        });
    }
    let theme_users = |name: &str| {
        sorted(
            all.iter()
                .filter(|c| same(&c.theme, name))
                .map(|c| c.name.clone())
                .collect(),
        )
    };
    for (index, (name, kind)) in builtin.iter().enumerate() {
        // Which tags pick it with no rule of the user's.
        let by_tag = ferossh_config::HostThemes::builtin()
            .iter()
            .filter(|r| same(&r.theme, name))
            .map(|r| r.tags.join(", "))
            .collect::<Vec<_>>();
        let details = if by_tag.is_empty() {
            (*kind).to_owned()
        } else {
            format!("{kind}; tags {}", by_tag.join(", "))
        };
        out.push(Row {
            group: Group::BuiltIn,
            index,
            rule: None,
            name: (*name).to_owned(),
            details,
            used_by: theme_users(name),
            dim: false,
        });
    }
    for (index, name) in custom.iter().enumerate() {
        out.push(Row {
            group: Group::Custom,
            index,
            rule: None,
            name: (*name).to_owned(),
            details: "Themes folder".to_owned(),
            used_by: theme_users(name),
            dim: false,
        });
    }
    for (index, f) in fonts.iter().enumerate() {
        let details = match (f.installed, f.windows) {
            (true, true) => "Installed (comes with Windows)",
            (true, false) => "Installed",
            (false, true) => "Not installed",
            (false, false) => "Not installed (free download)",
        };
        out.push(Row {
            group: Group::Fonts,
            index,
            rule: None,
            name: f.family.clone(),
            details: details.to_owned(),
            used_by: sorted(
                all.iter()
                    .filter(|c| same(&c.font, &f.family))
                    .map(|c| c.name.clone())
                    .collect(),
            ),
            dim: !f.installed,
        });
    }
    out
}

/// Cell `column` of `row`, cleaned of control and direction characters.
pub fn text(row: &Row, column: Column) -> String {
    let raw = match column {
        Column::Name => row.name.clone(),
        Column::Details => row.details.clone(),
        Column::Connections => match row.used_by.len() {
            0 => String::new(),
            n if n <= 3 => row.used_by.join(", "),
            n => format!(
                "{n}: {}, ...",
                row.used_by.get(..3).unwrap_or_default().join(", ")
            ),
        },
    };
    clean(&raw)
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(400)
        .collect()
}

/// The rows a search keeps: every word in a row's name, details or
/// connections (any case); `-word` leaves rows with it out; the word
/// `unused` keeps rows no connection uses.
pub fn filter<'a>(rows: &'a [Row], query: &str) -> Vec<&'a Row> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    rows.iter()
        .filter(|r| {
            let hay = [r.name.as_str(), r.details.as_str(), &r.used_by.join(" ")]
                .join(" ")
                .to_lowercase();
            words.iter().all(|w| match w.strip_prefix('-') {
                Some(not) if !not.is_empty() => !hay.contains(not),
                _ if w == "unused" => r.used_by.is_empty(),
                _ => hay.contains(w.as_str()),
            })
        })
        .collect()
}

/// Sorts `rows` within their groups: by `column`, or by their own order
/// with none; their own order breaks ties.
pub fn sort(rows: &mut [&Row], column: Option<Column>, descending: bool) {
    rows.sort_by(|a, b| {
        let by = match column {
            None => a.index.cmp(&b.index),
            Some(Column::Connections) => a.used_by.len().cmp(&b.used_by.len()),
            Some(c) => text(a, c).to_lowercase().cmp(&text(b, c).to_lowercase()),
        };
        let by = if descending { by.reverse() } else { by };
        a.group.cmp(&b.group).then(by).then(a.index.cmp(&b.index))
    });
}

/// The line under the list.
pub fn summary(shown: &[&Row], rules: usize) -> String {
    let count = |g| shown.iter().filter(|r| r.group == g).count();
    let rules_text = match rules {
        0 => {
            "No theme rules of your own yet: New rule... picks a theme and font by tag.".to_owned()
        }
        1 => "1 theme rule.".to_owned(),
        n => format!("{n} theme rules."),
    };
    format!(
        "{rules_text} {} themes, {} fonts shown. A connection's own theme or font beats a rule; \
         the first rule that sets each wins.",
        count(Group::BuiltIn) + count(Group::Custom),
        count(Group::Fonts),
    )
}

/// The question before deleting rule `row`.
pub fn delete_question(row: &Row) -> String {
    let mut text = format!(
        "Delete theme rule {} ({}: {})?",
        row.index + 1,
        clean(&row.name),
        clean(&row.details)
    );
    if !row.used_by.is_empty() {
        let names = row.used_by.iter().map(|n| clean(n)).collect::<Vec<_>>();
        text.push_str(&format!(
            " {} open{} with another look from then on, unless another rule matches.",
            crate::bulk::name_list(&names),
            if names.len() == 1 { "s" } else { "" }
        ));
    }
    text
}

/// What the rule form typed: host patterns, tags, theme, font, size.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Typed {
    pub hosts: String,
    pub tags: String,
    pub theme: String,
    pub font: String,
    pub size: String,
}

/// Checks what was typed and makes the rule (keeping `old`'s tint):
/// a known theme name (any case, written as listed), a font from the list
/// or one installed, a size of 4 to 96 points.
pub fn check(
    typed: &Typed,
    old: Option<&ThemeRule>,
    themes: &[&str],
    fonts: &[&str],
    installed: impl Fn(&str) -> bool,
) -> Result<ThemeRule, String> {
    use ferossh_config::host_policy::words;
    let theme = typed.theme.trim();
    let theme = if theme.is_empty() {
        String::new()
    } else {
        match themes.iter().find(|t| same(t, theme)) {
            Some(t) => (*t).to_owned(),
            None => {
                return Err(format!(
                    "No theme is called \"{}\". Type a name as the Theme manager lists it, \
                     or leave Theme empty.",
                    clean(theme)
                ));
            }
        }
    };
    let font = typed.font.trim();
    let font = if font.is_empty() {
        None
    } else if let Some(f) = fonts.iter().find(|f| same(f, font)) {
        Some((*f).to_owned())
    } else if installed(font) {
        Some(font.to_owned())
    } else {
        return Err(format!(
            "\"{}\" is not one of the fonts listed or installed. Type a font as the Theme \
             manager lists it, or leave Font empty.",
            clean(font)
        ));
    };
    let size = typed.size.trim().trim_end_matches("pt").trim();
    let font_size = if size.is_empty() {
        None
    } else {
        match size.replace(',', ".").parse::<f32>() {
            Ok(v) if (4.0..=96.0).contains(&v) => Some(v),
            _ => return Err("Font size is a number of points from 4 to 96.".to_owned()),
        }
    };
    let mut rule = ThemeRule::new(&theme);
    rule.hosts = words(&typed.hosts);
    rule.tags = words(&typed.tags);
    rule.font = font;
    rule.font_size = font_size;
    rule.tint = old.and_then(|o| o.tint);
    if rule.hosts.is_empty() && rule.tags.is_empty() {
        return Err(
            "Name the connections the rule is for: a tag or a host name pattern.".to_owned(),
        );
    }
    if !rule.sets_something() {
        return Err("Set a theme, a font or a font size.".to_owned());
    }
    Ok(rule)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn conn(name: &str, tags: &[&str], theme: &str, font: &str) -> Connection {
        Connection {
            name: name.into(),
            hostname: Some(format!("{name}.example")),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            theme: theme.into(),
            font: font.into(),
        }
    }

    fn sample() -> Vec<Row> {
        let rules = vec![
            ThemeRule::new("Dracula")
                .tags(&["lab"])
                .font("Hack", Some(12.0)),
            ThemeRule::new("")
                .hosts(&["*.nowhere"])
                .font("Consolas", None),
        ];
        let fonts = [
            Font {
                family: "Consolas".into(),
                installed: true,
                windows: true,
            },
            Font {
                family: "Hack".into(),
                installed: false,
                windows: false,
            },
        ];
        let all = [
            conn("pi", &["Lab"], "Dracula", "Hack"),
            conn("web", &["prod"], "Production", "Consolas"),
        ];
        rows(
            &rules,
            &[
                ("Nord", "Dark"),
                ("Production", "Warning"),
                ("Dracula", "Dark"),
            ],
            &["Mine"],
            &fonts,
            &all,
        )
    }

    #[test]
    fn rows_list_rules_themes_and_fonts_with_their_connections() {
        let rows = sample();
        assert_eq!(rows.len(), 2 + 3 + 1 + 2);
        assert_eq!(rows[0].used_by, ["pi"], "tags match in any case");
        assert_eq!(text(&rows[0], Column::Name), "tags lab");
        assert_eq!(text(&rows[0], Column::Details), "Dracula, Hack 12 pt");
        assert!(rows[1].used_by.is_empty());
        assert_eq!(rows[3].details, "Warning; tags production, prod");
        assert_eq!(rows[3].used_by, ["web"]);
        assert_eq!(rows[5].group, Group::Custom);
        let hack = rows.iter().find(|r| r.name == "Hack").unwrap();
        assert!(hack.dim);
        assert_eq!(hack.used_by, ["pi"]);
        assert_eq!(hack.preset_font(), Some("Hack"));
        assert_eq!(rows[2].preset_theme(), Some("Nord"));
        assert_eq!(rows[0].preset_theme(), None);
    }

    #[test]
    fn search_and_sort_keep_the_groups() {
        let rows = sample();
        assert_eq!(filter(&rows, "dracula").len(), 2);
        assert!(filter(&rows, "unused").iter().all(|r| r.used_by.is_empty()));
        assert!(
            filter(&rows, "-installed")
                .iter()
                .all(|r| r.group != Group::Fonts)
        );
        let mut all: Vec<&Row> = rows.iter().collect();
        sort(&mut all, Some(Column::Name), true);
        let groups: Vec<Group> = all.iter().map(|r| r.group).collect();
        let mut g = groups.clone();
        g.sort();
        assert_eq!(groups, g, "groups stay in order");
        assert_eq!(
            all[2].name, "Production",
            "names backwards within the group"
        );
        sort(&mut all, None, false);
        assert_eq!(all[2].name, "Nord");
        assert!(summary(&all, 2).starts_with("2 theme rules. 4 themes, 2 fonts shown."));
    }

    #[test]
    fn the_form_is_checked() {
        let themes = ["Nord", "Production"];
        let fonts = ["Consolas", "Hack"];
        let typed = |tags: &str, theme: &str, font: &str, size: &str| Typed {
            hosts: String::new(),
            tags: tags.into(),
            theme: theme.into(),
            font: font.into(),
            size: size.into(),
        };
        let ok = check(
            &typed("lab \"my team\"", "nord", "hack", "11,5"),
            None,
            &themes,
            &fonts,
            |_| false,
        )
        .unwrap();
        assert_eq!(ok.theme, "Nord");
        assert_eq!(ok.tags, ["lab", "my team"]);
        assert_eq!(ok.font.as_deref(), Some("Hack"));
        assert_eq!(ok.font_size, Some(11.5));
        let only_font = check(
            &typed("lab", "", "Some Font", ""),
            None,
            &themes,
            &fonts,
            |f| f == "Some Font",
        )
        .unwrap();
        assert_eq!(only_font.theme, "");
        assert!(
            check(&typed("lab", "Nope", "", ""), None, &themes, &fonts, |_| {
                true
            })
            .is_err()
        );
        assert!(
            check(&typed("lab", "", "Nope", ""), None, &themes, &fonts, |_| {
                false
            })
            .is_err()
        );
        assert!(
            check(&typed("lab", "", "", "200"), None, &themes, &fonts, |_| {
                false
            })
            .is_err()
        );
        assert!(check(&typed("", "Nord", "", ""), None, &themes, &fonts, |_| false).is_err());
        assert!(check(&typed("lab", "", "", ""), None, &themes, &fonts, |_| false).is_err());
        let old = ThemeRule::new("Nord").tags(&["x"]).tint([1, 2, 3], 9);
        let kept = check(
            &typed("x", "Production", "", ""),
            Some(&old),
            &themes,
            &fonts,
            |_| false,
        )
        .unwrap();
        assert_eq!(kept.tint, Some(([1, 2, 3], 9)));
    }

    #[test]
    fn the_delete_question_names_who_changes() {
        let rows = sample();
        let q = delete_question(&rows[0]);
        assert!(
            q.starts_with("Delete theme rule 1 (tags lab: Dracula, Hack 12 pt)?"),
            "{q}"
        );
        assert!(q.contains("pi opens with another look"), "{q}");
    }
}
