//! Login passwords kept in the Windows Credential Manager, protected by
//! the user's Windows sign-in (DPAPI). Each is a generic credential named
//! `FeroSSH:user@host:port`, kept on this PC only (not roamed), and shown
//! under Windows Credentials in Control Panel, where it can be removed.
//!
//! With a master password vault unlocked ([`crate::vault`]), passwords
//! are kept in the vault instead; ones saved before it are still read
//! from here. A portable FeroSSH never writes here, so a PC it ran on
//! keeps no password of it.

#![allow(unsafe_code)]

use ferossh_config::bundle::{Secret, is_secret_target};
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredEnumerateW,
    CredFree, CredReadW, CredWriteW,
};
use windows::core::{HSTRING, PWSTR, w};
use zeroize::{Zeroize, Zeroizing};

/// The credential's name for a login.
pub fn target(user: &str, host: &str, port: u16) -> String {
    format!("FeroSSH:{user}@{}:{port}", host.to_ascii_lowercase())
}

/// The credential's name for the password of `user` on the proxy at
/// `host`:`port`.
pub fn proxy_target(user: &str, host: &str, port: u16) -> String {
    format!("FeroSSH proxy:{user}@{}:{port}", host.to_ascii_lowercase())
}

/// The password kept under `target`; `Ok(None)` when there is none.
pub fn read(target: &str) -> Result<Option<Zeroizing<String>>, String> {
    if let Some(s) = crate::vault::secret(target) {
        return Ok(Some(s.password));
    }
    os_read(target)
}

/// Keeps `password` under `target`, replacing what was there: in the
/// vault when one is unlocked, else in the Credential Manager (never on
/// a portable FeroSSH).
pub fn write(target: &str, user: &str, password: &str) -> Result<(), String> {
    if crate::vault::is_open() {
        return crate::vault::set_secret(target, user, password);
    }
    if crate::portable::is_portable() {
        return Err(
            "A portable FeroSSH keeps passwords only in a master password vault \
                    (File > Master password), so none is left on this PC."
                .into(),
        );
    }
    os_write(target, user, password)
}

/// Forgets the password under `target`, in the vault and here.
pub fn delete(target: &str) {
    crate::vault::delete_secret(target);
    os_delete(target);
}

/// The password under `target` in the Credential Manager alone.
pub fn os_read(target: &str) -> Result<Option<Zeroizing<String>>, String> {
    let name = HSTRING::from(target);
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: valid name; on success `cred` points at a block the system
    // allocated, read here and freed once with CredFree.
    unsafe {
        if let Err(e) = CredReadW(&name, CRED_TYPE_GENERIC, None, &mut cred) {
            return if e.code() == ERROR_NOT_FOUND.to_hresult() {
                Ok(None)
            } else {
                Err(format!(
                    "The Windows Credential Manager could not be read ({e})."
                ))
            };
        }
        let c = &*cred;
        let len = c.CredentialBlobSize as usize;
        let mut blob = Zeroizing::new(if c.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, len).to_vec()
        });
        CredFree(cred as *const _);
        // UTF-16 little-endian, as Windows itself stores passwords.
        let mut units: Vec<u16> = blob
            .chunks_exact(2)
            .filter_map(|b| <[u8; 2]>::try_from(b).ok().map(u16::from_le_bytes))
            .collect();
        blob.zeroize();
        let text = Zeroizing::new(String::from_utf16_lossy(&units));
        units.zeroize();
        Ok(Some(text))
    }
}

/// Keeps `password` under `target` in the Credential Manager.
pub fn os_write(target: &str, user: &str, password: &str) -> Result<(), String> {
    let mut name: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    let mut user: Vec<u16> = user.encode_utf16().chain(Some(0)).collect();
    let mut blob = Zeroizing::new(
        password
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<u8>>(),
    );
    let cred = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(name.as_mut_ptr()),
        CredentialBlobSize: u32::try_from(blob.len()).map_err(|_| "The password is too long.")?,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    // SAFETY: every pointer in `cred` points at a buffer that outlives the
    // call; the system copies them.
    unsafe { CredWriteW(&cred, 0) }.map_err(|e| {
        format!("The password could not be saved in the Windows Credential Manager ({e}).")
    })
}

/// Forgets the password under `target` in the Credential Manager.
pub fn os_delete(target: &str) {
    let name = HSTRING::from(target);
    // SAFETY: valid name; a missing credential is not an error worth
    // reporting.
    let _ = unsafe { CredDeleteW(&name, CRED_TYPE_GENERIC, None) };
}

/// Every password FeroSSH saved in the Credential Manager (logins,
/// proxies and named secrets), for an export or a move into the vault.
pub fn os_list() -> Vec<Secret> {
    let mut count = 0u32;
    let mut creds: *mut *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: on success `creds` points at `count` credential pointers
    // the system allocated, read here and freed once with CredFree.
    unsafe {
        if CredEnumerateW(w!("FeroSSH*"), None, &mut count, &mut creds).is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 0..count as usize {
            let c = &**creds.add(i);
            if c.Type != CRED_TYPE_GENERIC || c.TargetName.is_null() {
                continue;
            }
            let Ok(target) = c.TargetName.to_string() else {
                continue;
            };
            if !is_secret_target(&target) {
                continue;
            }
            let user = if c.UserName.is_null() {
                String::new()
            } else {
                c.UserName.to_string().unwrap_or_default()
            };
            let mut blob = Zeroizing::new(if c.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
            });
            let mut units: Vec<u16> = blob
                .chunks_exact(2)
                .filter_map(|b| <[u8; 2]>::try_from(b).ok().map(u16::from_le_bytes))
                .collect();
            blob.zeroize();
            let password = Zeroizing::new(String::from_utf16_lossy(&units));
            units.zeroize();
            out.push(Secret {
                target,
                user,
                password,
            });
        }
        CredFree(creds as *const _);
        out
    }
}
