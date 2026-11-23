//! Tools > Connection log: one row per thing that happened to a
//! connection (connected, disconnected, lost, failed, opened), newest
//! first, searched by words, sorted by any column, and saved as CSV.
//! Platform-neutral, so it is tested on any host; the native window is in
//! `connlog_panel.rs`.

use ferossh_config::connlog::{Entry, Kind};

use crate::batch::csv_field;

/// What a column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Time,
    Event,
    Connection,
    Address,
    Type,
    Via,
    HostKey,
    Details,
}

impl Column {
    /// In the list's order.
    pub const ALL: [Self; 8] = [
        Self::Time,
        Self::Event,
        Self::Connection,
        Self::Address,
        Self::Type,
        Self::Via,
        Self::HostKey,
        Self::Details,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Time => "Time",
            Self::Event => "Event",
            Self::Connection => "Connection",
            Self::Address => "Address",
            Self::Type => "Type",
            Self::Via => "Via",
            Self::HostKey => "Host key",
            Self::Details => "Details",
        }
    }

    /// The column's width in the list, in DIPs.
    pub fn width(self) -> i32 {
        match self {
            Self::Time => 130,
            Self::Event => 90,
            Self::Connection => 130,
            Self::Address => 170,
            Self::Type => 55,
            Self::Via => 110,
            Self::HostKey => 200,
            Self::Details => 260,
        }
    }
}

/// The column's text; `when` shows a time.
pub fn text(e: &Entry, column: Column, when: impl Fn(u64) -> String) -> String {
    match column {
        Column::Time => when(e.time),
        Column::Event => e.kind.title().to_owned(),
        Column::Connection => e.name.clone(),
        Column::Address => e.address.clone(),
        Column::Type => e.protocol.clone(),
        Column::Via => e.via.clone(),
        Column::HostKey => e.host_key.clone(),
        Column::Details => e.details.clone(),
    }
}

/// The search's words: plain words must each be in some field, `-word`
/// in none; `"two words"` is one word. Case does not matter.
fn words(query: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut rest = query.trim();
    while !rest.is_empty() {
        let (not, after) = match rest.strip_prefix('-') {
            Some(a) if !a.is_empty() && !a.starts_with(' ') => (true, a),
            _ => (false, rest),
        };
        let (word, after) = if let Some(q) = after.strip_prefix('"') {
            match q.find('"') {
                Some(end) => (&q[..end], &q[end + 1..]),
                None => (q, ""),
            }
        } else {
            match after.find(char::is_whitespace) {
                Some(end) => (&after[..end], &after[end..]),
                None => (after, ""),
            }
        };
        if !word.is_empty() {
            out.push((not, word.to_lowercase()));
        }
        rest = after.trim_start();
    }
    out
}

/// The entries the search keeps; `when` shows a time as the list does,
/// so a date typed as shown finds that day's entries.
pub fn filter<'a>(
    entries: &'a [Entry],
    query: &str,
    when: impl Fn(u64) -> String,
) -> Vec<&'a Entry> {
    let words = words(query);
    entries
        .iter()
        .filter(|e| {
            let all = Column::ALL
                .iter()
                .map(|&c| text(e, c, &when).to_lowercase())
                .collect::<Vec<_>>()
                .join("\t");
            words
                .iter()
                .all(|(not, w)| all.contains(w.as_str()) != *not)
        })
        .collect()
}

/// Sorts `rows` (oldest first, as kept) by `column`; ties newest first,
/// also within one second, except when sorting oldest first by time.
pub fn sort(rows: &mut [&Entry], column: Column, descending: bool) {
    // The sort is stable: rows in newest-first order keep that order on
    // a tie.
    if !(column == Column::Time && !descending) {
        rows.reverse();
    }
    rows.sort_by(|a, b| {
        let o = match column {
            Column::Time => a.time.cmp(&b.time),
            _ => text(a, column, |_| String::new())
                .to_lowercase()
                .cmp(&text(b, column, |_| String::new()).to_lowercase()),
        };
        let o = if descending { o.reverse() } else { o };
        match column {
            Column::Time => o,
            _ => o.then_with(|| b.time.cmp(&a.time)),
        }
    });
}

/// How the window says what it shows: "12 of 40 entries; 3 failed".
pub fn summary(shown: &[&Entry], all: usize, locked: bool) -> String {
    let plural = |n: usize| if n == 1 { "entry" } else { "entries" };
    let mut out = if shown.len() == all {
        format!("{all} {}", plural(all))
    } else {
        format!("{} of {all} {}", shown.len(), plural(all))
    };
    let failed = shown.iter().filter(|e| e.kind == Kind::Failed).count();
    if failed > 0 {
        out.push_str(&format!("; {failed} failed"));
    }
    if locked {
        out.push_str("; older entries are sealed with a vault that is not open");
    }
    out.push('.');
    out
}

/// What a session showed as it ended, as one plain line for the log:
/// colour codes, brackets and the "Press Enter to reconnect." hint go.
pub fn reason(shown: &str) -> String {
    let mut plain = String::new();
    let mut chars = shown.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ ... final byte.
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        plain.push(if c.is_control() { ' ' } else { c });
    }
    let text = plain
        .replace("Press Enter to reconnect.", "")
        .replace(['[', ']'], "");
    ferossh_config::inventory::clean(text.trim())
}

/// The rows as CSV, in their order, times in UTC. Fields a spreadsheet
/// would read as a formula start with `'` (details come from servers).
pub fn csv(rows: &[&Entry]) -> String {
    let mut out = Column::ALL
        .iter()
        .map(|c| c.title())
        .collect::<Vec<_>>()
        .join(",");
    out.push_str("\r\n");
    for e in rows {
        let line: Vec<String> = Column::ALL
            .iter()
            .map(|&c| csv_field(&text(e, c, crate::inventory_view::iso_utc)))
            .collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn entry(time: u64, kind: Kind, name: &str, details: &str) -> Entry {
        Entry {
            time,
            kind,
            protocol: "ssh".into(),
            name: name.into(),
            address: format!("root@{name}.example:22"),
            via: String::new(),
            host_key: "SHA256:abc".into(),
            details: details.into(),
            target: String::new(),
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            entry(1_763_650_000, Kind::Connected, "web", ""),
            entry(1_763_650_100, Kind::Failed, "db", "Connection refused"),
            entry(
                1_763_650_200,
                Kind::Disconnected,
                "web",
                "=cmd|' /c calc'!A1",
            ),
        ]
    }

    #[test]
    fn searches_words_quotes_and_exclusions() {
        let all = sample();
        let names = |q: &str| -> Vec<u64> {
            filter(&all, q, crate::inventory_view::iso_utc)
                .iter()
                .map(|e| e.time)
                .collect()
        };
        assert_eq!(names("web").len(), 2);
        assert_eq!(names("failed"), [1_763_650_100]);
        assert_eq!(names("web -disconnected"), [1_763_650_000]);
        assert_eq!(names("\"connection refused\""), [1_763_650_100]);
        assert_eq!(names("").len(), 3);
        assert!(names("nothing").is_empty());
    }

    #[test]
    fn sorts_newest_first_by_default_and_by_any_column() {
        let all = sample();
        let mut shown = filter(&all, "", crate::inventory_view::iso_utc);
        sort(&mut shown, Column::Time, true);
        assert_eq!(shown[0].kind, Kind::Disconnected);
        sort(&mut shown, Column::Connection, false);
        assert_eq!(shown[0].name, "db");
        // Ties: newest first.
        assert_eq!(shown[1].kind, Kind::Disconnected);
    }

    #[test]
    fn csv_has_a_header_utc_times_and_no_formulas() {
        let all = sample();
        let shown = filter(&all, "", crate::inventory_view::iso_utc);
        let text = csv(&shown);
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(
            lines[0],
            "Time,Event,Connection,Address,Type,Via,Host key,Details"
        );
        assert_eq!(
            lines[1],
            "2025-11-20T14:46:40Z,Connected,web,root@web.example:22,ssh,,SHA256:abc,"
        );
        assert!(lines[3].ends_with(",'=cmd|' /c calc'!A1"), "{}", lines[3]);
    }

    #[test]
    fn finds_by_the_time_shown() {
        let all = sample();
        let found = filter(&all, "2025-11-20T14:46:40", crate::inventory_view::iso_utc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, Kind::Connected);
    }

    #[test]
    fn same_second_newest_first() {
        let all = vec![
            entry(5, Kind::Connected, "web", ""),
            entry(5, Kind::Disconnected, "web", ""),
        ];
        let mut shown = filter(&all, "", crate::inventory_view::iso_utc);
        sort(&mut shown, Column::Time, true);
        assert_eq!(shown[0].kind, Kind::Disconnected);
        let mut shown = filter(&all, "", crate::inventory_view::iso_utc);
        sort(&mut shown, Column::Time, false);
        assert_eq!(shown[0].kind, Kind::Connected);
        let mut shown = filter(&all, "", crate::inventory_view::iso_utc);
        sort(&mut shown, Column::Connection, false);
        assert_eq!(shown[0].kind, Kind::Disconnected);
    }

    #[test]
    fn reasons_are_plain() {
        assert_eq!(
            reason("\r\n\x1b[0;90m[Connection refused. Press Enter to reconnect.]\x1b[0m\r\n"),
            "Connection refused."
        );
        assert_eq!(reason("\x1b[0;90m\x1b[0m"), "");
    }

    #[test]
    fn summary_counts() {
        let all = sample();
        let shown = filter(&all, "", crate::inventory_view::iso_utc);
        assert_eq!(summary(&shown, 3, false), "3 entries; 1 failed.");
        let one = filter(&all, "web -disconnected", crate::inventory_view::iso_utc);
        assert_eq!(summary(&one, 3, false), "1 of 3 entries.");
        assert_eq!(
            summary(&[], 0, true),
            "0 entries; older entries are sealed with a vault that is not open."
        );
    }
}
