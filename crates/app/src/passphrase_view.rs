//! A small window that asks for a passphrase: the master password when
//! FeroSSH starts with a vault, a new one (typed twice), or the one of
//! an encrypted connections file. [`ask`] runs it modally, like a
//! message box. It can come up before the main window exists (to unlock
//! the vault), so the owner is optional.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, DeleteObject, FillRect, HDC, HFONT, HGDIOBJ};
use windows::Win32::UI::Controls::{BST_CHECKED, EM_LIMITTEXT, WC_BUTTONW, WC_EDITW, WC_STATICW};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW,
    CS_VREDRAW, CreateWindowExW, DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, ES_PASSWORD,
    GW_CHILD, GW_HWNDNEXT, GetClientRect, GetMessageW, GetSystemMetrics, GetWindow, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsDialogMessageW,
    LoadCursorW, MSG, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW,
    SendMessageW, SetForegroundWindow, SetWindowTextW, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_ERASEBKGND, WNDCLASSW, WS_BORDER,
    WS_CAPTION, WS_CHILD, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};
use zeroize::{Zeroize, Zeroizing};

use crate::native::{fonts, scale, set_font, wide};

const ID_PASS: usize = 100;
const ID_REPEAT: usize = 101;
const ID_OPTION: usize = 102;
const ID_STATUS: usize = 103;
const SS_NOPREFIX: u32 = 0x80;
const WIDTH: i32 = 460;

/// What to ask.
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    pub title: &'a str,
    /// A few lines above the fields: what the passphrase is for.
    pub intro: &'a str,
    /// A new passphrase: typed twice and checked with
    /// [`crate::vault::check_new`].
    pub new: bool,
    /// A check box under the fields, and whether it starts ticked.
    pub option: Option<(&'a str, bool)>,
    pub ok: &'a str,
    pub cancel: &'a str,
    /// Shown under the fields at first (why the last try failed).
    pub problem: Option<&'a str>,
}

/// The passphrase, and the check box.
#[derive(Debug)]
pub struct Answer {
    pub passphrase: Zeroizing<String>,
    pub option: bool,
}

#[derive(Clone, Copy)]
struct Handles {
    pass: HWND,
    repeat: Option<HWND>,
    option: Option<HWND>,
    status: HWND,
}

struct State {
    h: Handles,
    new: bool,
    done: Option<Option<Answer>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Shows the window (over `owner` when there is one) and waits for OK
/// or Cancel.
pub fn ask(owner: Option<HWND>, ask: &Ask) -> Option<Answer> {
    // SAFETY: plain query.
    let dpi = unsafe { GetDpiForSystem() };
    // SAFETY: a class with valid static strings and a window procedure
    // of ours; the window and its controls are destroyed before return.
    let dlg = unsafe { create(owner, dpi, ask) }.ok()?;
    let (font, title) = fonts(dpi);
    // SAFETY: the title font is not used here.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(title.0));
    }
    set_fonts(dlg, font);
    crate::ui_theme::restyle(dlg);
    crate::ui_theme::popup_frame(dlg);
    // SAFETY: a message box's modal loop; see keygen_view::ask.
    let answer = unsafe {
        if let Some(o) = owner {
            let _ = EnableWindow(o, false);
        }
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        if let Some(h) = with(|s| s.h) {
            let _ = SetFocus(Some(h.pass));
        }
        let mut msg = MSG::default();
        loop {
            if let Some(done) = with(|s| s.done.take()).flatten() {
                break done;
            }
            let got = GetMessageW(&mut msg, None, 0, 0);
            if got.0 == 0 {
                PostQuitMessage(msg.wParam.0 as i32);
                break None;
            }
            if got.0 == -1 {
                break None;
            }
            if !IsDialogMessageW(dlg, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    };
    // SAFETY: the owner first, so it gets the activation back.
    unsafe {
        if let Some(o) = owner {
            let _ = EnableWindow(o, true);
            let _ = SetForegroundWindow(o);
        }
        let _ = DestroyWindow(dlg);
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
    STATE.with(|s| s.borrow_mut().take());
    answer
}

fn set_fonts(dlg: HWND, font: HFONT) {
    set_font(dlg, font);
    // SAFETY: walking our own window's children.
    unsafe {
        let mut c = GetWindow(dlg, GW_CHILD).ok();
        while let Some(h) = c {
            set_font(h, font);
            c = GetWindow(h, GW_HWNDNEXT).ok();
        }
    }
}

/// The intro's height in DIPs: about 18 per line of 60 characters.
fn intro_height(text: &str) -> i32 {
    let lines: usize = text
        .split('\n')
        .map(|l| l.chars().count().div_ceil(62).max(1))
        .sum();
    i32::try_from(lines).unwrap_or(8).min(12) * 18
}

unsafe fn create(owner: Option<HWND>, dpi: u32, ask: &Ask) -> windows::core::Result<HWND> {
    // SAFETY: the caller's promise; see `ask`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHPassphrase");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let s = |d: i32| scale(d, dpi);
        let intro_h = intro_height(ask.intro);
        let mut y = 12 + intro_h + 8;
        let fields_top = y;
        y += 36;
        if ask.new {
            y += 36;
        }
        if ask.option.is_some() {
            y += 30;
        }
        let status_top = y;
        let height = status_top + 40 + 12 + 28 + 12;
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: s(WIDTH),
            bottom: s(height),
        };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut o = RECT::default();
        match owner {
            Some(owner) => {
                let _ = GetWindowRect(owner, &mut o);
            }
            None => {
                o.right = GetSystemMetrics(SM_CXSCREEN);
                o.bottom = GetSystemMetrics(SM_CYSCREEN);
            }
        }
        let x = o.left + ((o.right - o.left) - w) / 2;
        let top = o.top + ((o.bottom - o.top) - h) / 3;
        let title = wide(ask.title);
        let dlg = CreateWindowExW(
            ex,
            class,
            PCWSTR(title.as_ptr()),
            style,
            x,
            top,
            w,
            h,
            owner,
            None,
            Some(instance.into()),
            None,
        )?;
        let child = |class: PCWSTR, text: &str, style: u32, id: usize, at: (i32, i32, i32, i32)| {
            let text = wide(text);
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                s(at.0),
                s(at.1),
                s(at.2),
                s(at.3),
                Some(dlg),
                Some(HMENU(id as *mut _)),
                Some(instance.into()),
                None,
            )
        };
        child(
            WC_STATICW,
            ask.intro,
            SS_NOPREFIX,
            0,
            (12, 12, WIDTH - 24, intro_h),
        )?;
        let (lx, fw) = (124, WIDTH - 124 - 12);
        let label =
            |text: &str, y: i32| child(WC_STATICW, text, SS_NOPREFIX, 0, (12, y + 4, 108, 20));
        let field = WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32 | ES_PASSWORD as u32;
        let mut y = fields_top;
        label(
            if ask.new {
                "New passphrase"
            } else {
                "Passphrase"
            },
            y,
        )?;
        let pass = child(WC_EDITW, "", field, ID_PASS, (lx, y, fw, 24))?;
        SendMessageW(pass, EM_LIMITTEXT, Some(WPARAM(1024)), None);
        y += 36;
        let repeat = if ask.new {
            label("Repeat it", y)?;
            let r = child(WC_EDITW, "", field, ID_REPEAT, (lx, y, fw, 24))?;
            SendMessageW(r, EM_LIMITTEXT, Some(WPARAM(1024)), None);
            y += 36;
            Some(r)
        } else {
            None
        };
        let option = match ask.option {
            Some((text, on)) => {
                let b = child(
                    WC_BUTTONW,
                    text,
                    WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32,
                    ID_OPTION,
                    (lx, y, fw, 24),
                )?;
                if on {
                    SendMessageW(b, BM_SETCHECK, Some(WPARAM(BST_CHECKED.0 as usize)), None);
                }
                Some(b)
            }
            None => None,
        };
        let status = child(
            WC_STATICW,
            ask.problem.unwrap_or_default(),
            SS_NOPREFIX,
            ID_STATUS,
            (12, status_top, WIDTH - 24, 40),
        )?;
        let bottom = height - 12 - 28;
        child(
            WC_BUTTONW,
            ask.ok,
            WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
            IDOK.0 as usize,
            (WIDTH - 12 - 96 - 8 - 96, bottom, 96, 28),
        )?;
        child(
            WC_BUTTONW,
            ask.cancel,
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            IDCANCEL.0 as usize,
            (WIDTH - 12 - 96, bottom, 96, 28),
        )?;
        STATE.with(|st| {
            *st.borrow_mut() = Some(State {
                h: Handles {
                    pass,
                    repeat,
                    option,
                    status,
                },
                new: ask.new,
                done: None,
            });
        });
        Ok(dlg)
    }
}

/// A password field's text, with the copies wiped.
fn secret_text(hwnd: HWND) -> Zeroizing<String> {
    // SAFETY: a buffer sized from the control's own length.
    unsafe {
        let n = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = Zeroizing::new(vec![0u16; n + 1]);
        let got = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        let mut text = String::from_utf16_lossy(buf.get(..got).unwrap_or_default());
        let out = Zeroizing::new(text.clone());
        text.zeroize();
        out
    }
}

fn set_text(hwnd: HWND, text: &str) {
    let t = wide(text);
    // SAFETY: our own control; the text lives through the call.
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(t.as_ptr()));
    }
}

fn ok() {
    let Some((h, new)) = with(|s| (s.h, s.new)) else {
        return;
    };
    let passphrase = secret_text(h.pass);
    if new {
        let repeat = h.repeat.map(secret_text).unwrap_or_default();
        if let Err(why) = crate::vault::check_new(&passphrase, &repeat) {
            set_text(h.status, &why);
            return;
        }
    } else if passphrase.is_empty() {
        set_text(h.status, "Type the passphrase.");
        return;
    }
    let option = h.option.is_some_and(|b| {
        // SAFETY: plain query of our own control.
        unsafe { SendMessageW(b, BM_GETCHECK, None, None) }.0 == BST_CHECKED.0 as isize
    });
    with(|s| s.done = Some(Some(Answer { passphrase, option })));
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match wparam.0 & 0xFFFF {
                x if x == IDOK.0 as usize => ok(),
                x if x == IDCANCEL.0 as usize => {
                    with(|s| s.done = Some(None));
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            with(|s| s.done = Some(None));
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let mut r = RECT::default();
            // SAFETY: the DC Windows gives for our own window.
            unsafe {
                let _ = GetClientRect(hwnd, &mut r);
                FillRect(
                    HDC(wparam.0 as *mut _),
                    &r,
                    crate::ui_theme::sys_brush(COLOR_WINDOW),
                );
            }
            LRESULT(1)
        }
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
