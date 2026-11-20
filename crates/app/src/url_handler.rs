//! Settings > Open links: lets `ssh://` and `sftp://` links (in a
//! browser, a wiki, an inventory) open in FeroSSH. FeroSSH is one exe
//! with nothing to install, so this is opt-in and per user: it writes
//! only under `HKEY_CURRENT_USER`, and turning it off takes out what it
//! wrote.
//!
//! Windows starts `"<exe>" --url "%1"` for a link. `--url` takes exactly
//! one argument, which must be a valid `ssh://` or `sftp://` address
//! ([`ferossh_config::ssh_config::ssh_url`], [`sftp_url`]): a link that
//! breaks out of its quotes to add options is refused, never run.
//!
//! When another program already handles links of a kind, FeroSSH does not
//! take them over: it registers itself as able to open them (so it shows
//! in Windows' *Default apps*), and says where to choose it.
//!
//! [`sftp_url`]: ferossh_config::ssh_config::sftp_url

/// The kinds of link FeroSSH can open.
pub const SCHEMES: [&str; 2] = ["ssh", "sftp"];

/// The command line Windows runs for a link.
pub fn command(exe: &str) -> String {
    format!("\"{exe}\" --url \"%1\"")
}

/// Whether `link` is one FeroSSH opens from the command line: an
/// `ssh://` or `sftp://` address that parses.
pub fn valid_link(link: &str) -> bool {
    use ferossh_config::ssh_config::{sftp_url, ssh_url};
    ssh_url(link).is_some() || sftp_url(link).is_some()
}

/// The ProgID FeroSSH registers for `scheme`.
pub fn prog_id(scheme: &str) -> String {
    format!("FeroSSH.{scheme}")
}

/// What turning a kind of link on did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registered {
    /// Links of this kind open in FeroSSH now.
    Done,
    /// Another program handles them: FeroSSH is offered in Default apps,
    /// where the user picks it. The other program's command, if known.
    ChooseInSettings(Option<String>),
}

#[cfg(windows)]
pub use reg::{register, registered, unregister};

#[cfg(windows)]
mod reg {
    // Registry calls are FFI; each unsafe block is a direct, documented
    // call.
    #![allow(unsafe_code)]

    use std::ffi::c_void;

    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW,
        RegDeleteKeyW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
    };
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    use windows::core::HSTRING;

    use super::{Registered, SCHEMES, command, prog_id};

    const CLASSES: &str = "Software\\Classes";
    const CAPABILITIES: &str = "Software\\FeroSSH\\Capabilities";
    const APP_KEY: &str = "Software\\FeroSSH";
    const REGISTERED_APPS: &str = "Software\\RegisteredApplications";
    /// A value FeroSSH puts on a scheme key it wrote, so it only ever
    /// deletes its own.
    const MARK: &str = "FeroSSH";

    fn exe() -> Result<String, String> {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("Could not find FeroSSH's own exe: {e}"))
    }

    fn read(root: HKEY, key: &str, name: &str) -> Option<String> {
        let mut buf = vec![0u16; 2048];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: valid strings and a buffer of `size` bytes.
        let status = unsafe {
            RegGetValueW(
                root,
                &HSTRING::from(key),
                &HSTRING::from(name),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast::<c_void>()),
                Some(&mut size),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let units = (size as usize / 2).min(buf.len());
        let text = buf.get(..units)?;
        let end = text.iter().position(|&u| u == 0).unwrap_or(text.len());
        Some(String::from_utf16_lossy(text.get(..end)?))
    }

    fn write(key: &str, name: &str, value: &str) -> Result<(), String> {
        let data: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        // SAFETY: valid strings; `data` holds `len * 2` bytes.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(name),
                REG_SZ.0,
                Some(data.as_ptr().cast::<c_void>()),
                (data.len() * 2) as u32,
            )
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "Could not write HKEY_CURRENT_USER\\{key} ({:?}).",
                status
            ))
        }
    }

    fn delete_tree(key: &str) {
        // SAFETY: a valid string; a missing key is no error worth telling.
        // RegDeleteTreeW takes the key with its subkeys; RegDeleteKeyW
        // covers a key it left empty.
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key));
        }
    }

    fn delete_value(key: &str, name: &str) {
        // SAFETY: valid strings; a missing value is no error worth telling.
        unsafe {
            let _ =
                RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name));
        }
    }

    fn changed() {
        // SAFETY: no item pointers for SHCNE_ASSOCCHANGED.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }

    /// Writes a key that opens links in this exe: the ProgID, or the
    /// scheme's own key.
    fn write_handler(key: &str, description: &str, exe: &str) -> Result<(), String> {
        write(key, "", description)?;
        write(key, "URL Protocol", "")?;
        write(&format!("{key}\\DefaultIcon"), "", &format!("\"{exe}\",0"))?;
        write(&format!("{key}\\shell\\open\\command"), "", &command(exe))
    }

    /// Whether links of kind `scheme` open in this exe through FeroSSH's
    /// registration.
    pub fn registered(scheme: &str) -> bool {
        let Ok(exe) = exe() else {
            return false;
        };
        let prog = prog_id(scheme);
        read(
            HKEY_CURRENT_USER,
            &format!("{CAPABILITIES}\\URLAssociations"),
            scheme,
        )
        .is_some_and(|p| p == prog)
            && read(
                HKEY_CURRENT_USER,
                &format!("{CLASSES}\\{prog}\\shell\\open\\command"),
                "",
            )
            .is_some_and(|c| c.eq_ignore_ascii_case(&command(&exe)))
    }

    /// Lets links of kind `scheme` open in this exe.
    pub fn register(scheme: &str) -> Result<Registered, String> {
        let exe = exe()?;
        let prog = prog_id(scheme);
        let upper = scheme.to_ascii_uppercase();
        write_handler(
            &format!("{CLASSES}\\{prog}"),
            &format!("{upper} link (FeroSSH)"),
            &exe,
        )?;
        write(CAPABILITIES, "ApplicationName", "FeroSSH")?;
        write(
            CAPABILITIES,
            "ApplicationDescription",
            "Secure connections. Forged in Rust.",
        )?;
        write(&format!("{CAPABILITIES}\\URLAssociations"), scheme, &prog)?;
        write(REGISTERED_APPS, "FeroSSH", CAPABILITIES)?;

        let own = format!("{CLASSES}\\{scheme}");
        let ours = read(HKEY_CURRENT_USER, &own, MARK).is_some();
        // What handles the links now, for this user or the whole PC.
        let other = read(
            HKEY_CLASSES_ROOT,
            &format!("{scheme}\\shell\\open\\command"),
            "",
        )
        .filter(|c| !c.trim().is_empty());
        // A choice made in Default apps wins over both.
        let choice = read(
            HKEY_CURRENT_USER,
            &format!(
                "Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\{scheme}\\UserChoice"
            ),
            "ProgId",
        )
        .filter(|p| *p != prog);
        let result = if ours || other.is_none() {
            write_handler(&own, &format!("URL:{upper} link"), &exe)?;
            write(&own, MARK, "1")?;
            if choice.is_some() {
                Registered::ChooseInSettings(None)
            } else {
                Registered::Done
            }
        } else {
            Registered::ChooseInSettings(other)
        };
        changed();
        Ok(result)
    }

    /// Takes out what [`register`] wrote for `scheme`; the rest of
    /// FeroSSH's registration goes with the last kind.
    pub fn unregister(scheme: &str) {
        let own = format!("{CLASSES}\\{scheme}");
        if read(HKEY_CURRENT_USER, &own, MARK).is_some() {
            delete_tree(&own);
        }
        delete_tree(&format!("{CLASSES}\\{}", prog_id(scheme)));
        delete_value(&format!("{CAPABILITIES}\\URLAssociations"), scheme);
        let any = SCHEMES.iter().any(|s| {
            read(
                HKEY_CURRENT_USER,
                &format!("{CAPABILITIES}\\URLAssociations"),
                s,
            )
            .is_some()
        });
        if !any {
            delete_value(REGISTERED_APPS, "FeroSSH");
            delete_tree(APP_KEY);
        }
        changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_quotes_the_link() {
        assert_eq!(
            command("C:\\Tools\\ferossh.exe"),
            "\"C:\\Tools\\ferossh.exe\" --url \"%1\""
        );
        assert_eq!(prog_id("ssh"), "FeroSSH.ssh");
    }

    #[test]
    fn only_addresses_are_links() {
        assert!(valid_link("ssh://admin@web1.lan:2222"));
        assert!(valid_link("sftp://up@files.lan/incoming"));
        assert!(!valid_link("telnet://h"));
        assert!(!valid_link("ssh://h --no-sandbox"));
        assert!(!valid_link("ssh://-oProxyCommand=calc"));
        assert!(!valid_link("--local"));
        assert!(!valid_link("web1.lan"));
    }
}
