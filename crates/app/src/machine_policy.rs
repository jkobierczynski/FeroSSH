//! Reads the machine policy under `HKLM\Software\Policies\FeroSSH` (see
//! [`ferossh_config::team_policy`]). Only `HKEY_LOCAL_MACHINE` is read:
//! a user can write their own `HKEY_CURRENT_USER`, and neither the
//! settings file, a portable `FeroSSH-data` folder nor an environment
//! variable changes what is read here.

use std::sync::{Mutex, PoisonError};

use ferossh_config::log_policy::LogPolicy;
use ferossh_config::team_policy::TeamPolicy;

/// The policy as read last; `None` inside until first read.
static POLICY: Mutex<Option<Option<TeamPolicy>>> = Mutex::new(None);

/// The team library policy, read once and then kept until [`reload`].
pub fn team() -> Option<TeamPolicy> {
    let mut p = POLICY.lock().unwrap_or_else(PoisonError::into_inner);
    p.get_or_insert_with(read).clone()
}

/// Reads the policy again (Group Policy may have changed it since), and
/// returns it.
pub fn reload() -> Option<TeamPolicy> {
    let fresh = read();
    *POLICY.lock().unwrap_or_else(PoisonError::into_inner) = Some(fresh.clone());
    *LOG.lock().unwrap_or_else(PoisonError::into_inner) = Some(read_log());
    fresh
}

/// The connection log policy as read last.
static LOG: Mutex<Option<LogPolicy>> = Mutex::new(None);

/// What machine policy says about the connection log, read once and then
/// kept until [`reload`].
pub fn log() -> LogPolicy {
    *LOG.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(read_log)
}

#[cfg(not(windows))]
fn read_log() -> LogPolicy {
    LogPolicy::default()
}

#[cfg(windows)]
fn read_log() -> LogPolicy {
    use ferossh_config::log_policy::{DELETE, EVENT_LOG};
    use ferossh_config::team_policy::KEY;
    let Some(key) = reg::Key::open_hklm(KEY) else {
        return LogPolicy::default();
    };
    LogPolicy::from_values(key.dword(DELETE), key.dword(EVENT_LOG))
}

/// The team library to read and the keys to check it with: the policy's
/// while one is set, else the user's own settings.
pub fn team_source(own: Option<&str>, own_keys: &[String]) -> (Option<String>, Vec<String>) {
    match team() {
        Some(p) => (p.library, p.keys),
        None => (own.map(str::to_owned), own_keys.to_vec()),
    }
}

#[cfg(not(windows))]
fn read() -> Option<TeamPolicy> {
    None
}

#[cfg(windows)]
fn read() -> Option<TeamPolicy> {
    use ferossh_config::team_policy::{KEY, KEYS, LIBRARY, ONLY_TEAM, REQUIRE};
    let key = reg::Key::open_hklm(KEY)?;
    let mut keys = key.multi(KEYS);
    if let Some(list) = reg::Key::open_hklm(&format!(r"{KEY}\{KEYS}")) {
        keys.extend(list.texts());
    }
    TeamPolicy::from_values(
        key.text(LIBRARY),
        keys,
        key.dword(REQUIRE),
        key.dword(ONLY_TEAM),
    )
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) mod reg {
    //! The few registry reads the policy needs. Each unsafe block is a
    //! direct, documented call.

    use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_SZ, REG_VALUE_TYPE, RRF_NOEXPAND,
        RRF_RT_REG_DWORD, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ, RegCloseKey, RegEnumValueW,
        RegGetValueW, RegOpenKeyExW,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    /// The longest value read, in UTF-16 units: a few RSA keys' lines.
    const MAX_TEXT: usize = 32 * 1024;

    /// The most values of a list read.
    const MAX_VALUES: u32 = 256;

    #[derive(Debug)]
    pub struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: a key this value opened, closed once.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn text_of(buf: &[u16], bytes: u32) -> &[u16] {
        let units = (bytes as usize / 2).min(buf.len());
        buf.get(..units).unwrap_or_default()
    }

    impl Key {
        /// `path` under `HKEY_LOCAL_MACHINE`, in the 64-bit view Group
        /// Policy writes.
        pub fn open_hklm(path: &str) -> Option<Self> {
            let mut key = HKEY::default();
            // SAFETY: a valid string and a local out-pointer.
            let status = unsafe {
                RegOpenKeyExW(
                    HKEY_LOCAL_MACHINE,
                    &HSTRING::from(path),
                    None,
                    KEY_READ | KEY_WOW64_64KEY,
                    &mut key,
                )
            };
            (status == ERROR_SUCCESS).then_some(Self(key))
        }

        /// Raw value `name` of the types in `flags`, as UTF-16 units.
        fn raw(&self, name: &str, flags: u32) -> Option<Vec<u16>> {
            let mut buf = vec![0u16; MAX_TEXT];
            let mut size = (buf.len() * 2) as u32;
            // SAFETY: an open key, a valid string and a buffer of `size`
            // bytes. No expansion: %VARS% are the user's to set.
            let status = unsafe {
                RegGetValueW(
                    self.0,
                    PCWSTR::null(),
                    &HSTRING::from(name),
                    windows::Win32::System::Registry::REG_ROUTINE_FLAGS(flags | RRF_NOEXPAND.0),
                    None,
                    Some(buf.as_mut_ptr().cast()),
                    Some(&mut size),
                )
            };
            (status == ERROR_SUCCESS).then(|| text_of(&buf, size).to_vec())
        }

        /// REG_SZ value `name`.
        pub fn text(&self, name: &str) -> Option<String> {
            let units = self.raw(name, RRF_RT_REG_SZ.0)?;
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            Some(String::from_utf16_lossy(units.get(..end)?))
        }

        /// REG_MULTI_SZ value `name`, a string each (or a REG_SZ value,
        /// one string); empty when it is not there.
        pub fn multi(&self, name: &str) -> Vec<String> {
            if let Some(t) = self.text(name) {
                return vec![t];
            }
            let Some(units) = self.raw(name, RRF_RT_REG_MULTI_SZ.0) else {
                return Vec::new();
            };
            units
                .split(|&u| u == 0)
                .filter(|s| !s.is_empty())
                .map(String::from_utf16_lossy)
                .collect()
        }

        /// DWORD value `name`.
        pub fn dword(&self, name: &str) -> Option<u32> {
            let mut value = 0u32;
            let mut size = size_of::<u32>() as u32;
            // SAFETY: an open key and a DWORD-sized buffer.
            let status = unsafe {
                RegGetValueW(
                    self.0,
                    PCWSTR::null(),
                    &HSTRING::from(name),
                    RRF_RT_REG_DWORD,
                    None,
                    Some((&raw mut value).cast()),
                    Some(&mut size),
                )
            };
            (status == ERROR_SUCCESS).then_some(value)
        }

        /// Every REG_SZ value of the key, whatever its name (an ADMX
        /// list names them 1, 2, 3...).
        pub fn texts(&self) -> Vec<String> {
            let mut out = Vec::new();
            for index in 0..MAX_VALUES {
                let mut name = [0u16; 256];
                let mut name_len = name.len() as u32;
                let mut kind = REG_VALUE_TYPE::default();
                let mut buf = vec![0u8; MAX_TEXT * 2];
                let mut size = buf.len() as u32;
                // SAFETY: an open key, a name buffer of `name_len` units
                // and a data buffer of `size` bytes.
                let status = unsafe {
                    RegEnumValueW(
                        self.0,
                        index,
                        Some(PWSTR(name.as_mut_ptr())),
                        &mut name_len,
                        None,
                        Some((&raw mut kind).cast()),
                        Some(buf.as_mut_ptr()),
                        Some(&mut size),
                    )
                };
                if status == ERROR_NO_MORE_ITEMS {
                    break;
                }
                // One too big for the buffer is left out.
                if status != ERROR_SUCCESS || kind != REG_SZ {
                    continue;
                }
                let units: Vec<u16> = buf
                    .get(..size as usize)
                    .unwrap_or_default()
                    .chunks_exact(2)
                    .map(|c| match c {
                        [lo, hi] => u16::from_le_bytes([*lo, *hi]),
                        _ => 0,
                    })
                    .take_while(|&u| u != 0)
                    .collect();
                out.push(String::from_utf16_lossy(&units));
            }
            out
        }
    }
}
