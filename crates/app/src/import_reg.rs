//! Reads the saved sessions of PuTTY, KiTTY and WinSCP from the registry
//! for an import; [`ferossh_config::putty`] and [`ferossh_config::winscp`]
//! turn them into sessions. Stored passwords are only checked for being
//! there, never read.

// Registry calls are FFI; each unsafe block is a direct, documented call.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::c_void;

use ferossh_config::import::{Client, Session, Value};
use ferossh_config::{putty, winscp};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_SZ, REG_VALUE_TYPE, RRF_RT_ANY,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

/// Longest value read, in UTF-16 units; PuTTY's own limit is lower.
const MAX_TEXT: usize = 4096;

fn read_value(key: HKEY, session: &str, name: &str) -> Option<Value> {
    let mut kind = REG_VALUE_TYPE::default();
    let mut buf = vec![0u16; MAX_TEXT];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: an open key, valid strings and a buffer of `size` bytes.
    let status = unsafe {
        RegGetValueW(
            key,
            &HSTRING::from(session),
            &HSTRING::from(name),
            RRF_RT_REG_SZ | RRF_RT_REG_DWORD,
            Some(&mut kind),
            Some(buf.as_mut_ptr().cast::<c_void>()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    if kind == REG_DWORD {
        let lo = u32::from(*buf.first()?);
        let hi = u32::from(*buf.get(1)?);
        return Some(Value::Number(lo | (hi << 16)));
    }
    if kind == REG_SZ {
        let units = (size as usize / 2).min(buf.len());
        let text = buf.get(..units)?;
        let end = text.iter().position(|&u| u == 0).unwrap_or(text.len());
        return Some(Value::Text(String::from_utf16_lossy(text.get(..end)?)));
    }
    None
}

/// Whether the value is there, without reading it.
fn exists(key: HKEY, session: &str, name: &str) -> bool {
    // SAFETY: an open key and valid strings; no data is asked for.
    let status = unsafe {
        RegGetValueW(
            key,
            &HSTRING::from(session),
            &HSTRING::from(name),
            RRF_RT_ANY,
            None,
            None,
            None,
        )
    };
    status == ERROR_SUCCESS
}

/// A client's SSH sessions in the registry; empty if it saved none.
pub fn sessions(client: Client) -> Vec<Session> {
    let (path, values, build): (PCWSTR, &[&str], Builder) = match client {
        Client::Putty => (
            w!("Software\\SimonTatham\\PuTTY\\Sessions"),
            &putty::VALUES,
            putty_session,
        ),
        Client::Kitty => (
            w!("Software\\9bis.com\\KiTTY\\Sessions"),
            &putty::VALUES,
            putty_session,
        ),
        Client::WinScp => (
            w!("Software\\Martin Prikryl\\WinSCP 2\\Sessions"),
            &winscp::VALUES,
            winscp_session,
        ),
    };
    read(path, values, build)
}

type Builder = fn(&str, &HashMap<&str, Value>) -> Option<Session>;

fn putty_session(raw: &str, values: &HashMap<&str, Value>) -> Option<Session> {
    putty::session(raw, |k| values.get(k).cloned())
}

fn winscp_session(raw: &str, values: &HashMap<&str, Value>) -> Option<Session> {
    winscp::session(raw, |k| values.get(k).cloned())
}

fn read(path: PCWSTR, wanted: &[&str], build: Builder) -> Vec<Session> {
    let mut key = HKEY::default();
    // SAFETY: a constant path and a local out-pointer.
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path, None, KEY_READ, &mut key) };
    if status != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut out = Vec::new();
    for index in 0..10_000u32 {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: an open key and a buffer of `len` units.
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        let Some(raw) = name.get(..len as usize).map(String::from_utf16_lossy) else {
            continue;
        };
        let values: HashMap<&str, Value> = wanted
            .iter()
            .filter_map(|v| {
                if v.starts_with("Password") {
                    exists(key, &raw, v).then_some((*v, Value::Number(1)))
                } else {
                    Some((*v, read_value(key, &raw, v)?))
                }
            })
            .collect();
        if let Some(s) = build(&raw, &values) {
            out.push(s);
        }
    }
    // SAFETY: the key opened above.
    let _ = unsafe { RegCloseKey(key) };
    out
}
