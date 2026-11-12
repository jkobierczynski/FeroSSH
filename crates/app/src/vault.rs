//! The master password vault: FeroSSH's connection files
//! (`ferossh.conf`, `sessions.conf`, the `imported-*.conf` files) and
//! saved passwords, kept together in one encrypted file
//! ([`ferossh_config::sealed`]) instead of in plain files and the Windows
//! Credential Manager. FeroSSH asks for the master password when it
//! starts; while it runs, the files are read from and written to the
//! vault in memory, and every change is sealed to disk at once.
//!
//! The vault file can live anywhere: beside a portable exe on a USB
//! stick, or in a OneDrive or Dropbox folder that two PCs share. A
//! change another PC made is picked up on the next read (the file's size
//! or time changed), so the copy written next starts from it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use ferossh_config::bundle::{Bundle, Secret};
use ferossh_config::sealed::{self, Key, OpenError};
use zeroize::Zeroizing;

/// The file name of the vault in FeroSSH's folder.
pub const FILE_NAME: &str = "connections.vault";

/// A key for a new vault (tests use the lowest costs, to stay quick).
fn new_key(passphrase: &str) -> Result<Key, String> {
    #[cfg(test)]
    return Key::with_kdf(passphrase, sealed::Kdf::LOWEST);
    #[cfg(not(test))]
    Key::new(passphrase)
}

struct Vault {
    path: PathBuf,
    key: Key,
    bundle: Bundle,
    /// The size and time of the file as last read or written: another
    /// value means another PC (or a sync client) wrote it since.
    seen: Option<(u64, SystemTime)>,
}

static VAULT: Mutex<Option<Vault>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Option<Vault>) -> T) -> T {
    let mut guard = VAULT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Other FeroSSH processes on this PC wait too, so a read, change and
    // write of the vault never interleaves with theirs. (Other PCs, via a
    // sync client, cannot be locked out: there the last save wins.)
    let lock = ProcessLock::take();
    LOCKED.store(
        lock.is_some() || cfg!(not(windows)),
        std::sync::atomic::Ordering::Relaxed,
    );
    f(&mut guard)
}

/// Whether the [`with`] running now holds `vault.lock`: a vault is only
/// written while it does, so two FeroSSH processes never interleave.
static LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// An exclusive open of `vault.lock` in FeroSSH's local folder, which
/// every FeroSSH process takes the same way; released when dropped. None
/// when it cannot be had within five seconds: reads go on, but nothing is
/// written (see [`Vault::save`]) rather than hang.
struct ProcessLock(#[allow(dead_code)] std::fs::File);

impl ProcessLock {
    #[cfg(windows)]
    fn take() -> Option<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = crate::portable::local_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("vault.lock");
        for _ in 0..500 {
            if let Ok(f) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(&path)
            {
                return Some(Self(f));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    #[cfg(not(windows))]
    fn take() -> Option<Self> {
        None
    }
}

fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Writes through a temporary file beside it, so a crash or a full disk
/// never leaves half a vault.
fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A name of this write's own, made new: two writers never share one.
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}-{n}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(text.as_bytes())?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

impl Vault {
    /// Takes in what another PC wrote since we last looked. An error
    /// when the file is there but this key no longer opens it (the
    /// master password was changed elsewhere), so nothing overwrites it.
    fn refresh(&mut self) -> Result<(), String> {
        let now = stamp(&self.path);
        if now.is_none() || now == self.seen {
            return Ok(());
        }
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("The vault {} could not be read ({e}).", self.path.display()))?;
        let content = self.key.open(&text).map_err(|e| match e {
            OpenError::WrongPassphrase => format!(
                "The vault {} was changed with another master password, perhaps on \
                 another PC. Restart FeroSSH to unlock it again; nothing was saved.",
                self.path.display()
            ),
            other => format!("The vault {} cannot be read: {other}", self.path.display()),
        })?;
        self.bundle = Bundle::parse(&content).ok_or_else(|| {
            format!(
                "The vault {} is damaged; nothing was saved.",
                self.path.display()
            )
        })?;
        self.seen = now;
        Ok(())
    }

    fn save(&mut self) -> Result<(), String> {
        if !LOCKED.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(format!(
                "The vault {} is busy in another FeroSSH window; nothing was saved. \
                 Try again in a moment.",
                self.path.display()
            ));
        }
        let text = self.key.seal(&self.bundle.to_bytes())?;
        write_file(&self.path, &text).map_err(|e| {
            format!(
                "The vault {} could not be written ({e}).",
                self.path.display()
            )
        })?;
        self.seen = stamp(&self.path);
        Ok(())
    }
}

/// Whether a vault is unlocked in this FeroSSH.
pub fn is_open() -> bool {
    with(|v| v.is_some())
}

/// Where the unlocked vault is.
pub fn path() -> Option<PathBuf> {
    with(|v| v.as_ref().map(|v| v.path.clone()))
}

/// Why a vault did not unlock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockError {
    /// Worth another try.
    WrongPassphrase,
    /// Not worth one: unreadable, damaged or from a newer FeroSSH.
    Other(String),
}

impl std::fmt::Display for UnlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnlockError::WrongPassphrase => OpenError::WrongPassphrase.fmt(f),
            UnlockError::Other(e) => f.write_str(e),
        }
    }
}

/// Unlocks the vault at `path` with `passphrase`.
pub fn unlock(path: &Path, passphrase: &str) -> Result<(), UnlockError> {
    // The stamp first: a newer file written meanwhile is then read again
    // on the next refresh, never taken for the one read here.
    let seen = stamp(path);
    let text = std::fs::read_to_string(path).map_err(|e| {
        UnlockError::Other(format!(
            "The vault {} could not be read ({e}).",
            path.display()
        ))
    })?;
    let (content, key) = sealed::open(passphrase, &text).map_err(|e| match e {
        OpenError::WrongPassphrase => UnlockError::WrongPassphrase,
        other => UnlockError::Other(format!("{}: {other}", path.display())),
    })?;
    let bundle = Bundle::parse(&content)
        .ok_or_else(|| UnlockError::Other(format!("The vault {} is damaged.", path.display())))?;
    with(|v| {
        *v = Some(Vault {
            path: path.to_path_buf(),
            key,
            bundle,
            seen,
        });
    });
    Ok(())
}

/// Makes a new vault at `path` holding `bundle`, and keeps it unlocked.
pub fn create(path: &Path, passphrase: &str, bundle: Bundle) -> Result<(), String> {
    let mut vault = Vault {
        path: path.to_path_buf(),
        key: new_key(passphrase)?,
        bundle,
        seen: None,
    };
    with(|v| {
        vault.save()?;
        *v = Some(vault);
        Ok(())
    })
}

/// A new master password: a new salt and key, sealed at once.
pub fn change_passphrase(passphrase: &str) -> Result<(), String> {
    let key = new_key(passphrase)?;
    with(|v| {
        let v = v.as_mut().ok_or("No vault is open.")?;
        v.refresh()?;
        let old = std::mem::replace(&mut v.key, key);
        v.save().inspect_err(|_| v.key = old)
    })
}

/// Moves the vault file to `to`; the old file is removed once the new
/// one is written.
pub fn move_to(to: &Path) -> Result<(), String> {
    with(|v| {
        let v = v.as_mut().ok_or("No vault is open.")?;
        v.refresh()?;
        let from = std::mem::replace(&mut v.path, to.to_path_buf());
        if let Err(e) = v.save() {
            v.path = from;
            return Err(e);
        }
        if from != v.path {
            let _ = std::fs::remove_file(&from);
        }
        Ok(())
    })
}

/// A vault closed by [`remove`]: what it held, for writing out as plain
/// files again, and the way to open it again with [`restore`] when that
/// fails.
pub struct Removed(Vault);

impl Removed {
    pub fn bundle(&self) -> &Bundle {
        &self.0.bundle
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }
}

/// Closes the vault and hands it back. The file stays: the caller deletes
/// it once everything is written out (and never a shared one).
pub fn remove() -> Result<Removed, String> {
    with(|v| {
        let open = v.as_mut().ok_or("No vault is open.")?;
        open.refresh()?;
        let vault = v.take().ok_or("No vault is open.")?;
        Ok(Removed(vault))
    })
}

/// Closes the vault as it is, without reading its file again (which may
/// fail): FeroSSH goes back to plain files at once.
pub fn close() {
    with(|v| *v = None);
}

/// Opens a vault [`remove`] closed again, as it was.
pub fn restore(removed: Removed) {
    with(|v| *v = Some(removed.0));
}

/// Whether `passphrase` opens the vault file as it is now.
pub fn verify(passphrase: &str) -> bool {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|text| sealed::open(passphrase, &text).is_ok())
}

/// The text of `name` in the vault; `None` when it has none (or no vault
/// is open).
pub fn read(name: &str) -> Option<String> {
    with(|v| {
        let v = v.as_mut()?;
        let _ = v.refresh();
        v.bundle.file(name).map(str::to_owned)
    })
}

/// Sets `name` to `text` in the vault and seals it to disk.
pub fn write(name: &str, text: &str) -> Result<(), String> {
    with(|v| {
        let v = v.as_mut().ok_or("No vault is open.")?;
        v.refresh()?;
        v.bundle.set_file(name, text.to_owned());
        v.save()
    })
}

/// The password kept under `target`.
pub fn secret(target: &str) -> Option<Secret> {
    with(|v| {
        let v = v.as_mut()?;
        let _ = v.refresh();
        v.bundle.secret(target).cloned()
    })
}

/// Every saved password in the vault.
pub fn secrets() -> Vec<Secret> {
    with(|v| {
        v.as_mut().map_or_else(Vec::new, |v| {
            let _ = v.refresh();
            v.bundle.secrets.clone()
        })
    })
}

pub fn set_secret(target: &str, user: &str, password: &str) -> Result<(), String> {
    with(|v| {
        let v = v.as_mut().ok_or("No vault is open.")?;
        v.refresh()?;
        v.bundle.set_secret(Secret {
            target: target.to_owned(),
            user: user.to_owned(),
            password: Zeroizing::new(password.to_owned()),
        });
        v.save()
    })
}

pub fn delete_secret(target: &str) {
    with(|v| {
        if let Some(v) = v.as_mut()
            && v.refresh().is_ok()
            && v.bundle.secret(target).is_some()
        {
            v.bundle.remove_secret(target);
            let _ = v.save();
        }
    });
}

/// How good a new passphrase is: why not, for one too short or two that
/// differ.
pub fn check_new(passphrase: &str, repeat: &str) -> Result<(), String> {
    if passphrase.chars().count() < 8 {
        return Err(
            "Use at least 8 characters; a few words are easier to remember \
                    and harder to guess."
                .into(),
        );
    }
    if passphrase != repeat {
        return Err("The two passphrases differ.".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn lock() {
    with(|v| *v = None);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// The vault is one per process: the tests share it, one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ferossh-vault-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn create_write_unlock_and_change() {
        let _g = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let d = dir("main");
        let path = d.join(FILE_NAME);
        let mut b = Bundle::default();
        b.set_file("sessions.conf", "Host a\n".into());
        create(&path, "master pass", b).unwrap();
        assert!(is_open());
        assert_eq!(read("sessions.conf").as_deref(), Some("Host a\n"));
        write("ferossh.conf", "TabStrip no\n").unwrap();
        set_secret("FeroSSH:me@a:22", "me", "hunter2").unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("FeroSSH encrypted file v1")
        );
        assert!(!std::fs::read_to_string(&path).unwrap().contains("hunter2"));
        lock();
        assert!(!is_open());
        assert!(unlock(&path, "wrong").is_err());
        unlock(&path, "master pass").unwrap();
        assert_eq!(read("ferossh.conf").as_deref(), Some("TabStrip no\n"));
        assert_eq!(
            secret("FeroSSH:me@a:22").unwrap().password.as_str(),
            "hunter2"
        );
        change_passphrase("new master").unwrap();
        lock();
        assert!(unlock(&path, "master pass").is_err());
        unlock(&path, "new master").unwrap();
        delete_secret("FeroSSH:me@a:22");
        assert!(secret("FeroSSH:me@a:22").is_none());
        let moved = d.join("sync").join("v.vault");
        move_to(&moved).unwrap();
        assert!(!path.exists() && moved.exists());
        assert!(verify("new master") && !verify("master pass"));
        let back = remove().unwrap();
        assert!(back.path() == moved && moved.exists() && !is_open());
        assert_eq!(back.bundle().file("sessions.conf"), Some("Host a\n"));
        restore(back);
        assert!(is_open() && read("sessions.conf").as_deref() == Some("Host a\n"));
        drop(remove().unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn another_pcs_change_is_picked_up() {
        let _g = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let d = dir("sync");
        let path = d.join(FILE_NAME);
        create(&path, "master pass", Bundle::default()).unwrap();
        // "Another PC": the same file, opened on its own, changed.
        let text = std::fs::read_to_string(&path).unwrap();
        let (content, key) = sealed::open("master pass", &text).unwrap();
        let mut theirs = Bundle::parse(&content).unwrap();
        theirs.set_file("sessions.conf", "Host theirs\n".into());
        // A different size, so the change shows even within the
        // file system's time resolution.
        std::fs::write(&path, key.seal(&theirs.to_bytes()).unwrap()).unwrap();
        assert_eq!(read("sessions.conf").as_deref(), Some("Host theirs\n"));
        // Our next write starts from theirs.
        write("ferossh.conf", "x\n").unwrap();
        lock();
        unlock(&path, "master pass").unwrap();
        assert_eq!(read("sessions.conf").as_deref(), Some("Host theirs\n"));
        assert_eq!(read("ferossh.conf").as_deref(), Some("x\n"));
        // Another master password set elsewhere: refused, not overwritten.
        let other = new_key("other pass").unwrap();
        std::fs::write(
            &path,
            other.seal(&Bundle::default().to_bytes()).unwrap() + "\n",
        )
        .unwrap();
        assert!(write("sessions.conf", "Host mine\n").is_err());
        assert!(unlock(&path, "other pass").is_ok());
        lock();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_passphrases() {
        assert!(check_new("short", "short").is_err());
        assert!(check_new("long enough", "long enougj").is_err());
        assert!(check_new("long enough", "long enough").is_ok());
    }
}
