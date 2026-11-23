//! Reading the team library (see [`ferossh_config::team`]) from its share,
//! with a checked copy kept in FeroSSH's folder: a laptop away from the
//! share keeps the team's connections and rules, and a share that serves
//! a file with a bad signature does not take them away either.
//!
//! The copy is checked again each time it is read, against the keys set
//! now, so changing the key drops a copy signed by the old one. Each
//! library path has its own copy, so setting another library never uses
//! the copy of the one before.
//!
//! A library older than one FeroSSH already used, from the same path
//! or signed by the same key at any path, is refused: putting an earlier
//! signed library back does not bring back its connections and rules.

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
/// copy could not be written ([`keep_newest`]).
static ACCEPTED: Mutex<Vec<Accepted>> = Mutex::new(Vec::new());

/// A library as read and checked: its team, and the bytes and signature
/// its checked copy is written from.
#[derive(Clone)]
struct Copy {
    team: Team,
    bytes: Vec<u8>,
    sig: String,
}

/// One entry of [`ACCEPTED`].
struct Accepted {
    library: String,
    keys: Vec<String>,
    copy: Copy,
}

impl Accepted {
    fn is(&self, library: &str, keys: &[String]) -> bool {
        self.library == library.trim() && self.keys == keys
    }
}

/// Takes `copy` as the newest accepted for `library` under `keys`,
/// unless a newer one already is, which then comes back. One lock covers
/// the check and the update; nothing slow happens under it.
fn record(library: &str, keys: &[String], copy: Copy) -> Option<Team> {
    let Ok(mut a) = ACCEPTED.lock() else {
        return None;
    };
    match a.iter_mut().find(|e| e.is(library, keys)) {
        Some(e) if e.copy.team.serial > copy.team.serial => Some(e.copy.team.clone()),
        Some(e) => {
            e.copy = copy;
            None
        }
        None => {
            a.push(Accepted {
                library: library.trim().to_owned(),
                keys: keys.to_vec(),
                copy,
            });
            None
        }
    }
}

/// The newest library seen for `library` under `keys`: the checked copy
/// on disk (taken into [`ACCEPTED`] too, so it stays the baseline if the
/// copy later cannot be read) or this run's, whichever is newer.
fn newest_seen(library: &str, keys: &[String], cache_dir: &Path) -> Option<Team> {
    if let Some(disk) = read_copy(library, keys, cache_dir) {
        let team = disk.team.clone();
        return Some(record(library, keys, disk).unwrap_or(team));
    }
    ACCEPTED.lock().ok().and_then(|a| {
        a.iter()
            .find(|e| e.is(library, keys))
            .map(|e| e.copy.team.clone())
    })
}

/// The newest serial seen for `library` under `keys` (this run's or the
/// checked copy's), without taking anything: 0 for none.
fn seen_serial(library: &str, keys: &[String], cache_dir: &Path) -> u64 {
    let run = ACCEPTED
        .lock()
        .ok()
        .and_then(|a| {
            a.iter()
                .find(|e| e.is(library, keys))
                .map(|e| e.copy.team.serial)
        })
        .unwrap_or(0);
    let disk = read_copy(library, keys, cache_dir).map_or(0, |c| c.team.serial);
    run.max(disk)
}

/// Whether `team`, a library read under `keys`, is older than a library
/// the same key signed that this run has taken since (at another path,
/// by a read that crossed it): the window then reads it again, and the
/// read refuses it. This run's dates only, so it never waits on disk.
pub fn superseded(team: &Team, keys: &[String], dir: &Path) -> bool {
    if *team == Team::default() {
        return false;
    }
    let fps = team::fingerprints(keys);
    SIGNERS.lock().is_ok_and(|run| {
        run.iter()
            .any(|(d, fp, n)| d == dir && fps.contains(fp) && *n > team.serial)
    })
}

/// Takes the library just read (`copy`) as the newest accepted for
/// `library` under `keys`, unless a newer one was seen before (this run,
/// or the checked copy on disk), which then comes back instead.
fn accept_unless_older(
    library: &str,
    keys: &[String],
    cache_dir: &Path,
    copy: Copy,
) -> Option<Team> {
    // Read and checked before the lock, so a slow folder never holds it.
    if let Some(disk) = read_copy(library, keys, cache_dir)
        && disk.team.serial > copy.team.serial
    {
        let team = disk.team.clone();
        return Some(record(library, keys, disk).unwrap_or(team));
    }
    record(library, keys, copy)
}

/// The newest library accepted this run for `library` under `keys`, when
/// it is newer than serial `than` (the one in use): the window puts it
/// back when a later read it published turns out older (two reads that
/// crossed). Cheap: only a newer library is cloned.
pub fn newest_accepted(library: Option<&str>, keys: &[String], than: u64) -> Option<Team> {
    let library = library.map(str::trim).filter(|l| !l.is_empty())?;
    ACCEPTED.lock().ok().and_then(|a| {
        a.iter()
            .find(|e| e.is(library, keys) && e.copy.team.serial > than)
            .map(|e| e.copy.team.clone())
    })
}

/// The serial for a new export at `now` (seconds since 1970): past the
/// library this PC uses (`current`) and past the last export from this
/// PC, kept in `dir`, so every export is newer than the one before even
/// when the clock is behind or two exports fall in the same second. An
/// error when the last serial cannot be read or the new one kept: the
/// export must not go ahead then, or two exports could share a serial.
pub fn next_serial(dir: &Path, now: u64, current: u64) -> std::io::Result<u64> {
    // Read, count on and keep under the lock every FeroSSH process takes
    // for its folder, so two exports at once never share a serial.
    let _held = crate::inventory_view::lock(dir)?;
    let file = dir.join("team-export-serial");
    let last = match std::fs::read_to_string(&file) {
        Ok(t) => t
            .trim()
            .parse::<u64>()
            .map_err(|_| std::io::Error::other("team-export-serial is damaged"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    let past = |n: u64| {
        n.checked_add(1)
            .ok_or_else(|| std::io::Error::other("the library's version number cannot go higher"))
    };
    let serial = now.max(past(current)?).max(past(last)?);
    let tmp = write_new(&file, format!("{serial}\n").as_bytes(), serial)?;
    std::fs::rename(&tmp, &file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(serial)
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

/// The newest library date FeroSSH has used per signing key, one line
/// `SHA256:... <serial>` each, in FeroSSH's folder. A library older than
/// that is refused whatever path it is read from, so pointing FeroSSH at
/// an older file, or one put at a new path, does not bring back an
/// earlier library.
const SIGNERS_FILE: &str = "team-library-signers";

/// This run's dates per folder and signing key, so they hold for the
/// run even when [`SIGNERS_FILE`] cannot be written.
static SIGNERS: Mutex<Vec<(PathBuf, String, u64)>> = Mutex::new(Vec::new());

fn read_signers(dir: &Path) -> Vec<(String, u64)> {
    let Ok(bytes) = read_capped(&dir.join(SIGNERS_FILE), 64 * 1024) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| {
            let (fp, serial) = l.trim().rsplit_once(' ')?;
            Some((fp.trim().to_owned(), serial.parse().ok()?))
        })
        .collect()
}

/// The newest library date used with any of `keys`: a library they
/// check must be at least that new.
fn signer_floor(keys: &[String], dir: &Path) -> u64 {
    let fps = team::fingerprints(keys);
    let run: Vec<(String, u64)> = SIGNERS
        .lock()
        .map(|s| {
            s.iter()
                .filter(|(d, _, _)| d == dir)
                .map(|(_, fp, n)| (fp.clone(), *n))
                .collect()
        })
        .unwrap_or_default();
    run.into_iter()
        .chain(read_signers(dir))
        .filter(|(fp, _)| fps.contains(fp))
        .map(|(_, serial)| serial)
        .max()
        .unwrap_or(0)
}

/// Why [`claim_signer`] did not take a library.
#[derive(Debug)]
enum Unclaimed {
    /// Older than this date, used before with one of the keys.
    Older(u64),
    /// The folder lock could not be had.
    Unchecked(std::io::Error),
}

/// Checks a library dated `serial` and signed by `signer` against the
/// newest date used with any of `keys` and, unless it is older, raises
/// `signer`'s date to `serial`: one step under [`SIGNERS`] (and the
/// folder lock, between FeroSSH processes), so two reads at once, of
/// two paths, cannot each pass against the date from before either.
/// [`Unclaimed::Older`] with that date when the library is older, and
/// [`Unclaimed::Unchecked`] when the folder is there but its lock cannot
/// be had: another FeroSSH may be keeping a newer date just then, so the
/// library is not taken unchecked. Otherwise whether the new date could be kept on disk
/// (it holds for the run either way).
fn claim_signer(
    dir: &Path,
    keys: &[String],
    signer: &str,
    serial: u64,
) -> Result<std::io::Result<()>, Unclaimed> {
    let held = match crate::inventory_view::lock(dir) {
        // A folder that is there but locked all this time: another
        // FeroSSH may be keeping a newer date in it just now.
        Err(e) if dir.is_dir() => return Err(Unclaimed::Unchecked(e)),
        // No folder to keep dates in (or lock): none another FeroSSH
        // could have kept either, so this run's dates decide.
        held => held,
    };
    let mut run = SIGNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fps = team::fingerprints(keys);
    let mut disk = read_signers(dir);
    let floor = run
        .iter()
        .filter(|(d, _, _)| d == dir)
        .map(|(_, fp, n)| (fp, *n))
        .chain(disk.iter().map(|(fp, n)| (fp, *n)))
        .filter(|(fp, _)| fps.contains(fp))
        .map(|(_, n)| n)
        .max()
        .unwrap_or(0);
    if serial < floor {
        return Err(Unclaimed::Older(floor));
    }
    match run.iter_mut().find(|(d, fp, _)| d == dir && fp == signer) {
        Some(e) => e.2 = e.2.max(serial),
        None => run.push((dir.to_owned(), signer.to_owned(), serial)),
    }
    // Published for this run; the disk write below stays serialised by
    // the folder lock, and a reader of this run's dates need not wait.
    drop(run);
    Ok((|| {
        let _held = held?;
        match disk.iter_mut().find(|(fp, _)| fp == signer) {
            Some(e) if e.1 >= serial => return Ok(()),
            Some(e) => e.1 = serial,
            None => disk.push((signer.to_owned(), serial)),
        }
        let text: String = disk.iter().map(|(fp, n)| format!("{fp} {n}\n")).collect();
        let file = dir.join(SIGNERS_FILE);
        let tmp = write_new(&file, text.as_bytes(), serial)?;
        std::fs::rename(&tmp, &file).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    })())
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

/// Writes the newest library accepted this run for `library` under
/// `keys` as its checked copy, so the copy never falls behind it: when
/// this read is still the newest (`current`), or when the copy there is
/// one these keys check and older. Two reads that cross therefore leave
/// the newer library on disk, whichever finishes last.
fn keep_newest(
    cache_dir: &Path,
    library: &str,
    keys: &[String],
    current: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    // Held while the newest is picked and moved into place, so a read
    // that picked an older one cannot move it in after a newer one.
    let _held = COMMIT.lock();
    let Some(newest) = ACCEPTED.lock().ok().and_then(|a| {
        a.iter()
            .find(|e| e.is(library, keys))
            .map(|e| e.copy.clone())
    }) else {
        return Ok(());
    };
    let behind =
        read_copy(library, keys, cache_dir).is_some_and(|c| c.team.serial < newest.team.serial);
    if !current() && !behind {
        return Ok(());
    }
    write_copy(cache_dir, library, &newest.bytes, &newest.sig)
}

/// Writes the checked copy for `library` beside it, then moves it over
/// the old one, so a failed write leaves the old copy as it was. The
/// caller holds [`COMMIT`].
fn write_copy(cache_dir: &Path, library: &str, bytes: &[u8], sig: &str) -> std::io::Result<()> {
    /// Numbers each write, so two never share a temporary file.
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let file = cache_path(cache_dir, library);
    // The library's path first, checked on reading: two paths whose
    // hashes meet never share a copy.
    let mut record = format!("{}\n{}\n{sig}", library.trim(), sig.len()).into_bytes();
    record.extend_from_slice(bytes);
    let tmp = write_new(&file, &record, WRITES.fetch_add(1, Ordering::Relaxed))?;
    let done = std::fs::rename(&tmp, &file);
    // Still there when the rename failed.
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
fn read_copy(library: &str, keys: &[String], cache_dir: &Path) -> Option<Copy> {
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
    let team = team::load(bytes, sig, &team::keys(keys)).ok()?;
    Some(Copy {
        team,
        bytes: bytes.to_vec(),
        sig: sig.to_owned(),
    })
}

/// The checked copy's team, if it is still good under `keys`.
fn from_cache(library: &str, keys: &[String], cache_dir: &Path) -> Option<Team> {
    read_copy(library, keys, cache_dir).map(|c| c.team)
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
    let why = 'read: {
        match read {
            Ok((bytes, sig)) => match team::load(&bytes, &sig, &team::keys(keys)) {
                Ok(team) => {
                    // Signed, but older than the copy checked last: the share
                    // was turned back to an earlier library (one from before a
                    // rule was added, say), which only someone who cannot sign
                    // a new one has reason to do. The newer copy stays in use.
                    // Checked and taken as the new baseline in one step, so two
                    // reads at once cannot each pass against an old baseline.
                    // Refused too when older than a library the same key
                    // signed that FeroSSH used before, at this path or another
                    // (FeroSSH pointed at an older file, or one put at a new
                    // path). Checked and the key's date raised in one step,
                    // before it is taken as this path's newest.
                    // A newer copy of this path wins without this library's
                    // date being claimed: it is not used.
                    let path_has_newer = seen_serial(library, keys, cache_dir) > team.serial;
                    let signer = team::signer(&sig).unwrap_or_default();
                    let claimed = (!path_has_newer)
                        .then(|| claim_signer(cache_dir, keys, &signer, team.serial));
                    if let Some(Err(Unclaimed::Unchecked(e))) = &claimed {
                        // The copy checked last, below, as when the share
                        // cannot be read.
                        break 'read format!(
                            "was not used: FeroSSH could not check it against the newest library \
                         date ({e})"
                        );
                    }
                    let kept = if let Some(Err(Unclaimed::Older(floor))) = claimed {
                        match newest_seen(library, keys, cache_dir).filter(|t| t.serial >= floor) {
                            Some(kept) => Some(kept),
                            None => {
                                let note = format!(
                                    "The team library {shown} was not used: it is older than a \
                                 library signed with the same key that FeroSSH used before \
                                 (exported {}, that one {}), so it may be an earlier library \
                                 put back. Its connections and rules are not available until a \
                                 newer library is there; ask your administrator to export and \
                                 sign the library again.",
                                    exported(team.serial),
                                    exported(floor),
                                );
                                return Loaded {
                                    team: Team::default(),
                                    note: Some(note),
                                };
                            }
                        }
                    } else {
                        let copy = Copy {
                            team: team.clone(),
                            bytes: bytes.clone(),
                            sig: sig.clone(),
                        };
                        accept_unless_older(library, keys, cache_dir, copy)
                    };
                    // A newer copy of this path is still no older than a
                    // library the same key signed since, at another path.
                    let kept = match kept {
                        Some(k) if k.serial < signer_floor(keys, cache_dir) => {
                            let note = format!(
                                "The team library {shown} was not used, and neither is the copy \
                                 FeroSSH checked last: both are older than a library signed with \
                                 the same key that FeroSSH used since (exported {}, the copy {}). \
                                 Its connections and rules are not available until a newer \
                                 library is there.",
                                exported(team.serial),
                                exported(k.serial),
                            );
                            return Loaded {
                                team: Team::default(),
                                note: Some(note),
                            };
                        }
                        kept => kept,
                    };
                    if let Some(kept) = kept {
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
                    // The baseline is set already, so a copy that cannot be
                    // written only costs the offline fallback.
                    let mut notes = Vec::new();
                    if let Err(e) = keep_newest(cache_dir, library, keys, current) {
                        notes.push(format!(
                        "FeroSSH could not keep a copy of the team library {shown} ({e}), so it \
                         is used now but is not there when the share cannot be read."
                    ));
                    }
                    if let Some(Ok(Err(e))) = claimed {
                        notes.push(format!(
                            "FeroSSH could not note the date of the team library {shown} ({e}), so \
                         after a restart it cannot tell an older library signed with the same \
                         key at another path."
                        ));
                    }
                    let note = (!notes.is_empty()).then(|| notes.join(" "));
                    return Loaded { team, note };
                }
                Err(why) => format!("was not used: {why}"),
            },
            Err(e) => format!("could not be read ({e})"),
        }
    };
    // Not a copy older than a library the same key signed since.
    let floor = signer_floor(keys, cache_dir);
    match newest_seen(library, keys, cache_dir).filter(|t| t.serial >= floor) {
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
        // And when the share then cannot be read, that one stays too,
        // not the older copy on disk.
        std::fs::remove_file(&lib).unwrap();
        let offline = load(Some(path), &keys, &unwritable, &|| true);
        assert!(offline.team.has("newest"));
        // Nor with the older copy on disk (serial 200) as the fallback.
        let offline = load(Some(path), &keys, &cache, &|| true);
        assert!(offline.team.has("newest"));
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
        assert_eq!(next_serial(&dir, 1000, 0).unwrap(), 1000);
        // Same second, and a clock that went back: still newer.
        assert_eq!(next_serial(&dir, 1000, 0).unwrap(), 1001);
        assert_eq!(next_serial(&dir, 500, 0).unwrap(), 1002);
        // Past the library in use.
        assert_eq!(next_serial(&dir, 1000, 5000).unwrap(), 5001);
        // No number past the largest there is.
        assert!(next_serial(&dir, 1000, u64::MAX).is_err());
        // A damaged record, or a folder that cannot hold one: no serial.
        std::fs::write(dir.join("team-export-serial"), "junk").unwrap();
        assert!(next_serial(&dir, 1000, 0).is_err());
        let file = dir.join("a-file");
        std::fs::write(&file, b"").unwrap();
        assert!(next_serial(&file, 1000, 0).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The folder lock opens its file with no sharing, which Windows
    /// enforces per handle: two handles in one process exclude each
    /// other exactly as two processes do.
    #[cfg(windows)]
    #[test]
    fn exports_at_once_never_share_a_serial() {
        let dir = temp("serial-race");
        {
            let _held = crate::inventory_view::lock(&dir).unwrap();
            assert!(crate::inventory_view::lock(&dir).is_err());
        }
        let serials: Vec<u64> = std::thread::scope(|s| {
            let all: Vec<_> = (0..8)
                .map(|_| s.spawn(|| next_serial(&dir, 1000, 0).unwrap()))
                .collect();
            all.into_iter().map(|t| t.join().unwrap()).collect()
        });
        let mut sorted = serials.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, (1000..1008).collect::<Vec<_>>(), "{serials:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crossed_reads_leave_the_newer_copy() {
        let share = temp("crossed");
        let cache = temp("crossed-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let lib = write_dated(&share, "old", 100);
        let path = lib.to_str().unwrap();
        // The newest read sees serial 100 and writes its copy.
        assert!(load(Some(path), &keys, &cache, &|| true).team.has("old"));
        // An older read that finishes later saw serial 200: no longer
        // the newest, it still puts its newer library on disk.
        write_dated(&share, "new", 200);
        assert!(load(Some(path), &keys, &cache, &|| false).team.has("new"));
        let copy = cached(Some(path), &keys, &cache);
        assert!(copy.has("new") && copy.serial == 200);
        // And a current read of 100 after that does not write it back.
        write_dated(&share, "old", 100);
        assert!(load(Some(path), &keys, &cache, &|| true).team.has("new"));
        assert!(cached(Some(path), &keys, &cache).has("new"));
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_current_read_that_was_overtaken_writes_the_newer_library() {
        let share = temp("overtaken");
        let cache = temp("overtaken-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let read = |host: &str, serial: u64| {
            let lib = write_dated(&share, host, serial);
            let bytes = std::fs::read(&lib).unwrap();
            let sig = std::fs::read_to_string(sig_path(&lib)).unwrap();
            let team = team::load(&bytes, &sig, &team::keys(&keys)).unwrap();
            (lib, Copy { team, bytes, sig })
        };
        // The current read accepts 100; before it writes its copy, a
        // later read accepts 200.
        let (lib, old) = read("old", 100);
        let path = lib.to_str().unwrap();
        assert!(accept_unless_older(path, &keys, &cache, old).is_none());
        let (_, new) = read("new", 200);
        assert!(accept_unless_older(path, &keys, &cache, new).is_none());
        // The current read now writes: the newest accepted, 200.
        keep_newest(&cache, path, &keys, &|| true).unwrap();
        let copy = cached(Some(path), &keys, &cache);
        assert!(copy.has("new") && copy.serial == 200);
        // The later read, no longer current, leaves it as it is.
        keep_newest(&cache, path, &keys, &|| false).unwrap();
        assert_eq!(cached(Some(path), &keys, &cache).serial, 200);
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn an_older_library_by_the_same_key_is_refused_at_any_path() {
        let share = temp("other-path");
        let cache = temp("other-path-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        for d in ["a", "b"] {
            std::fs::create_dir_all(share.join(d)).unwrap();
        }
        let new = write_dated(&share.join("a"), "new", 200);
        let old = write_dated(&share.join("b"), "old", 100);
        let (new, old) = (new.to_str().unwrap(), old.to_str().unwrap());
        assert!(load(Some(new), &keys, &cache, &|| true).note.is_none());
        // Another path, signed by the same key, older: not used.
        let refused = load(Some(old), &keys, &cache, &|| true);
        assert!(refused.team.names.is_empty());
        assert!(refused.note.unwrap().contains("older than a library"));
        // Kept on disk too, so it holds after a restart.
        SIGNERS.lock().unwrap().retain(|(d, _, _)| d != &cache);
        assert!(
            load(Some(old), &keys, &cache, &|| true)
                .team
                .names
                .is_empty()
        );
        // Nor does its old checked copy come back when the share goes.
        std::fs::remove_file(old).unwrap();
        assert!(
            load(Some(old), &keys, &cache, &|| true)
                .team
                .names
                .is_empty()
        );
        // Equal or newer at that path is fine.
        let same = write_dated(&share.join("b"), "same", 200);
        assert!(
            load(same.to_str(), &keys, &cache, &|| true)
                .team
                .has("same")
        );
        // A key that signed nothing newer is not held back.
        let other = Ed25519Keypair::from_seed(&[9; 32]);
        let other = PrivateKey::new(KeypairData::Ed25519(other), "other").unwrap();
        let bytes = team::export("Host theirs\n", &Settings::default(), &[], 50);
        let sig = SshSig::sign(&other, team::NAMESPACE, HashAlg::Sha512, &bytes)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap();
        let theirs = share.join("c.ferossh-library");
        std::fs::write(&theirs, &bytes).unwrap();
        std::fs::write(sig_path(&theirs), sig).unwrap();
        let theirs_keys = [other.public_key().to_openssh().unwrap()];
        let used = load(theirs.to_str(), &theirs_keys, &cache, &|| true);
        assert!(used.team.has("theirs") && used.note.is_none());
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn the_key_date_is_checked_and_raised_in_one_step() {
        let cache = temp("claim");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let fp = team::fingerprints(&keys).pop().unwrap();
        // Two reads of two paths: once the newer one is claimed, the
        // older one fails against it, whichever started first.
        assert!(matches!(claim_signer(&cache, &keys, &fp, 200), Ok(Ok(()))));
        assert!(matches!(
            claim_signer(&cache, &keys, &fp, 150),
            Err(Unclaimed::Older(200))
        ));
        assert!(matches!(claim_signer(&cache, &keys, &fp, 200), Ok(Ok(()))));
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// Another FeroSSH holding the folder lock may be keeping a newer
    /// date just then: the library is not taken unchecked. (Windows: the
    /// lock is exclusive there.)
    #[cfg(windows)]
    #[test]
    fn a_library_is_not_taken_unchecked_while_the_folder_is_locked() {
        let share = temp("locked");
        let cache = temp("locked-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let lib = write_dated(&share, "new", 300);
        let held = crate::inventory_view::lock(&cache).unwrap();
        let got = load(lib.to_str(), &keys, &cache, &|| true);
        assert!(got.team.names.is_empty());
        assert!(got.note.unwrap().contains("could not check"));
        drop(held);
        assert!(load(lib.to_str(), &keys, &cache, &|| true).team.has("new"));
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_library_that_loses_to_its_paths_copy_claims_no_date() {
        let share = temp("loser");
        let cache = temp("loser-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let lib = write_dated(&share, "copy", 300);
        let path = lib.to_str().unwrap();
        let bytes = std::fs::read(&lib).unwrap();
        let sig = std::fs::read_to_string(sig_path(&lib)).unwrap();
        {
            let _held = COMMIT.lock();
            write_copy(&cache, path, &bytes, &sig).unwrap();
        }
        write_dated(&share, "older", 200);
        assert!(load(Some(path), &keys, &cache, &|| true).team.has("copy"));
        assert_eq!(signer_floor(&keys, &cache), 0);
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_paths_copy_older_than_the_keys_date_is_not_used_either() {
        let share = temp("copy-below");
        let cache = temp("copy-below-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        for d in ["a", "b"] {
            std::fs::create_dir_all(share.join(d)).unwrap();
        }
        // Path b was used at 180 before path a's 200 came.
        let b = write_dated(&share.join("b"), "b180", 180);
        let b = b.to_str().unwrap();
        assert!(load(Some(b), &keys, &cache, &|| true).team.has("b180"));
        let a = write_dated(&share.join("a"), "a200", 200);
        assert!(load(a.to_str(), &keys, &cache, &|| true).team.has("a200"));
        // An older one at b: neither it nor b's copy of 180.
        write_dated(&share.join("b"), "b150", 150);
        let got = load(Some(b), &keys, &cache, &|| true);
        assert!(got.team.names.is_empty() && got.note.is_some());
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_read_overtaken_by_a_newer_date_is_superseded() {
        let share = temp("superseded");
        let cache = temp("superseded-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let lib = write_dated(&share, "new", 200);
        let new = load(lib.to_str(), &keys, &cache, &|| true).team;
        assert!(!superseded(&new, &keys, &cache));
        let mut older = new.clone();
        older.serial = 150;
        assert!(superseded(&older, &keys, &cache));
        assert!(!superseded(&Team::default(), &keys, &cache));
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn reads_of_several_paths_at_once_end_at_the_newest_date() {
        let share = temp("paths-at-once");
        let cache = temp("paths-at-once-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let paths: Vec<(PathBuf, u64)> = (1..=8u64)
            .map(|n| {
                let dir = share.join(n.to_string());
                std::fs::create_dir_all(&dir).unwrap();
                (write_dated(&dir, &format!("lib{n}"), n * 100), n * 100)
            })
            .collect();
        let start = std::sync::Barrier::new(paths.len());
        let used: Vec<(u64, bool)> = std::thread::scope(|s| {
            let all: Vec<_> = paths
                .iter()
                .map(|(path, serial)| {
                    let (start, keys, cache) = (&start, &keys, &cache);
                    s.spawn(move || {
                        start.wait();
                        let got = load(path.to_str(), keys, cache, &|| true);
                        (*serial, got.team.serial == *serial)
                    })
                })
                .collect();
            all.into_iter().map(|t| t.join().unwrap()).collect()
        });
        // The newest is never refused, and nothing older gets past it now.
        assert!(used.contains(&(800, true)), "{used:?}");
        assert_eq!(signer_floor(&keys, &cache), 800);
        for (path, serial) in paths.iter().filter(|(_, n)| *n < 800) {
            let again = load(path.to_str(), &keys, &cache, &|| true);
            assert_ne!(again.team.serial, *serial);
        }
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_copy_from_disk_stays_the_baseline_when_it_goes() {
        let share = temp("disk-baseline");
        let cache = temp("disk-baseline-cache");
        let keys = [admin().public_key().to_openssh().unwrap()];
        let lib = write_dated(&share, "newest", 300);
        let path = lib.to_str().unwrap();
        // A copy of 300 on disk, as an earlier run of FeroSSH left it.
        let bytes = std::fs::read(&lib).unwrap();
        let sig = std::fs::read_to_string(sig_path(&lib)).unwrap();
        {
            let _held = COMMIT.lock();
            write_copy(&cache, path, &bytes, &sig).unwrap();
        }
        // This run first reads 200: refused for the copy of 300.
        write_dated(&share, "old", 200);
        assert!(load(Some(path), &keys, &cache, &|| true).team.has("newest"));
        // The copy then goes; 250 is still older than what was seen.
        for f in std::fs::read_dir(&cache).unwrap() {
            std::fs::remove_file(f.unwrap().path()).unwrap();
        }
        write_dated(&share, "older", 250);
        let after = load(Some(path), &keys, &cache, &|| true);
        assert!(after.team.has("newest") && after.note.is_some());
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
