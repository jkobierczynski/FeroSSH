//! Tools > Theme manager and Tools > Theme rules: what the two pages
//! list. The theme manager is the looks: the built-in themes, the
//! user's own themes (the themes folder) and the terminal fonts, each
//! with the saved connections that open with it. Theme rules are the
//! user's rules (tags or host patterns that pick a theme and a font), in
//! the order they are tried, then the built-in tag rules, each with the
//! connections it matches. Pure, so it is tested on any platform;
//! `theme_panel.rs` draws it.

use ferossh_config::ThemeRule;

/// A saved connection, as theme rules see it: what matches a rule.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Connection {
    pub name: String,
    pub hostname: Option<String>,
    pub tags: Vec<String>,
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

/// Which of the two pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    /// Tools > Theme manager: themes and fonts.
    #[default]
    Looks,
    /// Tools > Theme rules: which connections get which look.
    Rules,
}

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Page::Looks => "Theme manager",
            Page::Rules => "Theme rules",
        }
    }
}

/// Which group a row is in, in the order shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Rules,
    /// The built-in tag rules (Production, Staging), tried after the
    /// user's; they cannot be changed.
    BuiltInRules,
    /// The built-in themes, by kind.
    Dark,
    Light,
    Warning,
    Classic,
    /// The themes folder's.
    Custom,
    Fonts,
}

impl Group {
    pub const ALL: [Group; 8] = [
        Group::Rules,
        Group::BuiltInRules,
        Group::Dark,
        Group::Light,
        Group::Warning,
        Group::Classic,
        Group::Custom,
        Group::Fonts,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Rules => "Your rules (tried from the top)",
            Group::BuiltInRules => "Built-in rules (tried after yours)",
            Group::Dark => "Dark themes",
            Group::Light => "Light themes",
            Group::Warning => "Warning themes",
            Group::Classic => "Classic themes",
            Group::Custom => "Your themes",
            Group::Fonts => "Fonts",
        }
    }

    /// The page it is listed on.
    pub fn page(self) -> Page {
        match self {
            Group::Rules | Group::BuiltInRules => Page::Rules,
            _ => Page::Looks,
        }
    }

    /// A group of themes.
    pub fn is_themes(self) -> bool {
        matches!(
            self,
            Group::Dark | Group::Light | Group::Warning | Group::Classic | Group::Custom
        )
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
    /// The connections a rule covers, by name, sorted.
    pub used_by: Vec<String>,
    /// The rules that pick a theme or font, as their matches (`tags
    /// lab`), yours first.
    pub in_rules: Vec<String>,
    /// Shown grey: a font that is not installed.
    pub dim: bool,
}

impl Row {
    /// The theme a new rule made from this row starts with.
    pub fn preset_theme(&self) -> Option<&str> {
        self.group.is_themes().then_some(self.name.as_str())
    }

    /// The font a new rule made from this row starts with.
    pub fn preset_font(&self) -> Option<&str> {
        (self.group == Group::Fonts).then_some(self.name.as_str())
    }

    /// One of the user's rules: it can be changed, deleted and moved.
    pub fn is_own_rule(&self) -> bool {
        self.group == Group::Rules
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Name,
    Details,
    Connections,
    /// The rules that use a theme or font.
    Rules,
}

impl Column {
    /// The columns `page` shows: the theme manager only names its
    /// themes and fonts (the groups say what they are).
    pub fn on(page: Page) -> &'static [Column] {
        match page {
            Page::Looks => &[Column::Name, Column::Rules],
            Page::Rules => &[Column::Name, Column::Details, Column::Connections],
        }
    }

    /// The heading on `page`.
    pub fn title(self, page: Page) -> &'static str {
        match (self, page) {
            (Column::Name, Page::Looks) => "Name",
            (Column::Name, Page::Rules) => "Matches",
            (Column::Details, _) => "Sets",
            (Column::Connections, _) => "Connections",
            (Column::Rules, _) => "Rules",
        }
    }

    /// Width in DIPs on `page`.
    pub fn width(self, page: Page) -> i32 {
        match (self, page) {
            (Column::Name, Page::Looks) => 240,
            (Column::Rules, _) => 360,
            (Column::Name, Page::Rules) => 220,
            (Column::Details, _) => 240,
            (Column::Connections, _) => 320,
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

/// The rows: `rules` (the user's) and the built-in tag rules, with the
/// connections each matches; then the looks: the built-in themes
/// (`builtin`: each with its group, such as [`Group::Dark`]), the user's
/// own themes and `fonts`.
pub fn rows(
    rules: &[ThemeRule],
    builtin: &[(&str, Group)],
    custom: &[&str],
    fonts: &[Font],
    all: &[Connection],
) -> Vec<Row> {
    let mut out = Vec::new();
    let rule_row = |group, index, rule: &ThemeRule| Row {
        group,
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
        in_rules: Vec::new(),
        dim: false,
    };
    for (index, rule) in rules.iter().enumerate() {
        out.push(rule_row(Group::Rules, index, rule));
    }
    for (index, rule) in ferossh_config::HostThemes::builtin().iter().enumerate() {
        out.push(rule_row(Group::BuiltInRules, index, rule));
    }
    let all_rules: Vec<&ThemeRule> = rules
        .iter()
        .chain(ferossh_config::HostThemes::builtin())
        .collect();
    let in_rules = |uses: &dyn Fn(&ThemeRule) -> bool| {
        all_rules
            .iter()
            .filter(|r| uses(r))
            .map(|r| r.describe_match())
            .collect::<Vec<_>>()
    };
    let look = |group, index, name: &str, details: &str, dim| Row {
        group,
        index,
        rule: None,
        name: name.to_owned(),
        // Not shown: the group says it. Searched, so "installed" or
        // "download" finds fonts.
        details: details.to_owned(),
        used_by: Vec::new(),
        in_rules: if group == Group::Fonts {
            in_rules(&|r| r.font.as_deref().is_some_and(|f| same(f, name)))
        } else {
            in_rules(&|r| same(&r.theme, name))
        },
        dim,
    };
    for (index, (name, group)) in builtin.iter().enumerate() {
        out.push(look(*group, index, name, "", false));
    }
    for (index, name) in custom.iter().enumerate() {
        out.push(look(Group::Custom, index, name, "", false));
    }
    for (index, f) in fonts.iter().enumerate() {
        let details = match (f.installed, f.windows) {
            (true, true) => "installed, comes with Windows",
            (true, false) => "installed",
            (false, true) => "not installed",
            (false, false) => "not installed, free download",
        };
        out.push(look(Group::Fonts, index, &f.family, details, !f.installed));
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
        Column::Rules => row.in_rules.join("; "),
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

/// The rows a search keeps: every word in a row's name, details,
/// connections or rules (any case); `-word` leaves rows with it out; the
/// word `unused` keeps rows no connection (a rule) or rule (a theme or
/// font) uses.
pub fn filter<'a>(rows: impl IntoIterator<Item = &'a Row>, query: &str) -> Vec<&'a Row> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    rows.into_iter()
        .filter(|r| {
            let hay = [
                r.name.as_str(),
                r.details.as_str(),
                &r.used_by.join(" "),
                &r.in_rules.join(" "),
            ]
            .join(" ")
            .to_lowercase();
            words.iter().all(|w| match w.strip_prefix('-') {
                Some(not) if !not.is_empty() => !hay.contains(not),
                _ if w == "unused" => r.used_by.is_empty() && r.in_rules.is_empty(),
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
            Some(Column::Rules) => a.in_rules.len().cmp(&b.in_rules.len()),
            Some(c) => text(a, c).to_lowercase().cmp(&text(b, c).to_lowercase()),
        };
        let by = if descending { by.reverse() } else { by };
        a.group.cmp(&b.group).then(by).then(a.index.cmp(&b.index))
    });
}

/// The rows `page` lists.
pub fn on_page(rows: &[Row], page: Page) -> Vec<&Row> {
    rows.iter().filter(|r| r.group.page() == page).collect()
}

/// The line under the list on `page`.
pub fn summary(page: Page, shown: &[&Row], rules: usize) -> String {
    let count = |g| shown.iter().filter(|r| r.group == g).count();
    match page {
        Page::Looks => format!(
            "{} themes, {} fonts shown. Use in a rule... gives one to connections by tag \
             or host.",
            shown.iter().filter(|r| r.group.is_themes()).count(),
            count(Group::Fonts),
        ),
        Page::Rules => {
            let rules_text = match rules {
                0 => "No theme rules of your own yet: New rule... picks a theme and font by tag."
                    .to_owned(),
                1 => "1 theme rule.".to_owned(),
                n => format!("{n} theme rules."),
            };
            format!(
                "{rules_text} The first rule that sets a theme, font or size wins; \
                 a connection's own look beats them all."
            )
        }
    }
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

/// Which way Move up / Move down takes a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
}

/// Where rule `at` of `len` goes when moved: `None` when it is already
/// first (up) or last (down).
pub fn moved_to(len: usize, at: usize, way: Move) -> Option<usize> {
    match way {
        Move::Up => at.checked_sub(1),
        Move::Down => (at + 1 < len).then_some(at + 1),
    }
}

/// What the rule form typed: host patterns, tags, theme, font, size,
/// and whether the rule is enforced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Typed {
    pub hosts: String,
    pub tags: String,
    pub theme: String,
    pub font: String,
    pub size: String,
    pub enforce: bool,
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
    rule.enforce = typed.enforce;
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

    fn conn(name: &str, tags: &[&str]) -> Connection {
        Connection {
            name: name.into(),
            hostname: Some(format!("{name}.example")),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
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
        let all = [conn("pi", &["Lab"]), conn("web", &["prod"])];
        rows(
            &rules,
            &[
                ("Nord", Group::Dark),
                ("Production", Group::Warning),
                ("Dracula", Group::Dark),
            ],
            &["Mine"],
            &fonts,
            &all,
        )
    }

    #[test]
    fn rules_list_their_connections_and_looks_their_rules() {
        let rows = sample();
        assert_eq!(rows.len(), 2 + 3 + 3 + 1 + 2);
        assert_eq!(rows[0].used_by, ["pi"], "tags match in any case");
        assert_eq!(text(&rows[0], Column::Name), "tags lab");
        assert_eq!(text(&rows[0], Column::Details), "Dracula, Hack 12 pt");
        assert!(rows[1].used_by.is_empty());
        assert_eq!(rows[2].group, Group::BuiltInRules);
        assert_eq!(rows[2].used_by, ["web"]);
        assert!(!rows[2].is_own_rule());
        let prod = rows.iter().find(|r| r.name == "Production").unwrap();
        assert_eq!(prod.group, Group::Warning);
        assert_eq!(text(prod, Column::Rules), "tags production prod");
        let dracula = rows.iter().find(|r| r.name == "Dracula").unwrap();
        assert_eq!(dracula.in_rules, ["tags lab"]);
        assert!(
            dracula.used_by.is_empty(),
            "the theme manager lists no connections"
        );
        let hack = rows.iter().find(|r| r.name == "Hack").unwrap();
        assert!(hack.dim);
        assert_eq!(hack.in_rules, ["tags lab"]);
        assert_eq!(hack.preset_font(), Some("Hack"));
        assert_eq!(dracula.preset_theme(), Some("Dracula"));
        assert_eq!(rows[0].preset_theme(), None);
        let mine = rows.iter().find(|r| r.name == "Mine").unwrap();
        assert_eq!(mine.preset_theme(), Some("Mine"));
    }

    #[test]
    fn each_page_lists_its_own_groups() {
        let rows = sample();
        let looks = on_page(&rows, Page::Looks);
        assert!(looks.iter().all(|r| r.rule.is_none()));
        assert_eq!(looks.len(), 6);
        let rules = on_page(&rows, Page::Rules);
        assert_eq!(rules.len(), 5);
        assert_eq!(Column::on(Page::Looks), [Column::Name, Column::Rules]);
        assert_eq!(Column::on(Page::Rules).len(), 3);
    }

    #[test]
    fn search_and_sort_keep_the_groups() {
        let rows = sample();
        assert_eq!(filter(&rows, "dracula").len(), 2);
        assert!(
            filter(&rows, "unused")
                .iter()
                .all(|r| r.used_by.is_empty() && r.in_rules.is_empty())
        );
        assert!(filter(&rows, "unused").iter().any(|r| r.name == "Mine"));
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
        let dark: Vec<&str> = all
            .iter()
            .filter(|r| r.group == Group::Dark)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(
            dark,
            ["Nord", "Dracula"],
            "names backwards within the group"
        );
        sort(&mut all, None, false);
        let dark: Vec<&str> = all
            .iter()
            .filter(|r| r.group == Group::Dark)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(dark, ["Nord", "Dracula"]);
        let looks = on_page(&rows, Page::Looks);
        assert!(summary(Page::Looks, &looks, 2).starts_with("4 themes, 2 fonts shown."));
        let rules = on_page(&rows, Page::Rules);
        assert!(summary(Page::Rules, &rules, 2).starts_with("2 theme rules."));
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
            enforce: false,
        };
        let enforced = check(
            &Typed {
                enforce: true,
                ..typed("prod", "Production", "", "")
            },
            None,
            &themes,
            &fonts,
            |_| false,
        )
        .unwrap();
        assert!(enforced.enforce);
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

    #[test]
    fn rules_move_up_and_down() {
        let mut list = [
            ThemeRule::new("Nord").tags(&["a"]),
            ThemeRule::new("Dracula").tags(&["b"]),
        ];
        assert_eq!(moved_to(list.len(), 0, Move::Up), None);
        assert_eq!(moved_to(list.len(), 0, Move::Down), Some(1));
        assert_eq!(moved_to(list.len(), 1, Move::Down), None);
        list.swap(0, 1);
        assert_eq!(list[0].theme, "Dracula");
    }
}
