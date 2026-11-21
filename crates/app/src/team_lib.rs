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

/// The signature file beside `library`: its name with `.sig` added, as
/// `ssh-keygen -Y sign` writes it.
pub fn sig_path(library: &Path) -> PathBuf {
    let mut name = OsString::from(library.as_os_str());
    name.push(".sig");
    PathBuf::from(name)
}

/// The largest `.sig` file read: an SSH signature is a few hundred bytes.
const MAX_SIG_LEN: usize = 64 * 1024;

/// The checked copy of library `library` in `dir`: one file holding the
/// signature's length on the first line, the signature, then the library,
/// so the two are always replaced together. The name carries a hash of
/// the library's path.
fn cache_path(dir: &Path, library: &str) -> PathBuf {
    // FNV-1a: stable across builds, unlike the standard hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in library.trim().to_lowercase().bytes() {
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
    /// Held from the check that this read is still the newest to the
    /// copy's rename.
    static COMMIT: Mutex<()> = Mutex::new(());
    let file = cache_path(cache_dir, library);
    let mut record = format!("{}\n{sig}", sig.len()).into_bytes();
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
    let record = read_capped(&cache_path(cache_dir, library), MAX_LEN + MAX_SIG_LEN + 32).ok()?;
    let (len, rest) = record.split_at(record.iter().position(|b| *b == b'\n')?);
    let len: usize = std::str::from_utf8(len).ok()?.parse().ok()?;
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
            Ok(team) => {
                // A copy that cannot be written only costs the offline
                // fallback.
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
        let bytes = team::export(&format!("Host {host}\n"), &Settings::default());
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
        // Another key: the copy no longer passes either.
        let other = load(
            Some(path),
            &["ssh-ed25519 AAAA junk".into()],
            &cache,
            &|| true,
        );
        assert!(other.team.is_empty());
        // Another library, not there: not the first one's copy.
        let elsewhere = share.join("other.ferossh-library");
        let moved = load(Some(elsewhere.to_str().unwrap()), &keys, &cache, &|| true);
        assert!(moved.team.is_empty());
        assert!(cached(Some(elsewhere.to_str().unwrap()), &keys, &cache).is_empty());
        assert!(cached(Some(path), &keys, &cache).has("web"));
        // No library set: nothing, and nothing to say.
        let off = load(None, &keys, &cache, &|| true);
        assert!(off.team.is_empty() && off.note.is_none());
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
