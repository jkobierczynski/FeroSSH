//! FeroSSH's own record files for this PC (the connection log and the
//! inventory), kept in its folder for this PC (`%LOCALAPPDATA%\FeroSSH`,
//! or `FeroSSH-data` beside a portable exe), sealed while a vault is
//! open.
//!
//! The vault holds the passwords and the connection list; these files
//! say where the user connected and when, which the vault is there to
//! hide too (a portable exe on a USB stick). While a vault is open,
//! `name` is written as `name.sealed`, encrypted with a random key kept
//! in the vault (so a new master password needs no new seal), and the
//! plain `name` is removed. Without an open vault the plain file is used.
//! What was written plain while the vault was locked is read with the
//! sealed part and sealed with it at the next write.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ferossh_config::sealed::{Kdf, Key};

/// The files kept this way.
pub const FILES: [&str; 2] = [crate::connlog_store::FILE, crate::inventory_view::FILE];

/// The name of the local key's file in the vault (`.conf`, as a vault
/// file's name must be; it holds no settings and is never exported).
const KEY_FILE: &str = "local-records-key.conf";

/// The sealed copy's suffix.
const SEALED: &str = ".sealed";

/// What a file holds as read.
#[derive(Debug, Default)]
pub struct Read {
    /// The plain part and the sealed part's text, plain first.
    pub text: String,
    /// There is a sealed part this run cannot open (no vault open, or
    /// another vault's): it is left as it is.
    pub locked: bool,
}

/// The key for the open vault's sealed files, and the secret it came
/// from (a vault removed and made again has another).
static KEY: Mutex<Option<(String, Key)>> = Mutex::new(None);

fn dir() -> Option<PathBuf> {
    crate::portable::local_dir()
}

fn sealed_path(plain: &Path) -> PathBuf {
    let mut p = plain.as_os_str().to_owned();
    p.push(SEALED);
    PathBuf::from(p)
}

/// The key of the open vault, made (and kept in the vault) the first
/// time; `None` when no vault is open.
fn key() -> Option<Result<(), String>> {
    if !crate::vault::is_open() {
        return None;
    }
    // 32 random bytes of secret and 16 of salt, in hex.
    // A key that is there but damaged is never replaced: what it sealed
    // would be lost.
    let secret = match crate::vault::read(KEY_FILE).map(|t| t.trim().to_owned()) {
        Some(s) if s.len() == 96 && s.bytes().all(|b| b.is_ascii_hexdigit()) => {
            zeroize::Zeroizing::new(s)
        }
        Some(_) => {
            return Some(Err("The vault's key for local records is damaged.".into()));
        }
        None => {
            let mut raw = zeroize::Zeroizing::new([0u8; 48]);
            if getrandom::fill(raw.as_mut()).is_err() {
                return Some(Err("Windows gave no random bytes.".into()));
            }
            let s: String = raw.iter().map(|b| format!("{b:02x}")).collect();
            // Another FeroSSH may have made one since: theirs is kept.
            match crate::vault::read_or_write(KEY_FILE, &format!("{s}\n")) {
                Ok(t) => {
                    let t = t.trim().to_owned();
                    if t.len() != 96 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Some(Err("The vault's key for local records is damaged.".into()));
                    }
                    zeroize::Zeroizing::new(t)
                }
                Err(e) => return Some(Err(e)),
            }
        }
    };
    let mut held = KEY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.as_ref().is_some_and(|(s, _)| *s == *secret) {
        return Some(Ok(()));
    }
    let (pass, salt_hex) = secret.split_at(64);
    let salt: Option<Vec<u8>> = (0..16)
        .map(|i| {
            salt_hex
                .get(i * 2..i * 2 + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        })
        .collect();
    let Some(salt) = salt.and_then(|v| <[u8; 16]>::try_from(v).ok()) else {
        return Some(Err("The vault's key for local records is damaged.".into()));
    };
    // The secret is random: the KDF adds nothing and runs at its least.
    match Key::derive(pass, Kdf::LOWEST, salt) {
        Ok(k) => {
            *held = Some((secret.to_string(), k));
            Some(Ok(()))
        }
        Err(e) => Some(Err(e)),
    }
}

fn with_key<T>(f: impl FnOnce(&Key) -> T) -> Option<Result<T, String>> {
    match key()? {
        Ok(()) => {}
        Err(e) => return Some(Err(e)),
    }
    let held = KEY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.as_ref().map(|(_, k)| Ok(f(k)))
}

/// Whether the file is kept sealed now (a vault is open).
pub fn sealing() -> bool {
    crate::vault::is_open()
}

/// Reads file `name`.
pub fn read(name: &str) -> std::io::Result<Read> {
    let Some(dir) = dir() else {
        return Ok(Read::default());
    };
    read_in(&dir, name)
}

fn read_text(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_in(dir: &Path, name: &str) -> std::io::Result<Read> {
    let plain = dir.join(name);
    let mut out = Read {
        text: read_text(&plain)?.unwrap_or_default(),
        locked: false,
    };
    let Some(sealed) = read_text(&sealed_path(&plain))? else {
        return Ok(out);
    };
    match with_key(|k| k.open(&sealed)) {
        Some(Ok(Ok(bytes))) => {
            if !out.text.is_empty() && !out.text.ends_with('\n') {
                out.text.push('\n');
            }
            out.text.push_str(&String::from_utf8_lossy(&bytes));
        }
        _ => out.locked = true,
    }
    Ok(out)
}

/// Writes file `name` whole: sealed while a vault is open (and the
/// plain file removed), else plain. `was` is the read the text came
/// from: a sealed part it could not open is never written over.
pub fn write(name: &str, was: &Read, text: &str) -> std::io::Result<()> {
    let Some(dir) = dir() else {
        return Ok(());
    };
    std::fs::create_dir_all(&dir)?;
    let plain = dir.join(name);
    let mut locked = was.locked;
    if locked && sealing() {
        // Another vault's sealed copy: kept beside, never written over,
        // and this vault's copy starts afresh.
        let mut aside = sealed_path(&plain).into_os_string();
        aside.push(format!(".{}.old", crate::inventory_view::now()));
        std::fs::rename(sealed_path(&plain), aside)?;
        locked = false;
    }
    if !locked {
        match with_key(|k| k.seal(text.as_bytes())) {
            Some(Ok(Ok(sealed))) => return seal_in(&plain, &sealed),
            Some(Ok(Err(e)) | Err(e)) => return Err(std::io::Error::other(e)),
            None => {}
        }
    }
    crate::inventory_view::write_whole(&plain, text)?;
    // Unsealed for good (the vault was removed): the sealed copy, read
    // into `text`, goes.
    if !locked && !sealing() {
        match std::fs::remove_file(sealed_path(&plain)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Puts `sealed` (the whole text, plain part included) in place of
/// `plain` and its sealed copy. The plain part, left behind, would be
/// read twice: it is removed, else emptied, else the sealed copy goes
/// back to what it was, so a read or a retry never sees an entry twice.
fn seal_in(plain: &Path, sealed: &str) -> std::io::Result<()> {
    let at = sealed_path(plain);
    let before = read_text(&at)?;
    crate::inventory_view::write_whole(&at, sealed)?;
    let Err(e) = std::fs::remove_file(plain) else {
        return Ok(());
    };
    if e.kind() == std::io::ErrorKind::NotFound
        || crate::inventory_view::write_whole(plain, "").is_ok()
    {
        return Ok(());
    }
    let back = match &before {
        Some(t) => crate::inventory_view::write_whole(&at, t),
        None => std::fs::remove_file(&at),
    };
    Err(match back {
        Ok(()) => e,
        Err(b) => std::io::Error::new(
            e.kind(),
            format!(
                "{e}; {} holds the same entries and could not be put back ({b})",
                at.display()
            ),
        ),
    })
}

/// Writes `r` (the plain part and the sealed part) plain as `plain` and
/// removes the sealed copy; when that cannot go, the plain file goes
/// back to what it held, so no entry is there twice.
fn unseal_in(plain: &Path, r: &Read) -> std::io::Result<()> {
    let before = read_text(plain)?;
    crate::inventory_view::write_whole(plain, &r.text)?;
    let e = match std::fs::remove_file(sealed_path(plain)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => e,
        _ => return Ok(()),
    };
    let back = match &before {
        Some(t) => crate::inventory_view::write_whole(plain, t),
        None => std::fs::remove_file(plain),
    };
    Err(match back {
        Ok(()) => e,
        Err(b) => std::io::Error::new(
            e.kind(),
            format!(
                "{e}; {} holds the same entries and could not be put back ({b})",
                plain.display()
            ),
        ),
    })
}

/// Reads and writes every file again under its lock: sealed now that a
/// vault is open. Problems are given back, one line each.
pub fn seal_all() -> Vec<String> {
    if !sealing() {
        return Vec::new();
    }
    rewrite_all()
}

/// Before the vault is removed: every sealed file is written plain, so
/// nothing is left that only the vault could open.
pub fn unseal_all() -> Vec<String> {
    let mut problems = Vec::new();
    let Some(dir) = dir() else {
        return problems;
    };
    for name in FILES {
        let _held = match crate::inventory_view::lock(&dir) {
            Ok(l) => l,
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        };
        let r = match read_in(&dir, name) {
            Ok(r) => r,
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        };
        if r.locked {
            problems.push(format!("{name}: the sealed copy could not be opened"));
            continue;
        }
        let done = unseal_in(&dir.join(name), &r);
        if let Err(e) = done {
            problems.push(format!("{name}: {e}"));
        }
    }
    if !problems.is_empty() {
        // The vault stays: the files written plain go back under its
        // seal, so none is left both plain and sealed or half done.
        problems.extend(seal_all());
    }
    problems
}

fn rewrite_all() -> Vec<String> {
    let mut problems = Vec::new();
    let Some(dir) = dir() else {
        return problems;
    };
    for name in FILES {
        if !dir.join(name).exists() {
            continue;
        }
        let done = crate::inventory_view::lock(&dir).and_then(|_held| {
            let r = read_in(&dir, name)?;
            write(name, &r, &r.text)
        });
        if let Err(e) = done {
            problems.push(format!("{name}: {e}"));
        }
    }
    problems
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn plain_without_a_vault() {
        let dir = std::env::temp_dir().join(format!("ferossh-ls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.log"), "one\n").unwrap();
        let r = read_in(&dir, "x.log").unwrap();
        assert_eq!(r.text, "one\n");
        assert!(!r.locked);
        // A sealed copy no open vault can read is reported, not lost.
        std::fs::write(dir.join("x.log.sealed"), "not ours").unwrap();
        let r = read_in(&dir, "x.log").unwrap();
        assert!(r.locked);
        assert_eq!(r.text, "one\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fresh(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ferossh-ls-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sealing_never_leaves_an_entry_in_both_copies() {
        let dir = fresh("seal");
        let plain = dir.join("x.log");
        std::fs::write(&plain, "new\n").unwrap();
        seal_in(&plain, "SEALED old+new").unwrap();
        assert!(!plain.exists());
        assert_eq!(
            std::fs::read_to_string(sealed_path(&plain)).unwrap(),
            "SEALED old+new"
        );
        // A plain part that can be neither removed nor emptied (here a
        // folder): the sealed copy goes back to what it was.
        std::fs::create_dir_all(plain.join("busy")).unwrap();
        assert!(seal_in(&plain, "SEALED twice").is_err());
        assert_eq!(
            std::fs::read_to_string(sealed_path(&plain)).unwrap(),
            "SEALED old+new"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsealing_never_leaves_an_entry_in_both_copies() {
        let dir = fresh("unseal");
        let plain = dir.join("x.log");
        std::fs::write(&plain, "new\n").unwrap();
        // A sealed copy that cannot be removed (here a folder): the plain
        // file goes back to what it held.
        std::fs::create_dir_all(sealed_path(&plain).join("busy")).unwrap();
        let r = Read {
            text: "new\nold\n".into(),
            locked: false,
        };
        assert!(unseal_in(&plain, &r).is_err());
        assert_eq!(std::fs::read_to_string(&plain).unwrap(), "new\n");
        std::fs::remove_dir_all(sealed_path(&plain)).unwrap();
        std::fs::write(sealed_path(&plain), "SEALED").unwrap();
        unseal_in(&plain, &r).unwrap();
        assert_eq!(std::fs::read_to_string(&plain).unwrap(), "new\nold\n");
        assert!(!sealed_path(&plain).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
