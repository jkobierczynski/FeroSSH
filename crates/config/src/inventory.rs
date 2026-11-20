//! The connection inventory: what FeroSSH learnt about each connection
//! when it last logged in. The connection manager's Inventory window
//! lists it and exports it as CSV.
//!
//! Kept per PC in `inventory.conf` (`%LOCALAPPDATA%\FeroSSH`), one line
//! per connection, fields apart by tabs:
//!
//! ```text
//! # FeroSSH inventory
//! web01\t1763650000\tED25519\tSHA256:abc...\tDebian GNU/Linux 12\tLinux 6.1.0\t1763650003
//! ```
//!
//! The name, when it last logged in (seconds since 1970), the host key's
//! type and SHA-256 fingerprint, and, for a connection with
//! `SystemInfo yes`, the server's OS and kernel and when they were read.
//! The OS and kernel come from the server, which is not trusted: they
//! are read from a fixed command's output ([`SYSTEM_COMMAND`]), cut short
//! and stripped of control characters before they are kept.

use std::fmt::Write as _;

/// The longest field kept, in characters.
const MAX_FIELD: usize = 200;
/// Most connections kept; the ones not logged in to longest go first.
const MAX_RECORDS: usize = 10_000;
/// The most of the system command's output read.
pub const MAX_SYSTEM_OUTPUT: usize = 16 * 1024;

/// What is known about one connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// Seconds since 1970 of the last login.
    pub last_connected: Option<u64>,
    /// `ED25519`, `RSA`, ... as `ssh-keygen -l` names it.
    pub key_type: String,
    /// `SHA256:...`.
    pub fingerprint: String,
    /// `Debian GNU/Linux 12 (bookworm)`.
    pub os: String,
    /// `Linux 6.1.0-18-amd64`.
    pub kernel: String,
    /// Seconds since 1970 of when the OS and kernel were read.
    pub system_read: Option<u64>,
}

/// Every connection's [`Record`], by name (case does not matter).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    records: Vec<(String, Record)>,
}

/// `text` fit to keep: control characters (tabs and line ends among them)
/// become spaces, runs of spaces one, and it is cut at [`MAX_FIELD`].
pub fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        let c = if c.is_control() || is_bidi_control(c) {
            ' '
        } else {
            c
        };
        if c == ' ' {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        if out.chars().count() >= MAX_FIELD {
            break;
        }
        out.push(c);
    }
    out
}

/// Characters that reorder the text around them, which a server could use
/// to make a value read as something else.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{61c}'
    )
}

fn number(text: &str) -> Option<u64> {
    text.parse().ok().filter(|n| *n > 0)
}

impl Inventory {
    /// Reads `inventory.conf`; lines it does not understand are skipped.
    pub fn parse(text: &str) -> Self {
        let mut inv = Self::default();
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut f = line.split('\t');
            let name = clean(f.next().unwrap_or_default());
            if name.is_empty() || inv.get(&name).is_some() {
                continue;
            }
            let mut next = || clean(f.next().unwrap_or_default());
            let record = Record {
                last_connected: number(&next()),
                key_type: next(),
                fingerprint: next(),
                os: next(),
                kernel: next(),
                system_read: number(&next()),
            };
            inv.records.push((name, record));
        }
        inv.trim();
        inv
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut out = String::from("# FeroSSH inventory\n");
        let n = |v: Option<u64>| v.map(|v| v.to_string()).unwrap_or_default();
        for (name, r) in &self.records {
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                clean(name),
                n(r.last_connected),
                clean(&r.key_type),
                clean(&r.fingerprint),
                clean(&r.os),
                clean(&r.kernel),
                n(r.system_read),
            );
        }
        out
    }

    pub fn get(&self, name: &str) -> Option<&Record> {
        self.records
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, r)| r)
    }

    /// The record of `name`, added when there is none; `None` for a name
    /// with nothing left once cleaned.
    fn entry(&mut self, name: &str) -> Option<&mut Record> {
        let name = clean(name);
        if name.is_empty() {
            return None;
        }
        if self.get(&name).is_none() {
            self.records.push((name.clone(), Record::default()));
        }
        self.records
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(&name))
            .map(|(_, r)| r)
    }

    /// Connection `name` logged in at `now`, with this host key (`(type,
    /// fingerprint)`); `None` for a login without one (Telnet, WinRM).
    pub fn connected(&mut self, name: &str, now: u64, key: Option<(&str, &str)>) {
        let Some(r) = self.entry(name) else {
            return;
        };
        r.last_connected = Some(now);
        if let Some((kind, fp)) = key {
            r.key_type = clean(kind);
            r.fingerprint = clean(fp);
        }
        self.trim();
    }

    /// The server of connection `name` said this at `now`.
    pub fn system(&mut self, name: &str, now: u64, info: &SystemInfo) {
        let Some(r) = self.entry(name) else {
            return;
        };
        r.os = clean(&info.os);
        r.kernel = clean(&info.kernel);
        r.system_read = Some(now);
        self.trim();
    }

    /// Connection `old` is called `new` now.
    pub fn rename(&mut self, old: &str, new: &str) {
        if old.eq_ignore_ascii_case(new) || clean(new).is_empty() {
            return;
        }
        self.remove(new);
        if let Some((n, _)) = self
            .records
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(old))
        {
            *n = clean(new);
        }
    }

    /// Forgets connection `name`.
    pub fn remove(&mut self, name: &str) {
        self.records.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    /// Keeps the file to [`MAX_RECORDS`] lines.
    fn trim(&mut self) {
        if self.records.len() <= MAX_RECORDS {
            return;
        }
        self.records
            .sort_by_key(|(_, r)| std::cmp::Reverse(r.last_connected.unwrap_or(0)));
        self.records.truncate(MAX_RECORDS);
    }
}

/// What the server said it runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemInfo {
    pub os: String,
    pub kernel: String,
}

/// The line between the kernel and the OS in [`SYSTEM_COMMAND`]'s output.
const MARK: &str = "@ferossh-os";

/// The read-only command that prints the server's kernel and OS: `uname
/// -sr`, then `/etc/os-release` (Linux, the BSDs) or `sw_vers` (macOS).
/// `sh -c`, so it runs the same under any login shell that has single
/// quotes; a server without `sh` (Windows) prints no mark, which
/// [`parse_system`] takes as nothing known.
pub const SYSTEM_COMMAND: &str = "sh -c 'uname -sr 2>/dev/null; echo @ferossh-os; \
     cat /etc/os-release 2>/dev/null || sw_vers 2>/dev/null'";

/// The kernel and OS in [`SYSTEM_COMMAND`]'s output; `None` when the mark
/// is missing (the command did not run).
pub fn parse_system(output: &[u8]) -> Option<SystemInfo> {
    let output = output.get(..MAX_SYSTEM_OUTPUT).unwrap_or(output);
    let text = String::from_utf8_lossy(output);
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    let mut kernel = String::new();
    for line in lines.by_ref() {
        if line.trim() == MARK {
            break;
        }
        if kernel.is_empty() {
            kernel = clean(line);
        }
    }
    // No mark: the whole output went by.
    if !text
        .lines()
        .any(|l| l.trim_end_matches('\r').trim() == MARK)
    {
        return None;
    }
    let (mut pretty, mut name, mut version) = (None, None, None);
    let (mut product, mut product_version) = (None, None);
    for line in lines {
        if let Some((key, value)) = line.split_once('=') {
            let value = unquote(value.trim());
            match key.trim() {
                "PRETTY_NAME" => pretty = Some(value),
                "NAME" => name = Some(value),
                "VERSION_ID" => version = Some(value),
                _ => {}
            }
        } else if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "ProductName" => product = Some(value.trim().to_owned()),
                "ProductVersion" => product_version = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    let join = |a: Option<String>, b: Option<String>| match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, _) => a,
    };
    let os = pretty
        .filter(|p| !p.trim().is_empty())
        .or_else(|| join(name, version))
        .or_else(|| join(product, product_version))
        // Only `uname`: its system name.
        .or_else(|| kernel.split(' ').next().map(str::to_owned))
        .unwrap_or_default();
    Some(SystemInfo {
        os: clean(&os),
        kernel,
    })
}

/// An os-release value without its quotes and escapes.
fn unquote(value: &str) -> String {
    let inner = ['"', '\'']
        .iter()
        .find_map(|q| {
            value
                .strip_prefix(*q)
                .and_then(|v| v.strip_suffix(*q))
                .map(|v| (v, *q == '"'))
        })
        .unwrap_or((value, false));
    if !inner.1 {
        return inner.0.to_owned();
    }
    let mut out = String::new();
    let mut chars = inner.0.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_skips_what_it_does_not_understand() {
        let mut inv = Inventory::default();
        inv.connected("web01", 1_763_650_000, Some(("ED25519", "SHA256:abc")));
        inv.system(
            "WEB01",
            1_763_650_003,
            &SystemInfo {
                os: "Debian GNU/Linux 12 (bookworm)".into(),
                kernel: "Linux 6.1.0-18-amd64".into(),
            },
        );
        inv.connected("telnet-box", 5, None);
        let text = inv.to_text();
        let back = Inventory::parse(&format!("{text}\n\t1\t2\nweb01\tdup\n"));
        assert_eq!(back, inv);
        let r = back.get("Web01").unwrap();
        assert_eq!(r.last_connected, Some(1_763_650_000));
        assert_eq!(r.fingerprint, "SHA256:abc");
        assert_eq!(r.kernel, "Linux 6.1.0-18-amd64");
        assert_eq!(r.system_read, Some(1_763_650_003));
        assert!(back.get("telnet-box").unwrap().key_type.is_empty());
    }

    #[test]
    fn a_login_without_a_host_key_keeps_the_last_one() {
        let mut inv = Inventory::default();
        inv.connected("a", 1, Some(("RSA", "SHA256:x")));
        inv.connected("a", 2, None);
        let r = inv.get("a").unwrap();
        assert_eq!((r.last_connected, r.key_type.as_str()), (Some(2), "RSA"));
    }

    #[test]
    fn rename_and_remove_follow_the_connection() {
        let mut inv = Inventory::default();
        inv.connected("old", 1, None);
        inv.connected("new", 2, None);
        inv.rename("old", "new");
        assert_eq!(inv.get("new").unwrap().last_connected, Some(1));
        assert!(inv.get("old").is_none());
        inv.remove("NEW");
        assert!(inv.get("new").is_none());
    }

    #[test]
    fn server_text_cannot_break_a_line_or_run_long() {
        let mut inv = Inventory::default();
        let info = SystemInfo {
            os: format!("Evil\tOS\nnext\u{202e}line{}", "x".repeat(1000)),
            kernel: "\x1b]0;title\x07Linux".into(),
        };
        inv.system("a", 1, &info);
        let text = inv.to_text();
        assert_eq!(text.lines().count(), 2);
        let r = Inventory::parse(&text).get("a").unwrap().clone();
        assert!(r.os.starts_with("Evil OS next line"));
        assert_eq!(r.os.chars().count(), MAX_FIELD);
        assert!(!r.kernel.contains('\x1b'));
    }

    #[test]
    fn keeps_the_most_recent_connections() {
        let mut inv = Inventory::default();
        for i in 0..=MAX_RECORDS as u64 {
            inv.connected(&format!("h{i}"), i + 1, None);
        }
        assert!(inv.get("h0").is_none());
        assert!(inv.get(&format!("h{MAX_RECORDS}")).is_some());
    }

    #[test]
    fn reads_linux_os_release() {
        let out = b"Linux 6.1.0-18-amd64\n@ferossh-os\nNAME=\"Debian GNU/Linux\"\n\
                    VERSION_ID=\"12\"\nPRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n";
        let i = parse_system(out).unwrap();
        assert_eq!(i.kernel, "Linux 6.1.0-18-amd64");
        assert_eq!(i.os, "Debian GNU/Linux 12 (bookworm)");
        let out = b"Linux 5.4\r\n@ferossh-os\r\nNAME='Alpine \\\"x'\r\nVERSION_ID=3.19\r\n";
        assert_eq!(parse_system(out).unwrap().os, "Alpine \\\"x 3.19");
        let out = b"Linux 5.4\n@ferossh-os\nNAME=\"A \\\"B\\\"\"\n";
        assert_eq!(parse_system(out).unwrap().os, "A \"B\"");
    }

    #[test]
    fn reads_macos_and_bare_uname() {
        let out = b"Darwin 23.1.0\n@ferossh-os\nProductName:\tmacOS\nProductVersion:\t14.1\n\
                    BuildVersion:\t23B74\n";
        let i = parse_system(out).unwrap();
        assert_eq!(
            (i.os.as_str(), i.kernel.as_str()),
            ("macOS 14.1", "Darwin 23.1.0")
        );
        let i = parse_system(b"OpenBSD 7.4\n@ferossh-os\n").unwrap();
        assert_eq!(i.os, "OpenBSD");
    }

    #[test]
    fn no_mark_means_nothing_known() {
        assert_eq!(
            parse_system(b"'sh' is not recognized as an internal command\r\n"),
            None
        );
        assert_eq!(parse_system(b""), None);
    }
}
