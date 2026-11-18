//! The Windows notification for a pane that wants the user
//! ([`crate::attention`]).
//!
//! It goes up as a notification-area balloon (`Shell_NotifyIconW` with
//! `NIF_INFO`), which Windows 10 and 11 show as a notification and keep in
//! the notification centre. That needs no install, shortcut or
//! AppUserModelID, so the exe stays portable. The icon stays in the
//! notification area while something waits, so a notification missed
//! while away is still there; it goes when FeroSSH is brought forward,
//! by the notification, the icon or anything else. After Explorer
//! restarts (`TaskbarCreated`) the icon is put back, without its
//! notification.

// Shell_NotifyIconW and the class icon are Win32 FFI on a struct filled
// in here.
#![allow(unsafe_code)]

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_LARGE_ICON, NIIF_USER, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONSHOW, NIN_BALLOONUSERCLICK, NIN_SELECT,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONDATAW_0, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GCLP_HICON, GetClassLongPtrW, HICON, RegisterWindowMessageW, WM_APP, WM_LBUTTONUP,
};
use windows::core::w;

/// The message the icon and its notification send to the window.
pub const WM_NOTIFY_ICON: u32 = WM_APP + 60;
/// The icon's id among the window's notification-area icons.
const ICON_ID: u32 = 1;
/// `NIN_KEYSELECT`: the icon chosen with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT + 1;

thread_local! {
    /// Whether the icon is in the notification area (the window's thread).
    static SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The icon's tooltip, to put it back after Explorer restarts.
    static TIP: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// What a [`WM_NOTIFY_ICON`] message says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A notification came on screen.
    Shown,
    /// The user clicked a notification or the icon.
    Clicked,
    Other,
}

/// `text` into a fixed UTF-16 buffer, cut to fit with its NUL.
fn fill<const N: usize>(buf: &mut [u16; N], text: &str) {
    let mut n = 0;
    for unit in text.encode_utf16() {
        if n + 1 >= N {
            break;
        }
        if let Some(slot) = buf.get_mut(n) {
            *slot = unit;
        }
        n += 1;
    }
    // A cut between the halves of a surrogate pair leaves half a
    // character: drop it.
    if n > 0
        && let Some(&last) = buf.get(n - 1)
        && (0xd800..0xdc00).contains(&last)
    {
        n -= 1;
    }
    if let Some(slot) = buf.get_mut(n) {
        *slot = 0;
    }
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

/// Shows a notification headed `title`, saying `body`; `waiting` is the
/// icon's tooltip. Whether the shell took it. Call it outside the
/// window's state borrow: the shell may send the window messages
/// meanwhile.
pub fn show(hwnd: HWND, title: &str, body: &str, waiting: &str) -> bool {
    let mut d = data(hwnd);
    // SAFETY: our window; the class icon is the exe's, kept for the
    // window's lifetime.
    let icon = unsafe { GetClassLongPtrW(hwnd, GCLP_HICON) };
    d.hIcon = HICON(icon as *mut _);
    d.uCallbackMessage = WM_NOTIFY_ICON;
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP | NIF_INFO;
    fill(&mut d.szTip, waiting);
    TIP.with_borrow_mut(|t| waiting.clone_into(t));
    fill(&mut d.szInfoTitle, title);
    // An empty text would take the balloon away instead of showing it.
    fill(&mut d.szInfo, if body.is_empty() { " " } else { body });
    d.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
    // SAFETY: `d` is filled in as documented and lives over the calls.
    unsafe {
        if SHOWN.get() && Shell_NotifyIconW(NIM_MODIFY, &d).as_bool() {
            return true;
        }
        add(d)
    }
}

/// Adds the icon `d` describes and asks for the version 4 messages.
///
/// # Safety
/// `d` is filled in as documented.
unsafe fn add(mut d: NOTIFYICONDATAW) -> bool {
    // SAFETY: as the caller promises.
    unsafe {
        let added = Shell_NotifyIconW(NIM_ADD, &d).as_bool();
        SHOWN.set(added);
        if added {
            d.Anonymous = NOTIFYICONDATAW_0 {
                uVersion: NOTIFYICON_VERSION_4,
            };
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
        added
    }
}

/// The message Explorer broadcasts when it made the taskbar anew, after
/// a restart or crash; the icons on it are gone then.
pub fn taskbar_created() -> u32 {
    thread_local! {
        static MSG: u32 = {
            // SAFETY: a constant string.
            unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
        };
    }
    MSG.with(|m| *m)
}

/// After Explorer made the taskbar anew: puts the icon back if it was
/// there, without its notification (that was seen or missed already).
pub fn restore(hwnd: HWND) {
    if !SHOWN.replace(false) {
        return;
    }
    let mut d = data(hwnd);
    // SAFETY: as in `show`.
    let icon = unsafe { GetClassLongPtrW(hwnd, GCLP_HICON) };
    d.hIcon = HICON(icon as *mut _);
    d.uCallbackMessage = WM_NOTIFY_ICON;
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    TIP.with_borrow(|t| fill(&mut d.szTip, t));
    // SAFETY: `d` is filled in as documented.
    unsafe {
        add(d);
    }
}

/// Takes the icon, and with it its notifications, away.
pub fn hide(hwnd: HWND) {
    if !SHOWN.replace(false) {
        return;
    }
    let d = data(hwnd);
    // SAFETY: as in `show`.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &d);
    }
}

/// What a [`WM_NOTIFY_ICON`] message with `lparam` says. With
/// `NOTIFYICON_VERSION_4` the event is in the low word.
pub fn event(lparam: isize) -> Event {
    match (lparam as usize & 0xffff) as u32 {
        NIN_BALLOONSHOW => Event::Shown,
        NIN_BALLOONUSERCLICK | NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP => Event::Clicked,
        _ => Event::Other,
    }
}
