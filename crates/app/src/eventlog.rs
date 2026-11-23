//! Copies connection log entries to the Windows Application event log,
//! source `FeroSSH`, when machine policy asks for it
//! ([`ferossh_config::log_policy`]). A user who is not an administrator
//! can add to that log but not clear or change it, and Windows Event
//! Forwarding or a SIEM agent can collect it.
//!
//! Each event carries the user's account (its SID, from the process
//! token) and one text with the entry's fields. The event IDs:
//!
//! | ID   | Type        | What                 |
//! |------|-------------|----------------------|
//! | 1001 | Information | Connected            |
//! | 1002 | Information | Disconnected         |
//! | 1003 | Warning     | Lost (reconnecting)  |
//! | 1004 | Warning     | Failed               |
//! | 1005 | Information | Opened (RDP, VNC)    |
//! | 1010 | Warning     | Entries deleted      |
//!
//! Event Viewer shows the text as is once the source is registered
//! (`New-EventLog -LogName Application -Source FeroSSH`, see
//! `docs/admin/connection-log-policy.md`); unregistered, it shows the same
//! text after a note that the description is missing.

use ferossh_config::connlog::{Entry, Kind};

/// The event's ID.
pub fn event_id(kind: Kind) -> u32 {
    match kind {
        Kind::Connected => 1001,
        Kind::Disconnected => 1002,
        Kind::Lost => 1003,
        Kind::Failed => 1004,
        Kind::Opened => 1005,
    }
}

/// The ID of "entries deleted".
pub const DELETED_ID: u32 = 1010;

/// The event's text.
pub fn text(e: &Entry) -> String {
    let e = e.cleaned();
    let mut out = format!(
        "FeroSSH connection {}.\r\nConnection: {}\r\nAddress: {}\r\nProtocol: {}",
        e.kind.word(),
        e.name,
        e.address,
        e.protocol
    );
    if !e.via.is_empty() {
        out.push_str(&format!("\r\nVia: {}", e.via));
    }
    if !e.host_key.is_empty() {
        out.push_str(&format!("\r\nHost key: {}", e.host_key));
    }
    if !e.details.is_empty() {
        out.push_str(&format!("\r\nDetails: {}", e.details));
    }
    out
}

/// Writes `e` to the event log; failures are not reported (the file
/// still has the entry).
pub fn report(e: &Entry) {
    let warning = matches!(e.kind, Kind::Lost | Kind::Failed);
    imp::write(event_id(e.kind), warning, &text(e));
}

/// Says in the event log that `n` entries were deleted from the file.
pub fn report_deleted(n: usize) {
    imp::write(
        DELETED_ID,
        true,
        &format!(
            "FeroSSH connection log: the user deleted {n} entr{} from the log file.",
            if n == 1 { "y" } else { "ies" }
        ),
    );
}

#[cfg(not(windows))]
mod imp {
    pub fn write(_id: u32, _warning: bool, _text: &str) {}
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::EventLog::{
        DeregisterEventSource, EVENTLOG_INFORMATION_TYPE, EVENTLOG_WARNING_TYPE,
        RegisterEventSourceW, ReportEventW,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{HSTRING, PCWSTR, w};

    /// The process token's user, as the bytes of a TOKEN_USER (its SID
    /// points inside them).
    fn token_user() -> Option<Vec<u64>> {
        // SAFETY: a token of our own process, closed below; the buffer is
        // sized as GetTokenInformation asked and u64-aligned.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
            let mut len = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            let got = GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                len,
                &mut len,
            );
            let _ = CloseHandle(token);
            got.ok()?;
            Some(buf)
        }
    }

    pub fn write(id: u32, warning: bool, text: &str) {
        let user = token_user();
        // SAFETY: the source handle is ours and closed below; the string
        // and the SID (inside `user`) live through the call.
        unsafe {
            let Ok(source) = RegisterEventSourceW(PCWSTR::null(), w!("FeroSSH")) else {
                return;
            };
            let sid = user
                .as_ref()
                .map(|u| (*u.as_ptr().cast::<TOKEN_USER>()).User.Sid)
                .filter(|s: &PSID| !s.is_invalid());
            let text = HSTRING::from(text);
            let strings = [PCWSTR(text.as_ptr())];
            let _ = ReportEventW(
                source,
                if warning {
                    EVENTLOG_WARNING_TYPE
                } else {
                    EVENTLOG_INFORMATION_TYPE
                },
                0,
                id,
                sid,
                0,
                Some(&strings),
                None,
            );
            let _ = DeregisterEventSource(source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_has_the_fields_and_no_forged_lines() {
        let e = Entry {
            time: 1,
            kind: Kind::Failed,
            protocol: "ssh".into(),
            name: "web".into(),
            address: "root@web:22".into(),
            via: "bastion".into(),
            host_key: String::new(),
            details: "refused\r\nConnection: forged".into(),
            target: String::new(),
        };
        let t = text(&e);
        assert!(t.starts_with("FeroSSH connection failed.\r\nConnection: web\r\n"));
        assert!(t.contains("\r\nVia: bastion"));
        assert!(!t.contains("Host key"));
        assert_eq!(t.matches("Connection:").count(), 2);
        assert!(t.ends_with("Details: refused Connection: forged"));
        assert_eq!(event_id(Kind::Failed), 1004);
    }
}
