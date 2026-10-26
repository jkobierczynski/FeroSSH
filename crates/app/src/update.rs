//! Help > Check for updates: fetches the latest release's signed update
//! file over HTTPS with WinHTTP and checks it with `ferossh-update`. Only
//! when the user asks, or once a week when they turned that on; nothing is
//! ever downloaded or run beyond the two small files.
//!
//! The window process makes this one request itself: the two files are
//! read only up to `MAX_SIZE` bytes, and nothing in them is parsed before
//! the Ed25519 signature over the file checks out against the key built
//! into the exe.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ferossh_update::{Error, MAX_SIZE, SIGNATURE_URL, UPDATE_URL, Update};
use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_REFRESH,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle,
    WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// What a check found.
#[derive(Debug)]
pub enum Outcome {
    /// A release newer than this exe.
    Newer(Update),
    /// This exe is the latest release (or newer).
    Current(Update),
    /// The check could not be made or its answer was refused.
    Failed(String),
}

/// Runs a check; blocks for the network, so call it off the UI thread.
pub fn check() -> Outcome {
    let Some(key) = ferossh_update::public_key() else {
        return Outcome::Failed("this build of FeroSSH has no update key".into());
    };
    let file = match fetch(UPDATE_URL) {
        Ok(f) => f,
        Err(e) => return Outcome::Failed(e),
    };
    let signature = match fetch(SIGNATURE_URL) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e),
    };
    match ferossh_update::verify(&file, &signature, &key) {
        Ok(update) if ferossh_update::is_newer(&update, env!("CARGO_PKG_VERSION")) => {
            Outcome::Newer(update)
        }
        Ok(update) => Outcome::Current(update),
        Err(Error::BadSignature) => Outcome::Failed(
            "the update file's signature does not match FeroSSH's key, so it was ignored".into(),
        ),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// When the last automatic check ran: `%LOCALAPPDATA%\FeroSSH\last-update-check`.
fn stamp_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("FeroSSH")
        .join("last-update-check")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Whether the weekly check is due.
pub fn due() -> bool {
    let last = std::fs::read_to_string(stamp_path())
        .ok()
        .and_then(|t| t.trim().parse().ok());
    ferossh_update::due(last, now())
}

/// Notes that a check ran, so the next automatic one waits a week.
pub fn checked() {
    let path = stamp_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, now().to_string());
}

/// A WinHTTP handle, closed when dropped.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, what: &str) -> Result<Handle, String> {
        if raw.is_null() {
            Err(format!(
                "{what} failed: {}",
                windows::core::Error::from_thread()
            ))
        } else {
            Ok(Handle(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle WinHTTP gave us, closed once.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// GETs an https://github.com URL, following GitHub's redirect to its
/// download host (never to plain HTTP), and returns at most `MAX_SIZE`
/// bytes of a 200 response.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let path = url
        .strip_prefix("https://github.com")
        .ok_or_else(|| format!("not a GitHub URL: {url}"))?;
    let net = |e: windows::core::Error| format!("could not reach GitHub: {}", e.message());
    // SAFETY: WinHTTP calls with valid strings and handles that outlive
    // them (dropped in reverse order); the read buffer is ours and its
    // length is passed.
    unsafe {
        let agent = HSTRING::from(format!("FeroSSH/{}", env!("CARGO_PKG_VERSION")));
        let session = Handle::new(
            WinHttpOpen(
                &agent,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        // Resolve, connect, send, receive: 15 s each at most.
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 15_000, 15_000).map_err(net)?;
        let connection = Handle::new(
            WinHttpConnect(session.0, w!("github.com"), INTERNET_DEFAULT_HTTPS_PORT, 0),
            "WinHttpConnect",
        )?;
        let request = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE | WINHTTP_FLAG_REFRESH,
            ),
            "WinHttpOpenRequest",
        )?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(net)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(net)?;

        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(net)?;
        match status {
            200 => {}
            404 => return Err("the latest release has no update file yet".into()),
            _ => return Err(format!("GitHub answered HTTP {status}")),
        }

        let mut body = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
            .map_err(net)?;
            if read == 0 {
                break;
            }
            body.extend(buffer.iter().take(read as usize));
            if body.len() > MAX_SIZE {
                return Err("the update file is too large".into());
            }
        }
        Ok(body)
    }
}
