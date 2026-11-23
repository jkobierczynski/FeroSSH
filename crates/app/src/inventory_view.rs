//! The inventory: what FeroSSH learnt about each connection when it last
//! logged in (when, the host key, and with `SystemInfo yes` the server's
//! OS and kernel). Tools > Hosts shows it by machine (`hosts_view.rs`);
//! Tools > Connection log is the log of every connection
//! (`connlog_view.rs`). Kept with [`crate::local_store`].

use ferossh_config::Settings;
use ferossh_config::inventory::{Inventory, Record};

use crate::sessions::HostItem;

/// The inventory's file in FeroSSH's folder for this PC (see
/// [`crate::local_store`]).
pub const FILE: &str = "inventory.conf";

/// The inventory as kept: empty when there is no file yet, an error when
/// there is one that cannot be read ([`update`] never writes over such a
/// file either). A part sealed with a vault that is not open is left out.
pub fn read() -> std::io::Result<Inventory> {
    Ok(Inventory::parse(&crate::local_store::read(FILE)?.text))
}

/// Held while the inventory is read, changed and written: an exclusive
/// open of `inventory.lock`, which every FeroSSH process takes the same
/// way, so two windows' changes do not interleave. Released when dropped.
pub(crate) struct Lock(#[allow(dead_code)] std::fs::File);

pub(crate) fn lock(dir: &std::path::Path) -> std::io::Result<Lock> {
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
    Err(last.unwrap_or_else(|| std::io::Error::other("the connection log is busy")))
}

/// Writes `text` through a temporary file of this write's own beside
/// `path`, then moves it over: a whole file or none.
pub(crate) fn write_whole(path: &std::path::Path, text: &str) -> std::io::Result<()> {
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
    let Some(dir) = crate::portable::local_dir() else {
        return Ok(());
    };
    let _held = lock(&dir)?;
    let was = crate::local_store::read(FILE)?;
    let mut inv = Inventory::parse(&was.text);
    let before = inv.clone();
    change(&mut inv);
    if inv == before {
        return Ok(());
    }
    crate::local_store::write(FILE, &was, &inv.to_text())
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
    fn utc_dates() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_763_650_000), "2025-11-20T14:46:40Z");
    }
}
