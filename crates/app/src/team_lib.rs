//! Reading the team library (see [`ferossh_config::team`]) from its share,
//! with a checked copy kept in FeroSSH's folder: a laptop away from the
//! share keeps the team's connections and rules, and a share that serves
//! a file with a bad signature does not take them away either.
//!
//! The copy is checked again each time it is read, against the keys set
//! now, so changing the key drops a copy signed by the old one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

/// The checked copy in `dir`.
fn cache_paths(dir: &Path) -> (PathBuf, PathBuf) {
    let file = dir.join("team-library.cache");
    let sig = sig_path(&file);
    (file, sig)
}

/// A file read whole, refused when larger than a library may be.
fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_LEN as u64 {
        return Err(std::io::Error::other("larger than 8 MB"));
    }
    std::fs::read(path)
}

/// The copy in `cache_dir`, if it is still good under `keys`: what a
/// window starts with, before the share is read.
pub fn cached(library: Option<&str>, keys: &[String], cache_dir: &Path) -> Team {
    if library.is_none_or(|l| l.trim().is_empty()) {
        return Team::default();
    }
    from_cache(keys, cache_dir).unwrap_or_default()
}

/// The copy in `cache_dir`, if it is still good under `keys`.
fn from_cache(keys: &[String], cache_dir: &Path) -> Option<Team> {
    let (file, sig) = cache_paths(cache_dir);
    let bytes = read_capped(&file).ok()?;
    let sig = std::fs::read_to_string(sig).ok()?;
    team::load(&bytes, &sig, &team::keys(keys)).ok()
}

/// What reading the library gave: the team (empty for none) and, when
/// something is wrong, what to tell the user.
#[derive(Debug, Default)]
pub struct Loaded {
    pub team: Team,
    pub note: Option<String>,
}

/// Reads library `library` (its `.sig` beside it), checks it against
/// `keys` and keeps a copy in `cache_dir`; on any failure, the copy
/// checked last.
pub fn load(library: Option<&str>, keys: &[String], cache_dir: &Path) -> Loaded {
    let Some(library) = library.map(str::trim).filter(|l| !l.is_empty()) else {
        return Loaded::default();
    };
    let path = PathBuf::from(library);
    let shown = crate::ssh::sanitize_line(library);
    let read = read_capped(&path).and_then(|b| {
        let sig = std::fs::read_to_string(sig_path(&path))?;
        Ok((b, sig))
    });
    let why = match read {
        Ok((bytes, sig)) => match team::load(&bytes, &sig, &team::keys(keys)) {
            Ok(team) => {
                let (file, sig_file) = cache_paths(cache_dir);
                // A copy that cannot be written only costs the offline
                // fallback.
                if std::fs::write(&file, &bytes).is_ok() {
                    let _ = std::fs::write(&sig_file, sig);
                }
                return Loaded { team, note: None };
            }
            Err(why) => format!("was not used: {why}"),
        },
        Err(e) => format!("could not be read ({e})"),
    };
    match from_cache(keys, cache_dir) {
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
        let first = load(Some(path), &keys, &cache);
        assert!(first.note.is_none());
        assert!(first.team.has("web"));
        // Tampered on the share: refused, the copy used.
        std::fs::write(&lib, b"FeroSSH connections v1\nend\n").unwrap();
        let tampered = load(Some(path), &keys, &cache);
        assert!(tampered.team.has("web"));
        assert!(tampered.note.unwrap().contains("signature does not match"));
        // Gone from the share: the copy.
        std::fs::remove_file(&lib).unwrap();
        let offline = load(Some(path), &keys, &cache);
        assert!(offline.team.has("web"));
        assert!(offline.note.unwrap().contains("could not be read"));
        // Another key: the copy no longer passes either.
        let other = load(Some(path), &["ssh-ed25519 AAAA junk".into()], &cache);
        assert!(other.team.is_empty());
        // No library set: nothing, and nothing to say.
        let off = load(None, &keys, &cache);
        assert!(off.team.is_empty() && off.note.is_none());
        let _ = std::fs::remove_dir_all(&share);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
