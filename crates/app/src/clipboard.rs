//! Plain-text clipboard access.

// Clipboard and global-memory calls are Win32 FFI; each unsafe block is a
// direct call on memory this module locks, reads or fills and unlocks.
#![allow(unsafe_code)]

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Keeps the clipboard open for the lifetime of the guard.
struct Open;

impl Open {
    fn new(hwnd: HWND) -> Option<Self> {
        // SAFETY: our window; closed by Drop.
        unsafe { OpenClipboard(Some(hwnd)) }.ok().map(|()| Open)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: the clipboard this guard opened.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// The clipboard's text, if it holds any.
pub fn get_text(hwnd: HWND) -> Option<String> {
    let format = u32::from(CF_UNICODETEXT.0);
    // SAFETY: plain query.
    unsafe { IsClipboardFormatAvailable(format) }.ok()?;
    let _open = Open::new(hwnd)?;
    // SAFETY: the clipboard is open; the handle stays owned by it.
    let handle = unsafe { GetClipboardData(format) }.ok()?;
    let mem = HGLOBAL(handle.0);
    // SAFETY: a valid global handle; the units read stay inside the size
    // Windows reports for it, and the lock is released below.
    unsafe {
        let ptr = GlobalLock(mem) as *const u16;
        if ptr.is_null() {
            return None;
        }
        let max = GlobalSize(mem) / 2;
        let units = std::slice::from_raw_parts(ptr, max);
        let len = units.iter().position(|u| *u == 0).unwrap_or(max);
        let text = String::from_utf16_lossy(units.get(..len).unwrap_or_default());
        let _ = GlobalUnlock(mem);
        Some(text)
    }
}

/// Puts text on the clipboard. Returns false if the clipboard was busy.
pub fn set_text(hwnd: HWND, text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let bytes = units.len() * 2;
    let Some(_open) = Open::new(hwnd) else {
        return false;
    };
    // SAFETY: the clipboard is open. The allocation is filled within its
    // size, and either handed to the clipboard (which then owns it) or
    // freed.
    unsafe {
        if EmptyClipboard().is_err() {
            return false;
        }
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            return false;
        };
        let ptr = GlobalLock(mem) as *mut u16;
        if ptr.is_null() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
        let _ = GlobalUnlock(mem);
        if SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(mem.0))).is_err() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
    }
    true
}
