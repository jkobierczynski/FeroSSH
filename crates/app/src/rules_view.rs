//! Tools > Security rules: what the window lists. One row per rule, with
//! what it matches, its limits and the saved connections it covers, so a
//! rule that matches nothing (or far too much) shows. Pure, so it is
//! tested on any platform; `rules_panel.rs` draws it.

use ferossh_config::host_policy::{self, Policy, SecurityRule, Subject};
use ferossh_config::limit_record;

/// A saved connection, as the rules see it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    pub hostname: Option<String>,
    pub tags: Vec<String>,
    pub folder: Option<String>,
}

/// One rule, as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Where it sits in the settings' list of rules (or the team's).
    pub index: usize,
    /// From the team library: shown, never changed here.
    pub team: bool,
    pub rule: SecurityRule,
    /// The connections it covers, by name, sorted.
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Number,
    Matches,
    Limits,
    Connections,
}

impl Column {
    pub const ALL: [Column; 4] = [
        Column::Number,
        Column::Matches,
        Column::Limits,
        Column::Connections,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Column::Number => "#",
            Column::Matches => "Matches",
            Column::Limits => "Limits",
            Column::Connections => "Connections",
        }
    }

    /// Width in DIPs.
    pub fn width(self) -> i32 {
        match self {
            Column::Number => 56,
            Column::Matches => 260,
            Column::Limits => 330,
            Column::Connections => 320,
        }
    }
}

/// The rows for the team's rules, then `rules`, each with the connections
/// of `all` it covers.
pub fn rows_with_team(
    team: &[SecurityRule],
    rules: &[SecurityRule],
    all: &[Connection],
) -> Vec<Row> {
    let tagged = team
        .iter()
        .enumerate()
        .map(|(i, r)| (i, r, true))
        .chain(rules.iter().enumerate().map(|(i, r)| (i, r, false)));
    tagged
        .map(|(index, rule, team)| {
            let mut used_by: Vec<String> = all
                .iter()
                .filter(|c| {
                    rule.matches(&Subject {
                        name: &c.name,
                        hostname: c.hostname.as_deref(),
                        tags: &c.tags,
                        folder: c.folder.as_deref(),
                    })
                })
                .map(|c| c.name.clone())
                .collect();
            used_by.sort_by_key(|n| n.to_lowercase());
            Row {
                index,
                team,
                rule: rule.clone(),
                used_by,
            }
        })
        .collect()
}

/// Cell `column` of `row`, cleaned of control and direction characters.
pub fn text(row: &Row, column: Column) -> String {
    let raw = match column {
        Column::Number if row.team => format!("team {}", row.index + 1),
        Column::Number => (row.index + 1).to_string(),
        Column::Matches => row.rule.describe_match(),
        Column::Limits => limits_text(&row.rule),
        Column::Connections => match row.used_by.len() {
            0 => "none".to_owned(),
            n if n <= 3 => row.used_by.join(", "),
            n => format!(
                "{n}: {}, ...",
                row.used_by.get(..3).unwrap_or_default().join(", ")
            ),
        },
    };
    clean(&raw)
}

/// A rule's limits in words, with a warning for the lines this FeroSSH
/// does not understand (kept, but not applied).
fn limits_text(rule: &SecurityRule) -> String {
    let mut parts: Vec<String> = rule
        .limits
        .describe()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if !rule.unknown.is_empty() {
        parts.push(format!("⚠ {}", rule.describe_unknown()));
    }
    parts.join(", ")
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

/// The rows a search keeps: every word in what a row matches, limits
/// or connections (any case); `-word` leaves rows with it out; the word
/// `nothing` keeps the rules that cover no connection.
pub fn filter<'a>(rows: &'a [Row], query: &str) -> Vec<&'a Row> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    rows.iter()
        .filter(|r| {
            let hay = [
                r.rule.describe_match(),
                limits_text(&r.rule),
                r.used_by.join(" "),
            ]
            .join(" ")
            .to_lowercase();
            words.iter().all(|w| match w.strip_prefix('-') {
                Some(not) if !not.is_empty() => !hay.contains(not),
                _ if w == "nothing" => r.used_by.is_empty(),
                _ => hay.contains(w.as_str()),
            })
        })
        .collect()
}

/// Sorts `rows` by `column`; the rule's place breaks ties.
pub fn sort(rows: &mut [&Row], column: Column, descending: bool) {
    rows.sort_by(|a, b| {
        let by = match column {
            Column::Number => b.team.cmp(&a.team).then(a.index.cmp(&b.index)),
            Column::Connections => a.used_by.len().cmp(&b.used_by.len()),
            c => text(a, c).to_lowercase().cmp(&text(b, c).to_lowercase()),
        };
        let by = if descending { by.reverse() } else { by };
        by.then(a.index.cmp(&b.index))
    });
}

/// The line under the list.
pub fn summary(shown: &[&Row], total: usize) -> String {
    let unused = shown.iter().filter(|r| r.used_by.is_empty()).count();
    let mut text = match (shown.len(), total) {
        (_, 0) => "No security rules yet. New rule... adds one.".to_owned(),
        (1, 1) => "1 rule.".to_owned(),
        (n, t) if n == t => format!("{n} rules."),
        (n, t) => format!("{n} of {t} rules."),
    };
    if unused > 0 {
        text.push_str(&format!(
            " {unused} cover{} no saved connection yet.",
            if unused == 1 { "s" } else { "" }
        ));
    }
    let odd = shown.iter().filter(|r| !r.rule.unknown.is_empty()).count();
    if odd > 0 {
        text.push_str(&format!(
            " {odd} {} lines this FeroSSH does not understand (marked ⚠): they are kept in the \
             file but not applied; correct them in ferossh.conf or update FeroSSH.",
            if odd == 1 { "rule has" } else { "rules have" }
        ));
    }
    text.push_str(" A rule only tightens: every rule that matches a connection adds its limits.");
    text
}

/// The question before deleting `row`.
pub fn delete_question(row: &Row) -> String {
    let what = clean(&row.rule.describe_match());
    let limits = clean(&row.rule.limits.describe().join(", "));
    let mut text = format!("Delete rule {} ({what}: {limits})?", row.index + 1);
    if !row.used_by.is_empty() {
        let names = row.used_by.iter().map(|n| clean(n)).collect::<Vec<_>>();
        text.push_str(&format!(
            " {} lose{} these limits, unless another rule sets them.",
            crate::bulk::name_list(&names),
            if names.len() == 1 { "s" } else { "" }
        ));
    }
    text
}

/// One connection as it is after a change: `from` is its name before
/// (its old name when renamed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub from: String,
    pub to: Connection,
}

fn subject(c: &Connection) -> Subject<'_> {
    Subject {
        name: &c.name,
        hostname: c.hostname.as_deref(),
        tags: &c.tags,
        folder: c.folder.as_deref(),
    }
}

/// The limits of `c` among `all`, its machine's other connections'
/// included, as a connection gets them.
fn limits_of(rules: &[SecurityRule], c: &Connection, all: &[Connection]) -> Policy {
    let others: Vec<Subject<'_>> = all.iter().map(subject).collect();
    host_policy::policy_with_peers(rules, &subject(c), &others).0
}

/// What each connection loses when `before` (under `rules_before`)
/// becomes `before` with `changes` made (under `rules_after`): the
/// changed connections, and the others on their machines, which share
/// their rules. Named as after the change, in `before`'s order; only
/// the ones that lose something.
pub fn lost_limits(
    rules_before: &[SecurityRule],
    rules_after: &[SecurityRule],
    before: &[Connection],
    changes: &[Changed],
) -> Vec<(String, Policy)> {
    let changed = |name: &str| changes.iter().find(|c| c.from.eq_ignore_ascii_case(name));
    let after: Vec<Connection> = before
        .iter()
        .map(|b| changed(&b.name).map_or_else(|| b.clone(), |c| c.to.clone()))
        .collect();
    let machine = |c: &Connection| c.hostname.clone().unwrap_or_else(|| c.name.clone());
    let machines: Vec<String> = changes
        .iter()
        .filter_map(|c| before.iter().find(|b| b.name.eq_ignore_ascii_case(&c.from)))
        .map(machine)
        .chain(changes.iter().map(|c| machine(&c.to)))
        .collect();
    before
        .iter()
        .zip(&after)
        .filter(|(b, a)| {
            changed(&b.name).is_some()
                || machines.iter().any(|m| {
                    host_policy::same_machine(&machine(b), m)
                        || host_policy::same_machine(&machine(a), m)
                })
        })
        .filter_map(|(b, a)| {
            let lost = limit_record::lost(
                &limits_of(rules_before, b, before),
                &limits_of(rules_after, a, &after),
            );
            (!lost.is_empty()).then(|| (a.name.clone(), lost))
        })
        .collect()
}

/// The question before a change that takes limits away: `what` (`Remove
/// tag production from 3 connections`) and the connections that lose
/// limits, each with them.
pub fn loss_question(what: &str, lost: &[(String, Policy)]) -> String {
    const SHOWN: usize = 15;
    let mut text = format!(
        "{what}? These connections lose security limits, unless they still get them some \
         other way:\n"
    );
    for (name, p) in lost.iter().take(SHOWN) {
        text.push_str(&format!("\n{}: {}", clean(name), p.describe().join(", ")));
    }
    if lost.len() > SHOWN {
        text.push_str(&format!("\n... and {} more", lost.len() - SHOWN));
    }
    text.push_str(
        "\n\nThey connect without those limits from then on (FeroSSH asks once more before \
         the first connection). Go ahead?",
    );
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn conn(name: &str, tags: &[&str], folder: Option<&str>) -> Connection {
        Connection {
            name: name.into(),
            hostname: Some(format!("{name}.example")),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            folder: folder.map(str::to_owned),
        }
    }

    fn rules() -> Vec<SecurityRule> {
        let mut prod = SecurityRule::default();
        prod.tags.push("prod*".into());
        prod.limits.keys_only = true;
        let mut lab = SecurityRule::default();
        lab.folders.push("Lab".into());
        lab.limits.log_session = true;
        let mut none = SecurityRule::default();
        none.hosts.push("*.nowhere".into());
        none.limits.no_clipboard = true;
        vec![prod, lab, none]
    }

    fn all() -> Vec<Connection> {
        vec![
            conn("web", &["production"], None),
            conn("db", &["Prod"], Some("Lab/Old")),
            conn("pi", &[], Some("Lab")),
            conn("home", &[], None),
        ]
    }

    #[test]
    fn rows_list_the_connections_each_rule_covers() {
        let rows = rows_with_team(&[], &rules(), &all());
        assert_eq!(rows[0].used_by, ["db", "web"]);
        assert_eq!(rows[1].used_by, ["db", "pi"]);
        assert!(rows[2].used_by.is_empty());
        assert_eq!(text(&rows[0], Column::Number), "1");
        assert_eq!(text(&rows[0], Column::Matches), "tags prod*");
        assert_eq!(text(&rows[1], Column::Limits), "session logged");
        assert_eq!(text(&rows[2], Column::Connections), "none");
    }

    #[test]
    fn search_and_sort() {
        let rows = rows_with_team(&[], &rules(), &all());
        let hits = filter(&rows, "pi");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 1);
        assert_eq!(filter(&rows, "nothing")[0].index, 2);
        assert_eq!(filter(&rows, "-keys").len(), 2);
        let mut all: Vec<&Row> = rows.iter().collect();
        sort(&mut all, Column::Connections, true);
        assert_eq!(all.iter().map(|r| r.index).collect::<Vec<_>>(), [0, 1, 2]);
        sort(&mut all, Column::Number, true);
        assert_eq!(all[0].index, 2);
        let s = summary(&all, 3);
        assert!(s.starts_with("3 rules. 1 covers no saved connection yet."));
    }

    #[test]
    fn the_delete_question_names_who_loses_the_limits() {
        let rows = rows_with_team(&[], &rules(), &all());
        let q = delete_question(&rows[0]);
        assert!(
            q.starts_with("Delete rule 1 (tags prod*: keys only)?"),
            "{q}"
        );
        assert!(q.contains("db, web lose these limits"), "{q}");
        let mut evil = rows[2].clone();
        evil.rule.hosts = vec!["a\u{202e}b".into()];
        assert!(!delete_question(&evil).contains('\u{202e}'));
    }

    #[test]
    fn team_rules_come_first_marked() {
        let team = [SecurityRule {
            tags: vec!["prod".into()],
            limits: ferossh_config::host_policy::Policy {
                keys_only: true,
                ..Default::default()
            },
            ..Default::default()
        }];
        let rows = rows_with_team(&team, &team, &[]);
        assert!(rows[0].team && !rows[1].team);
        assert_eq!(text(&rows[0], Column::Number), "team 1");
        assert_eq!(text(&rows[1], Column::Number), "1");
        let mut all: Vec<&Row> = rows.iter().rev().collect();
        sort(&mut all, Column::Number, false);
        assert!(all[0].team);
    }

    #[test]
    fn lines_not_understood_show_as_a_warning() {
        let mut rule = SecurityRule::default();
        rule.tags.push("prod".into());
        rule.limits.log_session = true;
        rule.unknown.push("KeysOnly 1".into());
        let all = [conn("db", &["prod"], None)];
        let rows = rows_with_team(&[], &[rule], &all);
        assert_eq!(
            text(&rows[0], Column::Limits),
            "session logged, ⚠ not understood: KeysOnly 1"
        );
        let shown: Vec<&Row> = rows.iter().collect();
        assert!(summary(&shown, 1).contains("1 rule has lines this FeroSSH does not understand"));
        assert_eq!(filter(&rows, "understood").len(), 1);
    }

    #[test]
    fn a_change_names_the_limits_it_takes_away() {
        let all = all();
        let change = |from: &str, to: Connection| Changed {
            from: from.into(),
            to,
        };
        // Tag removed: web loses keys only.
        let lost = lost_limits(
            &rules(),
            &rules(),
            &all,
            &[change("web", conn("web", &[], None))],
        );
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].0, "web");
        assert!(lost[0].1.keys_only && !lost[0].1.log_session);
        // Moved out of Lab, renamed on the way: named as now.
        let lost = lost_limits(
            &rules(),
            &rules(),
            &all,
            &[change("pi", conn("raspberry", &[], None))],
        );
        assert_eq!(lost[0].0, "raspberry");
        assert!(lost[0].1.log_session);
        // Into another folder of the same rule, or a new tag that still
        // matches: nothing lost.
        assert!(
            lost_limits(
                &rules(),
                &rules(),
                &all,
                &[change("db", conn("db", &["prod-eu"], Some("Lab")))]
            )
            .is_empty()
        );
        // A rule gone: everything it covered.
        let lost = lost_limits(&rules(), &rules()[1..], &all, &[]);
        assert!(lost.is_empty(), "only changed connections are compared");
        // Another alias of the same machine keeps the tag: nothing lost.
        let mut twins = all.clone();
        twins.push(Connection {
            name: "web-alias".into(),
            hostname: Some("web.example".into()),
            tags: vec!["production".into()],
            folder: None,
        });
        let mut untagged = conn("web", &[], None);
        untagged.hostname = Some("web.example".into());
        assert!(
            lost_limits(
                &rules(),
                &rules(),
                &twins,
                &[change("web", untagged.clone())]
            )
            .is_empty()
        );
        // The alias's tag goes: both lose it.
        let mut alias_bare = twins[4].clone();
        alias_bare.tags.clear();
        let lost = lost_limits(
            &rules(),
            &rules(),
            &twins,
            &[change("web", untagged), change("web-alias", alias_bare)],
        );
        assert_eq!(lost.len(), 2);
        let q = loss_question("Remove tag production from 2 connections", &lost);
        assert!(q.starts_with("Remove tag production from 2 connections? "));
        assert!(q.contains("\nweb: keys only"));
    }
}
