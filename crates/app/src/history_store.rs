//! Command history on disk: one file per host in
//! `%LOCALAPPDATA%\FeroSSH\history` (on this PC only, never roamed; in
//! the data folder when FeroSSH is portable),
//! encrypted with DPAPI under the user's Windows sign-in, so another
//! account on the PC, or the file copied elsewhere, reads nothing. The
//! file name is a hash of the host, so a listing does not name hosts.

#![allow(unsafe_code)]

use std::path::PathBuf;
use std::time::SystemTime;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::w;
use zeroize::Zeroizing;

use crate::suggest::History;

/// Mixed into the encryption, so other programs' DPAPI blobs and these do
/// not open each other.
const ENTROPY: &[u8] = b"FeroSSH command history";

/// The folder the histories are kept in. A portable FeroSSH keeps one
/// per PC and Windows account: each file is sealed for one account on one
/// PC (DPAPI), so another PC could not read it and would replace it.
pub fn dir() -> Option<PathBuf> {
    let dir = crate::portable::local_dir()?.join("history");
    if !crate::portable::is_portable() {
        return Some(dir);
    }
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let who: String = format!("{}-{}", var("COMPUTERNAME"), var("USERNAME"))
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(dir.join(who))
}

fn file(key: &str) -> Option<PathBuf> {
    // FNV-1a, 64 bits: a name, not a secret (the host is checked inside).
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    Some(dir()?.join(format!("{h:016x}.dat")))
}

/// When the history file of `key` last changed; `None` when there is none.
/// Another FeroSSH window may change or delete it.
pub fn modified(key: &str) -> Option<SystemTime> {
    std::fs::metadata(file(key)?).ok()?.modified().ok()
}

/// The history kept for `key`, with the file's time; an empty one when
/// there is none, or it cannot be read (another user's, damaged).
pub fn load(key: &str) -> (History, Option<SystemTime>) {
    let stamp = modified(key);
    let history = file(key)
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|blob| unprotect(&blob))
        .and_then(|plain| {
            let text = std::str::from_utf8(&plain).ok()?;
            History::parse(text, key)
        })
        .unwrap_or_else(|| History::new(key));
    (history, stamp)
}

/// A history with its file's time when read.
pub type Stamped = (History, Option<SystemTime>);

/// Held while a history is read, changed and written, or histories are
/// deleted: an exclusive open of `history.lock`, which every FeroSSH
/// process takes the same way, so their changes and clears do not
/// interleave. Released when dropped.
struct Lock(#[allow(dead_code)] std::fs::File);

fn lock() -> Result<Lock, String> {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = dir().ok_or("LOCALAPPDATA is not set")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("history.lock");
    let mut last = String::new();
    // Another window holds it for a few milliseconds at most.
    for _ in 0..100 {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
        {
            Ok(f) => return Ok(Lock(f)),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err(format!("The command history is busy ({last})."))
}

/// Reads the history of `key` from disk as it is now, applies `change`,
/// and writes it back when `change` changed it, all under [`lock`]: so two
/// windows typing on the same host keep each other's commands, and a clear
/// in one holds. On an error the history comes back as changed but not
/// saved, with the file's time from before.
pub fn update(
    key: &str,
    change: impl FnOnce(&mut History) -> bool,
) -> Result<Stamped, (Stamped, String)> {
    let guard = lock();
    let (mut history, stamp) = load(key);
    if !change(&mut history) {
        return Ok((history, stamp));
    }
    let saved = guard.and_then(|_guard| save(&history));
    match saved {
        Ok(()) => {
            let stamp = modified(key);
            Ok((history, stamp))
        }
        Err(e) => Err(((history, stamp), e)),
    }
}

/// Writes `history` (to a file of this process's own, then over the old
/// one).
fn save(history: &History) -> Result<(), String> {
    let path = file(&history.key).ok_or("LOCALAPPDATA is not set")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let plain = Zeroizing::new(history.to_text().into_bytes());
    let blob = protect(&plain)?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, blob).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Deletes the history kept for `key`.
pub fn clear(key: &str) {
    let _guard = lock();
    if let Some(path) = file(key) {
        let _ = std::fs::remove_file(path);
    }
}

/// Deletes every host's history.
pub fn clear_all() {
    let _guard = lock();
    let Some(dir) = dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("dat") || x.eq_ignore_ascii_case("tmp"))
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
    }
}

/// Takes the bytes out of a blob DPAPI allocated, and frees it.
///
/// # Safety
/// `out` must come from a successful CryptProtectData or
/// CryptUnprotectData call.
unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: per the contract, `pbData` holds `cbData` bytes from
    // LocalAlloc, freed once here.
    unsafe {
        let bytes = if out.pbData.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec()
        };
        if !out.pbData.is_null() {
            // Clear the plain text before giving the memory back.
            std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
            let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        }
        bytes
    }
}

fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
    let input = blob(plain);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: the input blobs point at live slices for the call; `out` is
    // filled by the system and taken (and freed) once.
    unsafe {
        CryptProtectData(
            &input,
            w!("FeroSSH command history"),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| format!("The history could not be encrypted ({e})."))?;
        Ok(take(out))
    }
}

fn unprotect(sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let input = blob(sealed);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .ok()?;
        Some(Zeroizing::new(take(out)))
    }
}
