//! Reading the team library (see [`ferossh_config::team`]) from its share,
//! with a checked copy kept in FeroSSH's folder: a laptop away from the
//! share keeps the team's connections and rules, and a share that serves
//! a file with a bad signature does not take them away either.
//!
//! The copy is checked again each time it is read, against the keys set
//! now, so changing the key drops a copy signed by the old one. Each
//! library path has its own copy, so setting another library never uses
//! the copy of the one before.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use ferossh_config::team::{self, MAX_LEN, Team};

/// What the window loaded last, for code that reads the settings file
/// itself (`ssh_env`).
static CURRENT: Mutex<Option<Team>> = Mutex::new(None);

/// The team library in use; empty when there is none.
pub fn current() -> Team {
    CURRENT
        .lock()
        .ok()
        .and_then(|t| t.clone())
        .unwrap_or_default()
}

pub fn set_current(team: Team) {
    if let Ok(mut t) = CURRENT.lock() {
        *t = Some(team);
    }
}

/// The newest library this run accepted, per library path and the keys
/// it was checked against: the rollback check's baseline even when its
/// copy could not be written ([`keep`]).
static ACCEPTED: Mutex<Vec<(String, Vec<String>, Team)>> = Mutex::new(Vec::new());

/// The newest library accepted for `library` under `keys`: this run's,
/// or the checked copy's, whichever is newer.
fn newest_seen(library: &str, keys: &[String], cache_dir: &Path) -> Option<Team> {
    let run = ACCEPTED.lock().ok().and_then(|a| {
        a.iter()
            .find(|(l, k, _)| l == library.trim() && k == keys)
            .map(|(.., t)| t.clone())
    });
    let copy = from_cache(library, keys, cache_dir);
    match (run, copy) {
        (Some(r), Some(c)) => Some(if c.serial > r.serial { c } else { r }),
        (r, c) => r.or(c),
    }
}

/// Notes `team` as accepted for `library` under `keys`, unless a newer
/// one already is.
fn accepted(library: &str, keys: &[String], team: &Team) {
    let Ok(mut a) = ACCEPTED.lock() else {
        return;
    };
    match a
        .iter_mut()
        .find(|(l, k, _)| l == library.trim() && k == keys)
    {
        Some((.., t)) if t.serial <= team.serial => *t = team.clone(),
        Some(_) => {}
        None => a.push((library.trim().to_owned(), keys.to_vec(), team.clone())),
    }
}

/// The serial for a new export at `now` (seconds since 1970): past the
/// library this PC uses (`current`) and past the last export from this
/// PC, kept in `dir`, so every export is newer than the one before even
/// when the clock is behind or two exports fall in the same second.
pub fn next_serial(dir: Option<&Path>, now: u64, current: u64) -> u64 {
    let file = dir.map(|d| d.join("team-export-serial"));
    let last = file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| t.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let serial = now
        .max(current.saturating_add(1))
        .max(last.saturating_add(1));
    if let (Some(d), Some(f)) = (dir, file) {
        let _ = std::fs::create_dir_all(d);
        let _ = std::fs::write(f, format!("{serial}\n"));
    }
    serial
}

/// The signature file beside `library`: its name with `.sig` added, as
/// `ssh-keygen -Y sign` writes it.
pub fn sig_path(library: &Path) -> PathBuf {
    let mut name = OsString::from(library.as_os_str());
    name.push(".sig");
    PathBuf::from(name)
}

/// The largest `.sig` file read: an SSH signature is a few hundred bytes.
const MAX_SIG_LEN: usize = 64 * 1024;

/// Held from the check that a read is still the newest to its copy's
/// rename, and while a newer read is announced ([`newest`]), so the two
/// never cross.
static COMMIT: Mutex<()> = Mutex::new(());

/// Makes read `n` the newest in `latest`: an older read that has not
/// put its copy in place by now never will.
pub fn newest(latest: &AtomicU64, n: u64) {
    let _held = COMMIT.lock();
    latest.store(n, Ordering::Release);
}

/// The checked copy of library `library` in `dir`: one file holding the
/// library's path on the first line, the signature's length on the
/// second, the signature, then the library, so the two are always
/// replaced together. The name carries a hash of the exact path.
fn cache_path(dir: &Path, library: &str) -> PathBuf {
    // FNV-1a: stable across builds, unlike the standard hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in library.trim().bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    dir.join(format!("team-library-{hash:016x}.cache"))
}

/// A file read whole, refused when larger than `max` bytes; the limit
/// holds while it is read, so a file that grows meanwhile is refused too.
fn read_capped(path: &Path, max: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(std::io::Error::other(format!(
            "larger than {} KB",
            max / 1024
        )));
    }
    Ok(bytes)
}

/// A `.sig` file, read with its own limit.
fn read_sig(path: &Path) -> std::io::Result<String> {
    String::from_utf8(read_capped(path, MAX_SIG_LEN)?)
        .map_err(|_| std::io::Error::other("not text"))
}

/// Keeps library `bytes` and its signature `sig` as the checked copy
/// for `library`: written beside it, then moved over the old one, so a
/// failed write leaves the old copy as it was.
fn keep(
    cache_dir: &Path,
    library: &str,
    bytes: &[u8],
    sig: &str,
    current: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    /// Numbers each write, so two never share a temporary file.
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let file = cache_path(cache_dir, library);
    // The library's path first, checked on reading: two paths whose
    // hashes meet never share a copy.
    let mut record = format!("{}\n{}\n{sig}", library.trim(), sig.len()).into_bytes();
    record.extend_from_slice(bytes);
    let tmp = write_new(&file, &record, WRITES.fetch_add(1, Ordering::Relaxed))?;
    let _held = COMMIT.lock();
    let done = if current() {
        std::fs::rename(&tmp, &file)
    } else {
        Ok(())
    };
    // Still there when not moved: an older read's, or a failed rename.
    let _ = std::fs::remove_file(&tmp);
    done
}

/// Writes `bytes` to a new temporary file beside `file`, created
/// exclusively (never through a file or link already there), and
/// returns its path; removed again when writing fails.
pub fn write_new(file: &Path, bytes: &[u8], n: u64) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".{}-{n}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .inspect_err(|_| {
            drop(std::fs::remove_file(&tmp));
        })?;
    Ok(tmp)
}

/// The copy in `cache_dir`, if it is still good under `keys`: what a
/// window starts with, before the share is read.
pub fn cached(library: Option<&str>, keys: &[String], cache_dir: &Path) -> Team {
    match library.map(str::trim).filter(|l| !l.is_empty()) {
        Some(library) => from_cache(library, keys, cache_dir).unwrap_or_default(),
        None => Team::default(),
    }
}

/// The copy of `library` in `cache_dir`, if it is still good under `keys`.
fn from_cache(library: &str, keys: &[String], cache_dir: &Path) -> Option<Team> {
    let record = read_capped(
        &cache_path(cache_dir, library),
        MAX_LEN + MAX_SIG_LEN + 32 + library.len(),
    )
    .ok()?;
    let (path, rest) = record.split_at(record.iter().position(|b| *b == b'\n')?);
    if path != library.trim().as_bytes() {
        return None;
    }
    let rest = rest.get(1..)?;
    let (len, rest) = rest.split_at(rest.iter().position(|b| *b == b'\n')?);
    let len: usize = std::str::from_utf8(len).ok()?.parse().ok()?;
    if len > MAX_SIG_LEN {
        return None;
    }
    let rest = rest.get(1..)?;
    let sig = std::str::from_utf8(rest.get(..len)?).ok()?;
    let bytes = rest.get(len..)?;
    team::load(bytes, sig, &team::keys(keys)).ok()
}

/// What reading the library gave: the team (empty for none) and, when
/// something is wrong, what to tell the user.
#[derive(Debug, Default)]
pub struct Loaded {
    pub team: Team,
    pub note: Option<String>,
}

/// Reads library `library` (its `.sig` beside it), checks it against
/// `keys` and keeps a copy in `cache_dir` while `current` (this read is
/// still the newest); on any failure, the copy checked last.
pub fn load(
    library: Option<&str>,
    keys: &[String],
    cache_dir: &Path,
    current: &dyn Fn() -> bool,
) -> Loaded {
    let Some(library) = library.map(str::trim).filter(|l| !l.is_empty()) else {
        return Loaded::default();
    };
    let path = PathBuf::from(library);
    let shown = crate::ssh::sanitize_line(library);
    let read = read_capped(&path, MAX_LEN).and_then(|b| Ok((b, read_sig(&sig_path(&path))?)));
    let why = match read {
        Ok((bytes, sig)) => match team::load(&bytes, &sig, &team::keys(keys)) {
            // Signed, but older than the copy checked last: the share was
            // turned back to an earlier library (one from before a rule
            // was added, say), which only someone who cannot sign a new
            // one has reason to do. The newer copy stays in use.
            Ok(team) => {
                // Signed, but older than the copy checked last: the share
                // was turned back to an earlier library (one from before a
                // rule was added, say), which only someone who cannot sign
                // a new one has reason to do. The newer copy stays in use.
                if let Some(kept) = newest_seen(library, keys, cache_dir)
                    && kept.serial > team.serial
                {
                    let note = format!(
                        "The team library {shown} was not used: it is older than the copy \
                         FeroSSH checked last (exported {}, the copy {}), so someone may have \
                         put an earlier library back on the share. FeroSSH uses the newer copy; \
                         ask your administrator to export and sign the library again.",
                        exported(team.serial),
                        exported(kept.serial),
                    );
                    return Loaded {
                        team: kept,
                        note: Some(note),
                    };
                }
                // The baseline for the next read, even when the copy below
                // cannot be written (which otherwise only costs the
                // offline fallback).
                accepted(library, keys, &team);
                let _ = keep(cache_dir, library, &bytes, &sig, current);
                return Loaded { team, note: None };
            }
            Err(why) => format!("was not used: {why}"),
        },
        Err(e) => format!("could not be read ({e})"),
    };
    match from_cache(library, keys, cache_dir) {
        Some(team) => Loaded {
            team,
            note: Some(format!(
                "The team library {shown} {why}. FeroSSH uses the copy it checked last."
            )),
        },
        None => Loaded {
            team: Team::default(),
            note: Some(format!(
                "The team library {shown} {why}. Its connections and rules are not available."
            )),
        },
    }
}

/// When a library with serial `serial` was exported, for a note.
fn exported(serial: u64) -> String {
    if serial == 0 {
        "before FeroSSH dated its libraries".to_owned()
    } else {
        format!("{} UTC", ferossh_config::limit_record::utc(serial))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ferossh_config::Settings;
    use russh::keys::HashAlg;
    use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData, PrivateKey};
    use russh::keys::ssh_key::{LineEnding, SshSig};

    fn admin() -> PrivateKey {
        let pair = Ed25519Keypair::from_seed(&[7; 32]);
        PrivateKey::new(KeypairData::Ed25519(pair), "admin").unwrap()
    }

    fn write_library(dir: &Path, host: &str) -> PathBuf {
        write_dated(dir, host, 1)
    }

    fn write_dated(dir: &Path, host: &str, serial: u64) -> PathBuf {
        let bytes = team::export(&format!("Host {host}\n"), &Settings::default(), &[], serial);
        let sig = SshSig::sign(&admin(), team::NAMESPACE, HashAlg::Sha512, &bytes)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap();
        let path = dir.join("team.ferossh-library");
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(sig_path(&path), sig).unwrap();
        path
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ferossh-team-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn falls_back_to_the_checked_copy() {
        let share = temp("share");
        let cache = temp("cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let lib = write_library(&share, "web");
        let path = lib.to_str().unwrap();
        let first = load(Some(path), &keys, &cache, &|| true);
        assert!(first.note.is_none());
        assert!(first.team.has("web"));
        // Checked, so a policy that requires a library connects.
        assert!(first.team.verified);
        // Tampered on the share: refused, the copy used.
        std::fs::write(&lib, b"FeroSSH connections v1\nend\n").unwrap();
        let tampered = load(Some(path), &keys, &cache, &|| true);
        assert!(tampered.team.has("web"));
        assert!(tampered.note.unwrap().contains("signature does not match"));
        // Gone from the share: the copy.
        std::fs::remove_file(&lib).unwrap();
        let offline = load(Some(path), &keys, &cache, &|| true);
        assert!(offline.team.has("web"));
        assert!(offline.note.unwrap().contains("could not be read"));
        assert!(offline.team.verified, "the checked copy counts");
        // Another key: the copy no longer passes either.
        let other = load(
            Some(path),
            &["ssh-ed25519 AAAA junk".into()],
            &cache,
            &|| true,
        );
        assert!(other.team.is_empty());
        // Nothing good to use: a policy that requires one connects nowhere.
        assert!(!other.team.verified);
        // Another library, not there: not the first one's copy.
        let elsewhere = share.join("other.ferossh-library");
        let moved = load(Some(elsewhere.to_str().unwrap()), &keys, &cache, &|| true);
        assert!(moved.team.is_empty());
        assert!(cached(Some(elsewhere.to_str().unwrap()), &keys, &cache).is_empty());
        assert!(cached(Some(path), &keys, &cache).has("web"));
        // A path that differs only in case is another file on a
        // case-sensitive folder: not this copy either.
        let upper = path.replace("team.ferossh-library", "TEAM.ferossh-library");
        assert!(cached(Some(&upper), &keys, &cache).is_empty());
        // A read no longer the newest uses what it read but leaves the
        // copy as it is.
        let latest = AtomicU64::new(0);
        newest(&latest, 2);
        let read_one = || latest.load(Ordering::Acquire) == 1;
        write_library(&share, "db");
        assert!(load(Some(path), &keys, &cache, &read_one).team.has("db"));
        assert!(cached(Some(path), &keys, &cache).has("web"));
        // No library set: nothing, and nothing to say.
        let off = load(None, &keys, &cache, &|| true);
        assert!(off.team.is_empty() && off.note.is_none());
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn an_older_signed_library_does_not_replace_a_newer_copy() {
        let share = temp("rollback");
        let cache = temp("rollback-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let lib = write_dated(&share, "web", 200);
        let path = lib.to_str().unwrap();
        assert!(load(Some(path), &keys, &cache, &|| true).team.has("web"));
        // An earlier library, signed by the same key, put back.
        write_dated(&share, "old", 100);
        let back = load(Some(path), &keys, &cache, &|| true);
        assert!(back.team.has("web") && !back.team.has("old"));
        assert!(back.note.unwrap().contains("older than the copy"));
        assert!(cached(Some(path), &keys, &cache).has("web"));
        // One from before libraries were dated is older too.
        write_dated(&share, "undated", 0);
        assert!(load(Some(path), &keys, &cache, &|| true).team.has("web"));
        // A newer one whose copy could not be written still sets the
        // baseline: one between it and the copy is refused.
        let unwritable = cache.join("not-a-folder");
        std::fs::write(&unwritable, b"").unwrap();
        write_dated(&share, "newest", 250);
        assert!(
            load(Some(path), &keys, &unwritable, &|| true)
                .team
                .has("newest")
        );
        write_dated(&share, "between", 220);
        let between = load(Some(path), &keys, &unwritable, &|| true);
        assert!(between.team.has("newest") && between.note.is_some());
        // The same export read again, and a newer one, are used.
        write_dated(&share, "web2", 250);
        assert!(load(Some(path), &keys, &cache, &|| true).note.is_none());
        write_dated(&share, "db", 300);
        let newer = load(Some(path), &keys, &cache, &|| true);
        assert!(newer.team.has("db") && newer.note.is_none());
        assert!(cached(Some(path), &keys, &cache).has("db"));
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn each_export_gets_a_newer_serial() {
        let dir = temp("serial");
        assert_eq!(next_serial(Some(&dir), 1000, 0), 1000);
        // Same second, and a clock that went back: still newer.
        assert_eq!(next_serial(Some(&dir), 1000, 0), 1001);
        assert_eq!(next_serial(Some(&dir), 500, 0), 1002);
        // Past the library in use.
        assert_eq!(next_serial(Some(&dir), 1000, 5000), 5001);
        assert_eq!(next_serial(None, 10, 0), 10);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
