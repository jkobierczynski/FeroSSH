//! The limits the security rules gave each connection when it last
//! connected, so FeroSSH notices when they get fewer.
//!
//! Rules match connections by tag, folder and name, and nothing stops a
//! change to those (in FeroSSH, by hand in the files, or by an import),
//! a rule being removed or the team library not loading. Any of these
//! can take a limit away from a connection without anyone choosing that.
//! Before connecting, FeroSSH compares the limits it would apply now
//! with the ones kept here, and asks when one is gone.
//!
//! Kept per PC in `limits.conf` (`%LOCALAPPDATA%\FeroSSH`), one line per
//! connection, fields apart by tabs: its name, the machine it reaches,
//! the limits as their rule keywords, and when it last connected:
//!
//! ```text
//! # FeroSSH limits
//! db1\tdb1.example.com\tKeysOnly LogSession\t1763650000
//! ```
//!
//! A connection is compared with its own line and with the line of every
//! other connection to the same machine, because the rules follow the
//! machine: a new alias or a typed address does not start from nothing.
//!
//! This file is not protected: deleting it, or a line of it, makes the
//! next connection start a new record without asking. It is there to
//! catch the changes nobody meant, not to stop someone who means them.

use std::fmt::Write as _;

use crate::host_policy::{Policy, same_machine};

/// Most connections kept; the ones not connected to longest go first.
const MAX_RECORDS: usize = 10_000;
/// The longest name or machine kept, in characters.
const MAX_FIELD: usize = 255;

/// Each limit's keyword in a security rule, in the order of [`flags`].
const LIMITS: [&str; 6] = [
    "KeysOnly",
    "SavePassword",
    "ForwardAgent",
    "Clipboard",
    "ConfirmConnect",
    "LogSession",
];

fn flags(p: &Policy) -> [bool; 6] {
    [
        p.keys_only,
        p.no_saved_password,
        p.no_agent_forwarding,
        p.no_clipboard,
        p.confirm_connect,
        p.log_session,
    ]
}

fn from_flags(f: [bool; 6]) -> Policy {
    let [
        keys_only,
        no_saved_password,
        no_agent_forwarding,
        no_clipboard,
        confirm_connect,
        log_session,
    ] = f;
    Policy {
        keys_only,
        no_saved_password,
        no_agent_forwarding,
        no_clipboard,
        confirm_connect,
        log_session,
    }
}

/// The limits in `before` that `now` does not have.
pub fn lost(before: &Policy, now: &Policy) -> Policy {
    let mut out = flags(before);
    for (b, n) in out.iter_mut().zip(flags(now)) {
        *b &= !n;
    }
    from_flags(out)
}

/// `p`'s limits as their keywords, apart by spaces.
pub fn keywords(p: &Policy) -> String {
    LIMITS
        .iter()
        .zip(flags(p))
        .filter(|(_, on)| *on)
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(" ")
}

/// `p`'s limits read back from [`keywords`]; words it does not know are
/// left out.
pub fn parse_keywords(text: &str) -> Policy {
    let words: Vec<&str> = text.split_whitespace().collect();
    from_flags(std::array::from_fn(|i| {
        LIMITS
            .get(i)
            .is_some_and(|k| words.iter().any(|w| w.eq_ignore_ascii_case(k)))
    }))
}

/// What was kept for one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Its host name, else its name.
    pub machine: String,
    pub limits: Policy,
    /// Seconds since 1970 of when these limits were kept.
    pub when: u64,
}

/// What a connection had before: the limits of its own line and its
/// machine's other lines together, and the newest time among them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Before {
    pub limits: Policy,
    pub when: Option<u64>,
}

/// Every connection's [`Entry`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Limits {
    entries: Vec<Entry>,
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_FIELD)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl Limits {
    /// Reads `limits.conf`; lines it does not understand are skipped.
    pub fn parse(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut f = line.split('\t');
            let name = clean(f.next().unwrap_or_default());
            if name.is_empty()
                || out
                    .entries
                    .iter()
                    .any(|e| e.name.eq_ignore_ascii_case(&name))
            {
                continue;
            }
            let machine = clean(f.next().unwrap_or_default());
            let limits = parse_keywords(f.next().unwrap_or_default());
            let when = f.next().and_then(|w| w.trim().parse().ok()).unwrap_or(0);
            out.entries.push(Entry {
                machine: if machine.is_empty() {
                    name.clone()
                } else {
                    machine
                },
                name,
                limits,
                when,
            });
        }
        out
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut out = String::from("# FeroSSH limits\n");
        for e in &self.entries {
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{}",
                clean(&e.name),
                clean(&e.machine),
                keywords(&e.limits),
                e.when
            );
        }
        out
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn concerns(e: &Entry, name: &str, machine: &str) -> bool {
        e.name.eq_ignore_ascii_case(name) || same_machine(&e.machine, machine)
    }

    /// What connection `name` to `machine` had when it, or another
    /// connection to the same machine, last connected.
    pub fn before(&self, name: &str, machine: &str) -> Before {
        let mut out = Before::default();
        for e in self
            .entries
            .iter()
            .filter(|e| Self::concerns(e, name, machine))
        {
            out.limits.tighten(&e.limits);
            out.when = Some(out.when.map_or(e.when, |w| w.max(e.when)));
        }
        out
    }

    /// Keeps `limits` for connection `name` to `machine` at `now`, and
    /// for the machine's other lines too: what was answered for one of
    /// them holds for the machine.
    pub fn keep(&mut self, name: &str, machine: &str, limits: Policy, now: u64) {
        let machine = if machine.trim().is_empty() {
            name
        } else {
            machine
        };
        for e in self
            .entries
            .iter_mut()
            .filter(|e| same_machine(&e.machine, machine) && !e.name.eq_ignore_ascii_case(name))
        {
            e.limits = limits;
            e.when = now;
        }
        self.entries.retain(|e| !e.name.eq_ignore_ascii_case(name));
        self.entries.push(Entry {
            name: clean(name),
            machine: clean(machine),
            limits,
            when: now,
        });
        if self.entries.len() > MAX_RECORDS {
            self.entries.sort_by_key(|e| std::cmp::Reverse(e.when));
            self.entries.truncate(MAX_RECORDS);
        }
    }

    /// Connection `old` is called `new` now: its line follows.
    pub fn rename(&mut self, old: &str, new: &str) {
        if old.eq_ignore_ascii_case(new) {
            if let Some(e) = self
                .entries
                .iter_mut()
                .find(|e| e.name.eq_ignore_ascii_case(old))
            {
                e.name = clean(new);
            }
            return;
        }
        let Some(i) = self
            .entries
            .iter()
            .position(|e| e.name.eq_ignore_ascii_case(old))
        else {
            return;
        };
        let mut moved = self.entries.remove(i);
        // With no host name of its own, the name was the machine.
        if moved.machine.eq_ignore_ascii_case(&moved.name) {
            moved.machine = clean(new);
        }
        moved.name = clean(new);
        // A line already under the new name keeps what both had.
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|e| e.name.eq_ignore_ascii_case(new))
        {
            e.limits.tighten(&moved.limits);
            e.when = e.when.max(moved.when);
        } else {
            self.entries.push(moved);
        }
    }
}

/// `p`'s limits as FeroSSH shows them: `keys only, session logged`.
pub fn describe(p: &Policy) -> String {
    p.describe().join(", ")
}

/// One line of `limit-changes.log`: when (UTC, `YYYY-MM-DD HH:MM`), the
/// connection, the machine, the limits it lost and what was answered.
pub fn log_line(now: u64, name: &str, machine: &str, lost: &Policy, answer: &str) -> String {
    format!(
        "{}\t{}\t{}\tlost {}\t{}\n",
        utc(now),
        clean(name),
        clean(machine),
        keywords(lost),
        clean(answer)
    )
}

/// `secs` since 1970 as `YYYY-MM-DD HH:MM` UTC.
pub fn utc(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// Days since 1970-01-01 as a date (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn p(keys_only: bool, log_session: bool) -> Policy {
        Policy {
            keys_only,
            log_session,
            ..Policy::default()
        }
    }

    #[test]
    fn round_trips() {
        let mut l = Limits::default();
        l.keep("db1", "db1.example.com", p(true, true), 1_763_650_000);
        l.keep("web", "", Policy::default(), 5);
        let text = l.to_text();
        assert!(text.contains("db1\tdb1.example.com\tKeysOnly LogSession\t1763650000"));
        assert_eq!(Limits::parse(&text), l);
        assert_eq!(l.entries()[1].machine, "web");
    }

    #[test]
    fn notices_a_limit_gone() {
        let mut l = Limits::default();
        l.keep("db1", "db1.example.com", p(true, true), 10);
        let before = l.before("db1", "db1.example.com");
        assert_eq!(before.when, Some(10));
        assert_eq!(lost(&before.limits, &p(true, false)), p(false, true));
        assert!(lost(&before.limits, &p(true, true)).is_empty());
        // More limits than before lose nothing.
        assert!(
            lost(
                &before.limits,
                &Policy {
                    confirm_connect: true,
                    ..p(true, true)
                }
            )
            .is_empty()
        );
        // Unknown connection: nothing to lose.
        assert!(l.before("other", "other.example.com").limits.is_empty());
    }

    #[test]
    fn follows_the_machine() {
        let mut l = Limits::default();
        l.keep("db1", "DB1.example.com.", p(true, false), 10);
        // A typed address or another alias for the same machine.
        assert!(
            l.before("db1.example.com", "db1.example.com")
                .limits
                .keys_only
        );
        assert!(l.before("db-alias", "db1.example.com").limits.keys_only);
        // Accepting fewer limits for one of them does for the machine.
        l.keep("db-alias", "db1.example.com", Policy::default(), 20);
        assert!(l.before("db1", "db1.example.com").limits.is_empty());
        // Loopback is never one machine.
        l.keep("a", "localhost", p(true, false), 1);
        assert!(l.before("b", "localhost").limits.is_empty());
    }

    #[test]
    fn a_rename_keeps_the_record() {
        let mut l = Limits::default();
        l.keep("db1", "", p(true, false), 10);
        l.rename("db1", "db-one");
        assert!(l.before("db-one", "db-one").limits.keys_only);
        assert!(l.before("db1", "db1").limits.is_empty());
        // Onto a name with its own line: both limits kept.
        l.keep("web", "", p(false, true), 3);
        l.rename("web", "db-one");
        assert_eq!(l.before("db-one", "db-one").limits, p(true, true));
        assert_eq!(l.entries().len(), 1);
    }

    #[test]
    fn hand_edits_cannot_break_lines() {
        let l = Limits::parse(
            "# x\nweird\tm\tKeysOnly Bogus keysonly\tnope\n\tno name\nweird\tdup\t\t\n",
        );
        assert_eq!(l.entries().len(), 1);
        assert!(l.entries()[0].limits.keys_only);
        assert_eq!(l.entries()[0].when, 0);
        let mut l = Limits::default();
        l.keep("a\tb\nc", "h", p(true, false), 1);
        assert_eq!(Limits::parse(&l.to_text()).entries().len(), 1);
    }

    #[test]
    fn keywords_cover_every_limit() {
        let all = Policy {
            keys_only: true,
            no_saved_password: true,
            no_agent_forwarding: true,
            no_clipboard: true,
            confirm_connect: true,
            log_session: true,
        };
        assert_eq!(parse_keywords(&keywords(&all)), all);
        assert_eq!(describe(&all).split(", ").count(), 6);
    }

    #[test]
    fn logs_a_change() {
        assert_eq!(utc(0), "1970-01-01 00:00");
        assert_eq!(utc(1_763_836_200), "2025-11-22 18:30");
        let line = log_line(0, "db1", "db1.example.com", &p(true, false), "connected");
        assert_eq!(
            line,
            "1970-01-01 00:00\tdb1\tdb1.example.com\tlost KeysOnly\tconnected\n"
        );
    }
}
