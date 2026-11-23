//! The connection log: one entry for each thing that happened to a
//! connection, as it happened. Connected, disconnected, lost (and
//! reconnecting), or failed, with when, to what, through which jump
//! hosts, with which host key, and why it ended.
//!
//! Kept per PC in `connections.log` (`%LOCALAPPDATA%\FeroSSH`, or sealed
//! with the vault while one is open), one line per entry, oldest first,
//! fields apart by tabs:
//!
//! ```text
//! # FeroSSH connection log
//! 1763650000\tconnected\tssh\tweb01\troot@web01.example:22\tbastion\tSHA256:abc...\t
//! 1763650900\tdisconnected\tssh\tweb01\troot@web01.example:22\tbastion\tSHA256:abc...\t15 min. The shell exited.
//! ```
//!
//! The details may come from the server (a disconnect reason), which is
//! not trusted: every field is cleaned as the inventory's are
//! ([`crate::inventory::clean`]) before it is kept.

use std::fmt::Write as _;

use crate::inventory::clean;

/// Most entries kept; the oldest go first.
pub const MAX_ENTRIES: usize = 20_000;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The login went through (a shell, tunnels or file transfer opened).
    Connected,
    /// A connection that was up ended: the user closed it, the shell
    /// exited, or the server closed it.
    Disconnected,
    /// A connection that was up dropped; FeroSSH tries again.
    Lost,
    /// No login: the host could not be reached, refused, or the login
    /// was turned down or given up.
    Failed,
    /// A desktop or remote program was started directly (RDP, VNC).
    Opened,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Connected,
        Kind::Disconnected,
        Kind::Lost,
        Kind::Failed,
        Kind::Opened,
    ];

    /// The word in the file.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Connected => "connected",
            Kind::Disconnected => "disconnected",
            Kind::Lost => "lost",
            Kind::Failed => "failed",
            Kind::Opened => "opened",
        }
    }

    /// The word on screen.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Connected => "Connected",
            Kind::Disconnected => "Disconnected",
            Kind::Lost => "Lost",
            Kind::Failed => "Failed",
            Kind::Opened => "Opened",
        }
    }

    fn parse(word: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.word() == word)
    }
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Seconds since 1970.
    pub time: u64,
    pub kind: Kind,
    /// `ssh`, `telnet`, `winrm`, `rdp`, `vnc`...
    pub protocol: String,
    /// The connection's name (or what was typed at the prompt).
    pub name: String,
    /// `user@host:port` as connected to.
    pub address: String,
    /// The jump hosts gone through, first first, `a > b`; empty when
    /// none.
    pub via: String,
    /// The host key's SHA-256 fingerprint, when there is one.
    pub host_key: String,
    /// Why it ended, how long it was up.
    pub details: String,
    /// What to open to connect again, as typed (`user@host:port` or the
    /// connection's name); empty when `name` is that. A ninth field,
    /// written only when set, so older lines read as before.
    pub target: String,
}

impl Entry {
    /// The entry with every field cleaned, as it is kept.
    pub fn cleaned(&self) -> Entry {
        Entry {
            time: self.time,
            kind: self.kind,
            protocol: clean(&self.protocol),
            name: clean(&self.name),
            address: clean(&self.address),
            via: clean(&self.via),
            host_key: clean(&self.host_key),
            details: clean(&self.details),
            target: clean(&self.target),
        }
    }

    /// The entry's line in the file (no line end).
    pub fn line(&self) -> String {
        let e = self.cleaned();
        let mut out = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            e.time,
            e.kind.word(),
            e.protocol,
            e.name,
            e.address,
            e.via,
            e.host_key,
            e.details
        );
        if !e.target.is_empty() {
            out.push('\t');
            out.push_str(&e.target);
        }
        out
    }

    fn parse(line: &str) -> Option<Entry> {
        let mut f = line.split('\t');
        let time = f.next()?.trim().parse().ok().filter(|t| *t > 0)?;
        let kind = Kind::parse(f.next()?.trim())?;
        let mut next = || clean(f.next().unwrap_or_default());
        Some(Entry {
            time,
            kind,
            protocol: next(),
            name: next(),
            address: next(),
            via: next(),
            host_key: next(),
            details: next(),
            target: next(),
        })
    }
}

/// Every entry, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Log {
    entries: Vec<Entry>,
}

impl Log {
    /// Reads `connections.log`; lines it does not understand are
    /// skipped. Entries come out oldest first whatever order the text had
    /// them in (a sealed part and a plain part read together).
    pub fn parse(text: &str) -> Self {
        let mut entries: Vec<Entry> = text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .filter_map(Entry::parse)
            .collect();
        // Stable: entries of the same second keep their order.
        entries.sort_by_key(|e| e.time);
        let mut log = Log { entries };
        log.trim();
        log
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut out = String::from("# FeroSSH connection log\n");
        for e in &self.entries {
            let _ = writeln!(out, "{}", e.line());
        }
        out
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds `entry` (cleaned) at its time.
    pub fn push(&mut self, entry: &Entry) {
        let e = entry.cleaned();
        let at = self.entries.partition_point(|x| x.time <= e.time);
        self.entries.insert(at, e);
        self.trim();
    }

    /// Removes the entries equal to those in `gone` (one each), and
    /// says how many went.
    pub fn remove(&mut self, gone: &[Entry]) -> usize {
        let mut n = 0;
        for g in gone {
            let g = g.cleaned();
            if let Some(i) = self.entries.iter().position(|e| *e == g) {
                self.entries.remove(i);
                n += 1;
            }
        }
        n
    }

    /// Keeps the file to [`MAX_ENTRIES`], the newest.
    fn trim(&mut self) {
        if self.entries.len() > MAX_ENTRIES {
            let extra = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..extra);
        }
    }
}

/// `secs` as a length of time: `45 s`, `12 min`, `3 h 05 min`, `2 d 4 h`.
pub fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => format!("{} h {:02} min", secs / 3600, secs / 60 % 60),
        _ => format!("{} d {} h", secs / 86_400, secs / 3600 % 24),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn entry(time: u64, kind: Kind, name: &str) -> Entry {
        Entry {
            time,
            kind,
            protocol: "ssh".into(),
            name: name.into(),
            address: format!("root@{name}.example:22"),
            via: String::new(),
            host_key: "SHA256:abc".into(),
            details: String::new(),
            target: String::new(),
        }
    }

    #[test]
    fn round_trips_and_sorts_by_time() {
        let mut log = Log::default();
        log.push(&entry(20, Kind::Disconnected, "web"));
        log.push(&entry(10, Kind::Connected, "web"));
        log.push(&Entry {
            via: "bastion > inner".into(),
            details: "Connection refused".into(),
            ..entry(15, Kind::Failed, "db")
        });
        let text = log.to_text();
        assert!(text.starts_with("# FeroSSH connection log\n"));
        let back = Log::parse(&text);
        assert_eq!(back, log);
        let kinds: Vec<Kind> = back.entries().iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [Kind::Connected, Kind::Failed, Kind::Disconnected]);
        assert_eq!(back.entries()[1].via, "bastion > inner");
    }

    #[test]
    fn the_target_is_a_ninth_field_only_when_set() {
        let plain = entry(10, Kind::Connected, "web");
        assert_eq!(plain.line().split('\t').count(), 8);
        let typed = Entry {
            target: "root@10.0.0.5:2222".into(),
            ..entry(11, Kind::Connected, "10.0.0.5")
        };
        assert_eq!(typed.line().split('\t').count(), 9);
        let mut log = Log::default();
        log.push(&plain);
        log.push(&typed);
        let back = Log::parse(&log.to_text());
        assert_eq!(back.entries()[0].target, "");
        assert_eq!(back.entries()[1].target, "root@10.0.0.5:2222");
    }

    #[test]
    fn server_text_is_cleaned() {
        let mut log = Log::default();
        log.push(&Entry {
            details: "bye\tforged\nline\u{202e}".into(),
            ..entry(1, Kind::Disconnected, "web")
        });
        let text = log.to_text();
        assert_eq!(text.lines().count(), 2, "{text}");
        let back = Log::parse(&text);
        assert_eq!(back.entries()[0].details, "bye forged line");
    }

    #[test]
    fn skips_what_it_does_not_understand() {
        let log = Log::parse(
            "# x\n\nnot a line\n0\tconnected\tssh\tzero\n5\tteleported\tssh\tx\n7\tlost\ttelnet\tsw1\n",
        );
        assert_eq!(log.len(), 1);
        assert_eq!(log.entries()[0].kind, Kind::Lost);
        assert_eq!(log.entries()[0].name, "sw1");
        assert_eq!(log.entries()[0].address, "");
    }

    #[test]
    fn removes_only_what_was_picked() {
        let mut log = Log::default();
        let a = entry(1, Kind::Connected, "web");
        let b = entry(2, Kind::Disconnected, "web");
        log.push(&a);
        log.push(&b);
        log.push(&a);
        assert_eq!(log.remove(&[a.clone(), entry(9, Kind::Lost, "x")]), 1);
        assert_eq!(log.len(), 2);
        assert_eq!(log.entries()[0], a);
        assert_eq!(log.entries()[1], b);
    }

    #[test]
    fn keeps_the_newest() {
        let mut text = String::new();
        for t in 1..=(MAX_ENTRIES as u64 + 5) {
            text.push_str(&entry(t, Kind::Connected, "h").line());
            text.push('\n');
        }
        let log = Log::parse(&text);
        assert_eq!(log.len(), MAX_ENTRIES);
        assert_eq!(log.entries()[0].time, 6);
    }

    #[test]
    fn durations() {
        assert_eq!(duration(45), "45 s");
        assert_eq!(duration(12 * 60 + 5), "12 min");
        assert_eq!(duration(3 * 3600 + 5 * 60), "3 h 05 min");
        assert_eq!(duration(2 * 86_400 + 4 * 3600), "2 d 4 h");
    }
}
