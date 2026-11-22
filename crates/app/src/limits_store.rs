//! Keeping [`ferossh_config::limit_record`] in `limits.conf` and noting
//! each change the user accepted in `limit-changes.log`, both in
//! FeroSSH's folder for this PC. Platform-neutral, so it is tested on
//! any host.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ferossh_config::host_policy::Policy;
use ferossh_config::limit_record::{self, Limits};

/// The largest `limit-changes.log` kept: past this, it starts again
/// with its newer half.
const MAX_LOG: u64 = 1024 * 1024;

/// One connection on the way, as the security rules see it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    /// Its host name, else its name.
    pub machine: String,
    pub now: Policy,
}

/// A connection with fewer limits than it had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lowered {
    pub check: Check,
    pub lost: Policy,
    /// When it had them, seconds since 1970.
    pub since: Option<u64>,
}

fn dir() -> Option<PathBuf> {
    crate::portable::local_dir()
}

/// `limits.conf` as kept in `dir`: empty when there is none, or when it
/// cannot be read (nothing to compare with is no reason not to connect).
fn read_in(dir: &Path) -> Limits {
    std::fs::read_to_string(dir.join("limits.conf"))
        .map(|t| Limits::parse(&t))
        .unwrap_or_default()
}

/// Which of `checks` lost a limit they had, as kept in `dir`.
pub fn lowered_in(dir: &Path, checks: &[Check]) -> Vec<Lowered> {
    let kept = read_in(dir);
    checks
        .iter()
        .filter_map(|c| {
            let before = kept.before(&c.name, &c.machine);
            let lost = limit_record::lost(&before.limits, &c.now);
            (!lost.is_empty()).then(|| Lowered {
                check: c.clone(),
                lost,
                since: before.when,
            })
        })
        .collect()
}

pub fn lowered(checks: &[Check]) -> Vec<Lowered> {
    dir().map(|d| lowered_in(&d, checks)).unwrap_or_default()
}

/// Changes `limits.conf` in `dir` with `change`, under the connection
/// log's lock, so two windows keep each other's lines.
fn update_in(dir: &Path, change: impl FnOnce(&mut Limits)) -> std::io::Result<()> {
    let _held = crate::inventory_view::lock(dir)?;
    update_locked(dir, change)
}

/// [`update_in`] with the lock already held.
/// The records as they are on disk; the caller holds the lock.
fn read_locked(dir: &Path) -> std::io::Result<Limits> {
    match std::fs::read_to_string(dir.join("limits.conf")) {
        Ok(t) => Ok(Limits::parse(&t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Limits::default()),
        Err(e) => Err(e),
    }
}

fn update_locked(dir: &Path, change: impl FnOnce(&mut Limits)) -> std::io::Result<()> {
    let path = dir.join("limits.conf");
    let mut kept = read_locked(dir)?;
    let before = kept.clone();
    change(&mut kept);
    if kept == before {
        return Ok(());
    }
    crate::inventory_view::write_whole(&path, &kept.to_text())
}

/// Whether keeping `c` now (in `kept`, as it is under the lock) takes
/// away only what `accepted` showed: a limit another window kept since
/// the question was asked, or one nobody was asked about, stays.
fn approved(kept: &Limits, c: &Check, accepted: &[Lowered]) -> bool {
    let lost = limit_record::lost(&kept.before(&c.name, &c.machine).limits, &c.now);
    if lost.is_empty() {
        return true;
    }
    accepted
        .iter()
        .filter(|l| l.check.name.eq_ignore_ascii_case(&c.name))
        .any(|l| limit_record::lost(&lost, &l.lost).is_empty())
}

/// Keeps `checks`' limits as they are now, at `now`; for the ones in
/// `accepted` (fewer than before, connected anyway) a line in
/// `limit-changes.log` too. All under one lock: two windows never
/// interleave their lines, or rotate the log over each other. A
/// connection whose record lost more than `accepted` says is left as it
/// is, so the next connection asks about the rest.
pub fn keep_in(
    dir: &Path,
    checks: &[Check],
    accepted: &[Lowered],
    now: u64,
) -> std::io::Result<()> {
    let _held = crate::inventory_view::lock(dir)?;
    // Nothing is written unless the records, as they are under the lock,
    // lost no more than the Yes was given for, for every connection on
    // the way: another window may have kept more limits since it was
    // asked. The connection is then asked again, and a No to that leaves
    // no line or record for the jump hosts either.
    let on_disk = read_locked(dir)?;
    if checks
        .iter()
        .any(|c| worth_keeping(&on_disk, c) && !approved(&on_disk, c, accepted))
    {
        return Ok(());
    }
    if !accepted.is_empty() {
        let text: String = accepted
            .iter()
            .map(|l| {
                limit_record::log_line(
                    now,
                    &l.check.name,
                    &l.check.machine,
                    &l.lost,
                    "connected anyway",
                )
            })
            .collect();
        append_log(&dir.join("limit-changes.log"), &text)?;
    }
    update_locked(dir, |kept| {
        // Each against the records as the ones before it left them: one
        // alias's line never takes away what another on the same machine
        // just kept.
        for c in checks {
            if worth_keeping(kept, c) && approved(kept, c, accepted) {
                kept.keep(&c.name, &c.machine, c.now, now);
            }
        }
    })
}

/// Whether `lowered` holds no loss that `approved` (the losses a Yes
/// was given for) did not show.
pub fn covered(lowered: &[Lowered], approved: &[Lowered]) -> bool {
    lowered.iter().all(|l| {
        approved
            .iter()
            .filter(|a| a.check.name.eq_ignore_ascii_case(&l.check.name))
            .any(|a| limit_record::lost(&l.lost, &a.lost).is_empty())
    })
}

pub fn keep(checks: &[Check], accepted: &[Lowered], now: u64) -> std::io::Result<()> {
    match dir() {
        Some(d) => keep_in(&d, checks, accepted, now),
        None => Ok(()),
    }
}

/// Whether `c` changes what `kept` holds: other limits than its line
/// has, or limits and no line yet. A connection that never had a limit
/// needs no line, so the file does not list every address typed.
fn worth_keeping(kept: &Limits, c: &Check) -> bool {
    match kept
        .entries()
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(&c.name))
    {
        Some(e) => {
            e.limits != c.now || !ferossh_config::host_policy::same_machine(&e.machine, &c.machine)
        }
        None => !c.now.is_empty() || kept.before(&c.name, &c.machine).when.is_some(),
    }
}

/// Whether keeping `checks` would change anything: saves a write per
/// connection when nothing did.
pub fn changes(checks: &[Check]) -> bool {
    let Some(dir) = dir() else { return false };
    let kept = read_in(&dir);
    checks.iter().any(|c| worth_keeping(&kept, c))
}

/// Connection `old` is called `new` now.
pub fn renamed(old: &str, new: &str) {
    if let Some(d) = dir() {
        let _ = update_in(&d, |kept| kept.rename(old, new));
    }
}

fn append_log(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG) {
        let old = std::fs::read(path)?;
        let half = old.get(old.len() / 2..).unwrap_or_default();
        let start = half.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
        crate::inventory_view::write_whole(
            path,
            &String::from_utf8_lossy(half.get(start..).unwrap_or_default()),
        )?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(text.as_bytes())
}

/// What the question before connecting says about `lowered`.
pub fn describe(lowered: &[Lowered]) -> String {
    lowered
        .iter()
        .map(|l| {
            let since = l
                .since
                .filter(|s| *s > 0)
                .map(|s| format!(" (had them since {} UTC)", limit_record::utc(s)))
                .unwrap_or_default();
            format!(
                "{}: {}{since}",
                crate::ssh::sanitize_line(&l.check.name),
                limit_record::describe(&l.lost)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ferossh-limits-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn check(keys_only: bool) -> Check {
        Check {
            name: "db1".into(),
            machine: "db1.example.com".into(),
            now: Policy {
                keys_only,
                ..Policy::default()
            },
        }
    }

    #[test]
    fn asks_once_then_keeps_the_answer() {
        let dir = temp("once");
        // First connection: nothing to compare with.
        assert!(lowered_in(&dir, &[check(true)]).is_empty());
        keep_in(&dir, &[check(true)], &[], 100).unwrap();
        // The tag went: KeysOnly is lost.
        let low = lowered_in(&dir, &[check(false)]);
        assert_eq!(low.len(), 1);
        assert!(low[0].lost.keys_only);
        assert_eq!(low[0].since, Some(100));
        assert!(describe(&low).contains("db1: keys only (had them since 1970-01-01 00:01 UTC)"));
        // Connected anyway: logged, and not asked again.
        keep_in(&dir, &[check(false)], &low, 200).unwrap();
        assert!(lowered_in(&dir, &[check(false)]).is_empty());
        let log = std::fs::read_to_string(dir.join("limit-changes.log")).unwrap();
        assert!(log.contains("db1\tdb1.example.com\tlost KeysOnly\tconnected anyway"));
        // A Yes for KeysOnly does not take away a limit another window
        // kept meanwhile.
        let both = Check {
            now: Policy {
                keys_only: true,
                log_session: true,
                ..Policy::default()
            },
            ..check(true)
        };
        keep_in(&dir, &[both], &[], 250).unwrap();
        let shown = Lowered {
            check: check(false),
            lost: check(true).now,
            since: None,
        };
        keep_in(&dir, &[check(false)], std::slice::from_ref(&shown), 260).unwrap();
        // ... nor writes a line for it: that connection is asked again.
        let log = std::fs::read_to_string(dir.join("limit-changes.log")).unwrap();
        assert_eq!(log.matches("connected anyway").count(), 1);
        let left = lowered_in(&dir, &[check(false)]);
        assert!(left[0].lost.log_session && left[0].lost.keys_only);
        assert!(!covered(&left, std::slice::from_ref(&shown)));
        assert!(covered(
            std::slice::from_ref(&shown),
            std::slice::from_ref(&shown)
        ));
        // A jump host's Yes is not written while the destination on the
        // same route lost more than was shown.
        let jump = Check {
            name: "jump1".into(),
            machine: "jump1.example.com".into(),
            now: check(true).now,
        };
        keep_in(&dir, std::slice::from_ref(&jump), &[], 270).unwrap();
        let jump_low = Lowered {
            check: Check {
                now: Policy::default(),
                ..jump.clone()
            },
            lost: jump.now,
            since: None,
        };
        keep_in(
            &dir,
            &[jump_low.check.clone(), check(false)],
            &[jump_low.clone(), shown.clone()],
            280,
        )
        .unwrap();
        assert_eq!(
            lowered_in(&dir, std::slice::from_ref(&jump_low.check)).len(),
            1
        );
        let log = std::fs::read_to_string(dir.join("limit-changes.log")).unwrap();
        assert!(!log.contains("jump1"));
        // A connection that never had a limit gets no line.
        let typed = Check {
            name: "10.0.0.9".into(),
            machine: "10.0.0.9".into(),
            now: Policy::default(),
        };
        keep_in(&dir, &[typed], &[], 300).unwrap();
        let text = std::fs::read_to_string(dir.join("limits.conf")).unwrap();
        assert!(!text.contains("10.0.0.9"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_stays_small() {
        let dir = temp("log");
        let path = dir.join("limit-changes.log");
        let line = "x".repeat(99) + "\n";
        std::fs::write(&path, line.repeat((MAX_LOG / 100 + 10) as usize)).unwrap();
        append_log(&path, "new\n").unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(len < MAX_LOG / 2 + 2000);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .ends_with(&format!("{line}new\n"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
