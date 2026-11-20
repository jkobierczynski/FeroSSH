//! A small window that asks for a few lines of text and one check box,
//! like a message box: Session > tmux > Share this session and Join a
//! shared session. [`ask`] runs it modally over the main window.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, DeleteObject, FillRect, HDC, HFONT, HGDIOBJ};
use windows::Win32::UI::Controls::{BST_CHECKED, EM_LIMITTEXT, WC_BUTTONW, WC_EDITW, WC_STATICW};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW,
    CS_VREDRAW, CreateWindowExW, DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, GW_CHILD,
    GW_HWNDNEXT, GetClientRect, GetMessageW, GetWindow, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsDialogMessageW, LoadCursorW, MSG,
    PostQuitMessage, RegisterClassW, SW_SHOW, SendMessageW, SetForegroundWindow, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_ERASEBKGND,
    WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_POPUP,
    WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::native::{fonts, scale, set_font, wide};

const ID_FIELD: usize = 100;
const ID_OPTION: usize = 90;
const SS_NOPREFIX: u32 = 0x80;
const WIDTH: i32 = 480;
/// Fields at most.
const MAX_FIELDS: usize = 4;

/// What to ask.
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    pub title: &'a str,
    /// A few lines above the fields.
    pub intro: &'a str,
    /// Each field's label and starting text.
    pub fields: &'a [(&'a str, &'a str)],
    /// A check box under the fields, and whether it starts ticked.
    pub option: Option<(&'a str, bool)>,
    pub ok: &'a str,
    pub cancel: &'a str,
}

/// The fields' text, in order, and the check box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub fields: Vec<String>,
    pub option: bool,
}

struct State {
    fields: Vec<HWND>,
    option: Option<HWND>,
    done: Option<Option<Answer>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Shows the window over `owner` and waits for OK or Cancel.
pub fn ask(owner: HWND, ask: &Ask) -> Option<Answer> {
    // SAFETY: plain query of our own window: the form takes the DPI of
    // the monitor it opens on.
    let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
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
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        if let Some(Some(first)) = with(|s| s.fields.first().copied()) {
            let _ = SetFocus(Some(first));
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
        let mut c = GetWindow(dlg, GW_CHILD).ok();
        while let Some(h) = c {
            set_font(h, font);
            c = GetWindow(h, GW_HWNDNEXT).ok();
        }
    }
}

/// The intro's height in DIPs: about 18 per line of 64 characters.
fn intro_height(text: &str) -> i32 {
    let lines: usize = text
        .split('\n')
        .map(|l| l.chars().count().div_ceil(64).max(1))
        .sum();
    i32::try_from(lines).unwrap_or(10).min(16) * 18
}

unsafe fn create(owner: HWND, dpi: u32, ask: &Ask) -> windows::core::Result<HWND> {
    // SAFETY: the caller's promise; see `ask`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHFields");
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
        let fields = ask
            .fields
            .get(..ask.fields.len().min(MAX_FIELDS))
            .unwrap_or_default();
        let intro_h = intro_height(ask.intro);
        let fields_top = 12 + intro_h + 8;
        let mut height = fields_top + 36 * i32::try_from(fields.len()).unwrap_or(0);
        if ask.option.is_some() {
            height += 30;
        }
        height += 12 + 28 + 12;
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
        let _ = GetWindowRect(owner, &mut o);
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
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )?;
        let built = build(
            dlg,
            instance.into(),
            dpi,
            ask,
            fields,
            height,
            intro_h,
            fields_top,
        );
        if built.is_err() {
            // Its controls go with it.
            let _ = DestroyWindow(dlg);
        }
        built.map(|()| dlg)
    }
}

/// The form's controls, in window `dlg`.
#[allow(clippy::too_many_arguments)]
unsafe fn build(
    dlg: HWND,
    instance: windows::Win32::Foundation::HINSTANCE,
    dpi: u32,
    ask: &Ask,
    fields: &[(&str, &str)],
    height: i32,
    intro_h: i32,
    fields_top: i32,
) -> windows::core::Result<()> {
    let s = |d: i32| scale(d, dpi);
    // SAFETY: the caller's promise; see `ask`.
    unsafe {
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
                Some(instance),
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
        let (lx, fw) = (164, WIDTH - 164 - 12);
        let mut y = fields_top;
        let mut edits = Vec::new();
        for (i, (label, text)) in fields.iter().enumerate() {
            child(WC_STATICW, label, SS_NOPREFIX, 0, (12, y + 4, lx - 16, 20))?;
            let e = child(
                WC_EDITW,
                text,
                WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                ID_FIELD + i,
                (lx, y, fw, 24),
            )?;
            SendMessageW(e, EM_LIMITTEXT, Some(WPARAM(512)), None);
            edits.push(e);
            y += 36;
        }
        let option = match ask.option {
            Some((text, on)) => {
                let b = child(
                    WC_BUTTONW,
                    text,
                    WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32,
                    ID_OPTION,
                    (12, y, WIDTH - 24, 24),
                )?;
                if on {
                    SendMessageW(b, BM_SETCHECK, Some(WPARAM(BST_CHECKED.0 as usize)), None);
                }
                Some(b)
            }
            None => None,
        };
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
                fields: edits,
                option,
                done: None,
            });
        });
        Ok(())
    }
}

fn text_of(hwnd: HWND) -> String {
    // SAFETY: a buffer sized from the control's own length.
    unsafe {
        let n = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(buf.get(..got).unwrap_or_default())
    }
}

fn ok() {
    let Some((fields, option)) = with(|s| (s.fields.clone(), s.option)) else {
        return;
    };
    let fields = fields.into_iter().map(text_of).collect();
    let option = option.is_some_and(|b| {
        // SAFETY: plain query of our own control.
        unsafe { SendMessageW(b, BM_GETCHECK, None, None) }.0 == BST_CHECKED.0 as isize
    });
    with(|s| s.done = Some(Some(Answer { fields, option })));
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
