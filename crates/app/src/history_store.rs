//! Command history on disk: one file per host in
//! `%LOCALAPPDATA%\FeroSSH\history` (on this PC only, never roamed),
//! encrypted with DPAPI under the user's Windows sign-in, so another
//! account on the PC, or the file copied elsewhere, reads nothing. The
//! file name is a hash of the host, so a listing does not name hosts.

#![allow(unsafe_code)]

use std::path::PathBuf;

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

/// The folder the histories are kept in.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("FeroSSH").join("history"))
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

/// The history kept for `key`; an empty one when there is none, or it
/// cannot be read (another user's, damaged).
pub fn load(key: &str) -> History {
    let Some(path) = file(key) else {
        return History::new(key);
    };
    std::fs::read(path)
        .ok()
        .and_then(|blob| unprotect(&blob))
        .and_then(|plain| {
            let text = std::str::from_utf8(&plain).ok()?;
            History::parse(text, key)
        })
        .unwrap_or_else(|| History::new(key))
}

/// Writes `history` (to a new file, then over the old one).
pub fn save(history: &History) -> Result<(), String> {
    let path = file(&history.key).ok_or("LOCALAPPDATA is not set")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let plain = Zeroizing::new(history.to_text().into_bytes());
    let blob = protect(&plain)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, blob).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Deletes the history kept for `key`.
pub fn clear(key: &str) {
    if let Some(path) = file(key) {
        let _ = std::fs::remove_file(path);
    }
}

/// Deletes every host's history.
pub fn clear_all() {
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
