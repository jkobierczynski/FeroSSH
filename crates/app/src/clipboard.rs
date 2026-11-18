//! Clipboard access: text, and for pasting files to a host, the files
//! Explorer copied or an image.

// Clipboard and global-memory calls are Win32 FFI; each unsafe block is a
// direct call on memory this module locks, reads or fills and unlocks.
#![allow(unsafe_code)]

use std::path::PathBuf;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::core::w;

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

/// The bytes of clipboard format `format`, if the clipboard holds it.
fn get_bytes(hwnd: HWND, format: u32) -> Option<Vec<u8>> {
    // SAFETY: plain query.
    unsafe { IsClipboardFormatAvailable(format) }.ok()?;
    let _open = Open::new(hwnd)?;
    // SAFETY: the clipboard is open; the handle stays owned by it.
    let handle = unsafe { GetClipboardData(format) }.ok()?;
    let mem = HGLOBAL(handle.0);
    // SAFETY: a valid global handle; the bytes read stay inside the size
    // Windows reports for it, and the lock is released below.
    unsafe {
        let ptr = GlobalLock(mem) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, GlobalSize(mem)).to_vec();
        let _ = GlobalUnlock(mem);
        Some(bytes)
    }
}

/// A PNG image on the clipboard, as browsers and screenshot tools put
/// one there (the registered "PNG" format, which keeps transparency).
pub fn get_png(hwnd: HWND) -> Option<Vec<u8>> {
    // SAFETY: registers (or looks up) a format name; no pointers kept.
    let format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    if format == 0 {
        return None;
    }
    get_bytes(hwnd, format).filter(|b| crate::paste_upload::is_png(b))
}

/// A bitmap on the clipboard: the header, colour table and pixels of
/// `CF_DIBV5`, else `CF_DIB` (Windows makes either from the other).
pub fn get_dib(hwnd: HWND) -> Option<Vec<u8>> {
    get_bytes(hwnd, u32::from(CF_DIBV5.0)).or_else(|| get_bytes(hwnd, u32::from(CF_DIB.0)))
}

/// The files on the clipboard, copied in Explorer (`CF_HDROP`).
pub fn get_files(hwnd: HWND) -> Vec<PathBuf> {
    let format = u32::from(CF_HDROP.0);
    // SAFETY: plain query.
    if unsafe { IsClipboardFormatAvailable(format) }.is_err() {
        return Vec::new();
    }
    let Some(_open) = Open::new(hwnd) else {
        return Vec::new();
    };
    // SAFETY: the clipboard is open; the handle stays owned by it, so it
    // is only read, never finished.
    let Ok(handle) = (unsafe { GetClipboardData(format) }) else {
        return Vec::new();
    };
    let drop = HDROP(handle.0);
    let mut paths = Vec::new();
    // SAFETY: a drop handle the clipboard owns; each buffer is sized
    // from the length the first call reports.
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            if let Some(name) = buf.get(..got) {
                paths.push(PathBuf::from(String::from_utf16_lossy(name)));
            }
        }
    }
    paths
}
