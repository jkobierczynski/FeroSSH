//! The connection manager's Inventory: a row per connection with what
//! FeroSSH learnt when it last logged in (when, the host key, and with
//! `SystemInfo yes` the server's OS and kernel), searched with the
//! connection manager's search words, sorted by any column, and saved as
//! CSV. Platform-neutral, so it is tested on any host; the native window
//! is in `inventory_panel.rs`.

use ferossh_config::Settings;
use ferossh_config::host_query::Query;
use ferossh_config::inventory::{Inventory, Record};

use crate::batch::csv_field;
use crate::sessions::HostItem;

/// `inventory.conf` in FeroSSH's folder for this PC.
fn store_path() -> Option<std::path::PathBuf> {
    Some(crate::portable::local_dir()?.join("inventory.conf"))
}

/// The inventory as kept; empty when there is none yet, or when it cannot
/// be read (for showing only: [`update`] never writes over such a file).
pub fn load() -> Inventory {
    read().unwrap_or_default()
}

/// The inventory as kept: empty when there is no file yet, an error when
/// there is one that cannot be read.
fn read() -> std::io::Result<Inventory> {
    let Some(path) = store_path() else {
        return Ok(Inventory::default());
    };
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Inventory::parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Inventory::default()),
        Err(e) => Err(e),
    }
}

/// Held while the inventory is read, changed and written: an exclusive
/// open of `inventory.lock`, which every FeroSSH process takes the same
/// way, so two windows' changes do not interleave. Released when dropped.
struct Lock(#[allow(dead_code)] std::fs::File);

fn lock(dir: &std::path::Path) -> std::io::Result<Lock> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("inventory.lock");
    let mut last = None;
    // Another window holds it for a few milliseconds at most.
    for _ in 0..100 {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        match options.open(&path) {
            Ok(f) => return Ok(Lock(f)),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("the inventory is busy")))
}

/// Writes `text` through a temporary file of this write's own beside
/// `path`, then moves it over: a whole file or none.
fn write_whole(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let (tmp, mut file) = loop {
        let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{}-{n}.tmp", std::process::id()));
        let tmp = std::path::PathBuf::from(tmp);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    let written = written.and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Reads the file again, changes it and writes it back, all under one
/// lock, so two FeroSSH windows each keep what the other wrote. A file
/// that cannot be read is left as it is, and the error comes back.
pub fn update(change: impl FnOnce(&mut Inventory)) -> std::io::Result<()> {
    let Some(path) = store_path() else {
        return Ok(());
    };
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    let _held = lock(dir)?;
    let mut inv = read()?;
    let before = inv.clone();
    change(&mut inv);
    if inv == before {
        return Ok(());
    }
    write_whole(&path, &inv.to_text())
}

/// Writes `text` to a new file `stem.ext` in `dir`, or `stem-2.ext` and
/// on when that is taken: an earlier export is never written over.
pub fn write_new(
    dir: &std::path::Path,
    stem: &str,
    ext: &str,
    text: &str,
) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    for n in 1..=100 {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => return f.write_all(text.as_bytes()).map(|()| path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "a hundred exports of this second are there already",
    ))
}

/// Seconds since 1970, now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// One connection's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub host: HostItem,
    pub record: Record,
    /// `SystemInfo yes`: the OS and kernel are read after login.
    pub system_info: bool,
}

/// What a column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Name,
    Address,
    Type,
    Folder,
    Tags,
    LastConnected,
    KeyType,
    Fingerprint,
    Os,
    Kernel,
}

impl Column {
    /// In the list's order.
    pub const ALL: [Self; 10] = [
        Self::Name,
        Self::Address,
        Self::Type,
        Self::Folder,
        Self::Tags,
        Self::LastConnected,
        Self::KeyType,
        Self::Fingerprint,
        Self::Os,
        Self::Kernel,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Address => "Address",
            Self::Type => "Type",
            Self::Folder => "Folder",
            Self::Tags => "Tags",
            Self::LastConnected => "Last connected",
            Self::KeyType => "Host key",
            Self::Fingerprint => "Fingerprint",
            Self::Os => "OS",
            Self::Kernel => "Kernel",
        }
    }

    /// The column's width in the list, in DIPs.
    pub fn width(self) -> i32 {
        match self {
            Self::Name => 140,
            Self::Address => 170,
            Self::Type => 60,
            Self::Folder => 100,
            Self::Tags => 110,
            Self::LastConnected => 120,
            Self::KeyType => 80,
            Self::Fingerprint => 200,
            Self::Os => 190,
            Self::Kernel => 160,
        }
    }
}

/// Every listed connection with what the inventory holds on it.
pub fn rows(hosts: &[HostItem], inventory: &Inventory, settings: &Settings) -> Vec<Row> {
    hosts
        .iter()
        .map(|h| Row {
            host: h.clone(),
            record: inventory.get(&h.name).cloned().unwrap_or_default(),
            system_info: settings.profile(&h.name).is_some_and(|p| p.system_info),
        })
        .collect()
}

/// The rows a connection manager search (`tag:prod -os`, plain words)
/// keeps; each plain word also looks at the OS, kernel and fingerprint,
/// and a field word (`folder:db`) only at its field.
pub fn filter<'a>(rows: &'a [Row], query: &str) -> Vec<&'a Row> {
    let q = Query::parse(query);
    rows.iter()
        .filter(|r| {
            let more = [
                r.record.os.as_str(),
                r.record.kernel.as_str(),
                r.record.fingerprint.as_str(),
            ];
            q.matches_with(&r.host.facts(), &more)
        })
        .collect()
}

/// Sorts by `column`, newest first for the last login; ties by name.
pub fn sort(rows: &mut [&Row], column: Column, descending: bool) {
    let key = |r: &Row| -> String { text(r, column, |_| String::new()) };
    rows.sort_by(|a, b| {
        let o = match column {
            Column::LastConnected => a.record.last_connected.cmp(&b.record.last_connected),
            _ => key(a).to_lowercase().cmp(&key(b).to_lowercase()),
        };
        let o = if descending { o.reverse() } else { o };
        o.then_with(|| a.host.name.to_lowercase().cmp(&b.host.name.to_lowercase()))
    });
}

/// The column's text; `when` shows a time.
pub fn text(r: &Row, column: Column, when: impl Fn(u64) -> String) -> String {
    match column {
        Column::Name => r.host.name.clone(),
        Column::Address => r.host.detail.clone(),
        Column::Type => r.host.kind.name().to_owned(),
        Column::Folder => r.host.folder.clone().unwrap_or_default(),
        Column::Tags => r.host.tags.join(" "),
        Column::LastConnected => r.record.last_connected.map(when).unwrap_or_default(),
        Column::KeyType => r.record.key_type.clone(),
        Column::Fingerprint => r.record.fingerprint.clone(),
        Column::Os => r.record.os.clone(),
        Column::Kernel => r.record.kernel.clone(),
    }
}

/// How the window says what it shows: "12 of 40 connections; 3 never
/// connected".
pub fn summary(shown: &[&Row], all: usize) -> String {
    // Only SSH logins are recorded: a Telnet or WinRM row is never
    // counted as not connected to.
    let never = shown
        .iter()
        .filter(|r| {
            r.host.kind == ferossh_config::SessionKind::Ssh && r.record.last_connected.is_none()
        })
        .count();
    let what = if shown.len() == all {
        format!("{all} connection{}", if all == 1 { "" } else { "s" })
    } else {
        format!("{} of {all} connections", shown.len())
    };
    let off = shown
        .iter()
        .filter(|r| r.host.kind == ferossh_config::SessionKind::Ssh && !r.system_info)
        .count();
    let mut out = what;
    if never > 0 {
        out.push_str(&format!("; {never} not connected to yet"));
    }
    if off > 0 {
        out.push_str(&format!(
            "; OS and kernel are read for SSH connections with Inventory on in their form ({off} \
             off)"
        ));
    }
    out.push('.');
    out
}

/// `secs` since 1970 as UTC, `2026-11-20T18:04:05Z`, which spreadsheets
/// and scripts read alike.
pub fn iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// The rows as CSV, in their order, times in UTC. Fields a spreadsheet
/// would read as a formula start with `'` (the OS and kernel come from
/// the servers).
pub fn csv(rows: &[&Row]) -> String {
    let mut out = Column::ALL
        .iter()
        .map(|c| c.title())
        .collect::<Vec<_>>()
        .join(",");
    out.push_str("\r\n");
    for r in rows {
        let line: Vec<String> = Column::ALL
            .iter()
            .map(|&c| csv_field(&text(r, c, iso_utc)))
            .collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use ferossh_config::SessionKind;
    use ferossh_config::inventory::SystemInfo;

    use super::*;

    fn host(name: &str, folder: Option<&str>, tags: &[&str]) -> HostItem {
        HostItem {
            name: name.into(),
            folder: folder.map(Into::into),
            detail: format!("root@{name}.example"),
            kind: SessionKind::Ssh,
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            user: Some("root".into()),
            hostname: Some(format!("{name}.example")),
            port: Some(22),
        }
    }

    fn sample() -> Vec<Row> {
        let hosts = [
            host("web", Some("prod"), &["prod", "web"]),
            host("db", Some("prod"), &["prod"]),
            host("lab", None, &[]),
        ];
        let mut inv = Inventory::default();
        inv.connected("web", 1_763_650_000, Some(("ED25519", "SHA256:aaa")));
        inv.connected("db", 1_700_000_000, Some(("RSA", "SHA256:bbb")));
        inv.system(
            "db",
            1_700_000_001,
            &SystemInfo {
                os: "=cmd|' /c calc'!A1".into(),
                kernel: "Linux 6.1".into(),
            },
        );
        let settings = Settings::parse("Profile db\n    SystemInfo yes\n");
        rows(&hosts, &inv, &settings)
    }

    #[test]
    fn rows_carry_the_inventory_and_the_switch() {
        let rows = sample();
        assert_eq!(rows[0].record.fingerprint, "SHA256:aaa");
        assert!(rows[1].system_info);
        assert!(!rows[0].system_info);
        assert_eq!(rows[2].record, Record::default());
    }

    #[test]
    fn searches_with_field_words_and_server_text() {
        let rows = sample();
        let names = |q: &str| -> Vec<String> {
            filter(&rows, q)
                .iter()
                .map(|r| r.host.name.clone())
                .collect()
        };
        assert_eq!(names("tag:prod -tag:web"), ["db"]);
        assert_eq!(names("linux"), ["db"]);
        assert_eq!(names("sha256:aaa"), ["web"]);
        assert_eq!(names(""), ["web", "db", "lab"]);
        assert_eq!(names("folder:nothing"), Vec::<String>::new());
        // Field and plain words together, each word on its own.
        assert_eq!(names("tag:prod linux"), ["db"]);
        assert_eq!(names("tag:prod -linux"), ["web"]);
        assert_eq!(names("\"linux 6\""), ["db"]);
        // A field word looks only at its field, not at the server's text.
        assert_eq!(names("folder:linux"), Vec::<String>::new());
    }

    #[test]
    fn exports_never_write_over_an_earlier_one() {
        let dir = std::env::temp_dir().join(format!("ferossh-inv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = write_new(&dir, "inv", "csv", "a").unwrap();
        let b = write_new(&dir, "inv", "csv", "b").unwrap();
        assert_eq!(a.file_name().unwrap(), "inv.csv");
        assert_eq!(b.file_name().unwrap(), "inv-2.csv");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sorts_by_any_column() {
        let rows = sample();
        let mut shown = filter(&rows, "");
        sort(&mut shown, Column::LastConnected, true);
        let order: Vec<&str> = shown.iter().map(|r| r.host.name.as_str()).collect();
        assert_eq!(order, ["web", "db", "lab"]);
        sort(&mut shown, Column::Name, false);
        let order: Vec<&str> = shown.iter().map(|r| r.host.name.as_str()).collect();
        assert_eq!(order, ["db", "lab", "web"]);
        sort(&mut shown, Column::KeyType, false);
        assert_eq!(shown[0].host.name, "lab", "empty first");
    }

    #[test]
    fn csv_has_a_header_utc_times_and_no_formulas() {
        let rows = sample();
        let shown = filter(&rows, "");
        let text = csv(&shown);
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(
            lines[0],
            "Name,Address,Type,Folder,Tags,Last connected,Host key,Fingerprint,OS,Kernel"
        );
        assert_eq!(
            lines[1],
            "web,root@web.example,ssh,prod,prod web,2025-11-20T14:46:40Z,ED25519,SHA256:aaa,,"
        );
        assert!(lines[2].contains(",'=cmd|' /c calc'!A1,"), "{}", lines[2]);
        assert_eq!(lines[3], "lab,root@lab.example,ssh,,,,,,,");
    }

    #[test]
    fn utc_dates() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_763_650_000), "2025-11-20T14:46:40Z");
    }

    #[test]
    fn summary_says_what_is_missing() {
        let rows = sample();
        let shown = filter(&rows, "");
        assert_eq!(
            summary(&shown, 3),
            "3 connections; 1 not connected to yet; OS and kernel are read for SSH connections \
             with Inventory on in their form (2 off)."
        );
        let one = filter(&rows, "db");
        assert_eq!(summary(&one, 3), "1 of 3 connections.");
        // Telnet logins are not recorded: never counted as not connected.
        let mut telnet = rows[2].clone();
        telnet.host.kind = SessionKind::Telnet;
        assert_eq!(summary(&[&telnet], 1), "1 connection.");
    }
}
