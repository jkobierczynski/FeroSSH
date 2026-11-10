//! Tools > New SSH key: a small window over the main one that asks what
//! key to make and where, as native Windows controls. [`ask`] runs it
//! modally, like a message box, and returns what the user chose; the
//! checks are [`crate::newkey::check`]'s, so a taken file name or two
//! different passphrases keep the window open with the reason shown.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, DeleteObject, FillRect, HDC, HFONT, HGDIOBJ};
use windows::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_EXPLORER, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::{EM_LIMITTEXT, WC_BUTTONW, WC_COMBOBOXW, WC_EDITW, WC_STATICW};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL,
    CBN_SELCHANGE, CBS_DROPDOWNLIST, CBS_HASSTRINGS, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, ES_PASSWORD, GetClientRect, GetMessageW,
    GetWindowRect, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsDialogMessageW, LoadCursorW, MSG,
    PostQuitMessage, RegisterClassW, SW_SHOW, SendMessageW, SetForegroundWindow, SetWindowTextW,
    ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_ERASEBKGND, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_CONTROLPARENT,
    WS_EX_DLGMODALFRAME, WS_GROUP, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::native::{fonts, scale, search_text, set_font, wide};
use crate::newkey::{Kind, Request, check, free_name};

const ID_KIND: usize = 100;
const ID_FILE: usize = 101;
const ID_BROWSE: usize = 102;
const ID_COMMENT: usize = 103;
const ID_PASS: usize = 104;
const ID_REPEAT: usize = 105;
const ID_STATUS: usize = 106;
const SS_NOPREFIX: u32 = 0x80;

/// Width and height of the window's inside, in DIPs.
const WIDTH: i32 = 480;
const HEIGHT: i32 = 300;

#[derive(Clone, Copy)]
struct Handles {
    kind: HWND,
    file: HWND,
    comment: HWND,
    pass: HWND,
    repeat: HWND,
    pass_labels: [HWND; 2],
    status: HWND,
}

struct State {
    h: Handles,
    dir: PathBuf,
    /// The file name filled in for the kind chosen: changing the kind
    /// changes it only while the user has not typed their own.
    suggested: String,
    /// `None` while the window is open; then the answer.
    done: Option<Option<Request>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Shows the window over `owner` and waits for Make or Cancel. New keys
/// go in `dir` (`~/.ssh`) by default; `comment` is filled in.
pub fn ask(owner: HWND, dpi: u32, dir: &Path, comment: &str) -> Option<Request> {
    // SAFETY: a class with valid static strings and a window procedure
    // of ours; the window and its controls are destroyed before return.
    let dlg = unsafe { create(owner, dpi, dir, comment) }.ok()?;
    let (font, title) = fonts(dpi);
    // SAFETY: the title font is not used here.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(title.0));
    }
    set_fonts(dlg, font);
    crate::ui_theme::restyle(dlg);
    crate::ui_theme::popup_frame(dlg);
    // SAFETY: the modal loop of a message box: the owner is disabled
    // while the window is up; every message is dispatched as usual, so
    // the main window keeps painting and reading its connections. No
    // state borrow is held while a message runs.
    let answer = unsafe {
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        if let Some(h) = with(|s| s.h) {
            let _ = SetFocus(Some(h.file));
        }
        let mut msg = MSG::default();
        loop {
            if let Some(done) = with(|s| s.done.take()).flatten() {
                break done;
            }
            let got = GetMessageW(&mut msg, None, 0, 0);
            if got.0 == 0 {
                // WM_QUIT: hand it on to the main loop.
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
    // SAFETY: the owner first, so it gets the activation back, then our
    // own window and font.
    unsafe {
        let _ = EnableWindow(owner, true);
        let _ = SetForegroundWindow(owner);
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
        let mut c = windows::Win32::UI::WindowsAndMessaging::GetWindow(
            dlg,
            windows::Win32::UI::WindowsAndMessaging::GW_CHILD,
        )
        .ok();
        while let Some(h) = c {
            set_font(h, font);
            c = windows::Win32::UI::WindowsAndMessaging::GetWindow(
                h,
                windows::Win32::UI::WindowsAndMessaging::GW_HWNDNEXT,
            )
            .ok();
        }
    }
}

unsafe fn create(owner: HWND, dpi: u32, dir: &Path, comment: &str) -> windows::core::Result<HWND> {
    // SAFETY: the caller's promise; see `ask`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHNewKey");
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
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: s(WIDTH),
            bottom: s(HEIGHT),
        };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut o = RECT::default();
        let _ = GetWindowRect(owner, &mut o);
        let x = o.left + ((o.right - o.left) - w) / 2;
        let y = o.top + ((o.bottom - o.top) - h) / 3;
        let dlg = CreateWindowExW(
            ex,
            class,
            w!("New SSH key"),
            style,
            x,
            y,
            w,
            h,
            Some(owner),
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
        let label =
            |text: &str, y: i32| child(WC_STATICW, text, SS_NOPREFIX, 0, (12, y + 4, 96, 20));
        let field = WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32;
        let (lx, fw) = (112, WIDTH - 112 - 12);
        label("Kind of key", 12)?;
        let kind = child(
            WC_COMBOBOXW,
            "",
            WS_TABSTOP.0 | WS_GROUP.0 | WS_VSCROLL.0 | (CBS_DROPDOWNLIST | CBS_HASSTRINGS) as u32,
            ID_KIND,
            (lx, 12, fw, 200),
        )?;
        for k in Kind::ALL {
            let t = wide(k.label());
            SendMessageW(kind, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        }
        SendMessageW(kind, CB_SETCURSEL, Some(WPARAM(0)), None);
        label("File", 48)?;
        let suggested = free_name(dir, Kind::File.file_name());
        let suggested = suggested.display().to_string();
        let file = child(WC_EDITW, &suggested, field, ID_FILE, (lx, 48, fw - 88, 24))?;
        child(
            WC_BUTTONW,
            "Browse...",
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            ID_BROWSE,
            (WIDTH - 12 - 80, 48, 80, 24),
        )?;
        label("Comment", 84)?;
        let comment = child(WC_EDITW, comment, field, ID_COMMENT, (lx, 84, fw, 24))?;
        let pass_labels = [label("Passphrase", 120)?, label("Repeat it", 156)?];
        let secret = field | ES_PASSWORD as u32;
        let pass = child(WC_EDITW, "", secret, ID_PASS, (lx, 120, fw, 24))?;
        let repeat = child(WC_EDITW, "", secret, ID_REPEAT, (lx, 156, fw, 24))?;
        for e in [pass, repeat, comment] {
            SendMessageW(e, EM_LIMITTEXT, Some(WPARAM(1024)), None);
        }
        let status = child(
            WC_STATICW,
            "",
            SS_NOPREFIX,
            ID_STATUS,
            (12, 192, WIDTH - 24, 56),
        )?;
        child(
            WC_BUTTONW,
            "Make key",
            WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
            IDOK.0 as usize,
            (WIDTH - 12 - 88 - 8 - 88, HEIGHT - 12 - 28, 88, 28),
        )?;
        child(
            WC_BUTTONW,
            "Cancel",
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            IDCANCEL.0 as usize,
            (WIDTH - 12 - 88, HEIGHT - 12 - 28, 88, 28),
        )?;
        let h = Handles {
            kind,
            file,
            comment,
            pass,
            repeat,
            pass_labels,
            status,
        };
        STATE.with(|st| {
            *st.borrow_mut() = Some(State {
                h,
                dir: dir.to_path_buf(),
                suggested,
                done: None,
            });
        });
        kind_changed();
        Ok(dlg)
    }
}

fn kind() -> Kind {
    let Some(h) = with(|s| s.h) else {
        return Kind::File;
    };
    // SAFETY: plain query of our own control.
    let i = unsafe { SendMessageW(h.kind, CB_GETCURSEL, None, None) }.0;
    usize::try_from(i)
        .ok()
        .and_then(|i| Kind::ALL.get(i).copied())
        .unwrap_or(Kind::File)
}

fn set_text(hwnd: HWND, text: &str) {
    let t = wide(text);
    // SAFETY: our own control; the text lives through the call.
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(t.as_ptr()));
    }
}

/// Another kind: its usual file name, unless the user typed one, and
/// the passphrase only for a key file.
fn kind_changed() {
    let kind = kind();
    let Some((h, dir, suggested)) = with(|s| (s.h, s.dir.clone(), s.suggested.clone())) else {
        return;
    };
    if search_text(h.file) == suggested {
        let name = free_name(&dir, kind.file_name()).display().to_string();
        set_text(h.file, &name);
        with(|s| s.suggested = name);
    }
    let on = kind.has_passphrase();
    // SAFETY: our own controls.
    unsafe {
        for c in [h.pass, h.repeat, h.pass_labels[0], h.pass_labels[1]] {
            let _ = EnableWindow(c, on);
        }
    }
    set_text(
        h.status,
        match kind {
            Kind::File => {
                "Leave the passphrase empty for a key that needs none. FeroSSH asks for \
                 the passphrase when it logs in with the key."
            }
            Kind::Hello => {
                "Windows asks for your PIN, face or fingerprint. The file holds no \
                 secret: the key stays in this PC's TPM."
            }
            _ => {
                "Windows asks you to touch the security key or to use your phone. The \
                 file holds no secret: the key stays on the device."
            }
        },
    );
}

/// The save dialog for Browse, starting at the file named now.
fn browse(dlg: HWND) {
    let Some(h) = with(|s| s.h) else {
        return;
    };
    let now = PathBuf::from(search_text(h.file));
    let dir = now.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = now
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut buf = vec![0u16; 4096];
    for (d, c) in buf.iter_mut().zip(name.encode_utf16()) {
        *d = c;
    }
    let dir = wide(&dir.display().to_string());
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: dlg,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: (buf.len() - 1) as u32,
        lpstrInitialDir: PCWSTR(dir.as_ptr()),
        lpstrTitle: w!("Where to save the new key"),
        Flags: OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: the struct is filled in as documented; `buf` and `dir`
    // outlive the call; no state borrow is held during it.
    let ok = unsafe { GetSaveFileNameW(&mut ofn) }.as_bool();
    if ok {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let picked = String::from_utf16_lossy(buf.get(..len).unwrap_or_default());
        set_text(h.file, &picked);
    }
}

/// Make: the answer, or why not shown in the window.
fn make() {
    let Some(h) = with(|s| s.h) else {
        return;
    };
    let kind = kind();
    let path = PathBuf::from(search_text(h.file).trim());
    let passphrase = zeroize::Zeroizing::new(search_text(h.pass));
    let repeat = zeroize::Zeroizing::new(search_text(h.repeat));
    let passphrase = if kind.has_passphrase() {
        passphrase
    } else {
        zeroize::Zeroizing::new(String::new())
    };
    if let Err(why) = check(kind, &path, &passphrase, &repeat) {
        set_text(h.status, &why);
        return;
    }
    let comment = ferossh_auth::keygen::clean_comment(&search_text(h.comment));
    with(|s| {
        s.done = Some(Some(Request {
            kind,
            path,
            comment,
            passphrase,
        }));
    });
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            match id {
                x if x == IDOK.0 as usize => make(),
                x if x == IDCANCEL.0 as usize => {
                    with(|s| s.done = Some(None));
                }
                ID_BROWSE if code == BN_CLICKED => browse(hwnd),
                ID_KIND if code == CBN_SELCHANGE => kind_changed(),
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
