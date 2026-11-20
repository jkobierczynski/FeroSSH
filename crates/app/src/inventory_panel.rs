//! Connection manager > Inventory: a window over the main one listing
//! every connection with when it was last connected to, its host key
//! and, where switched on, the server's OS and kernel (see
//! `inventory_view.rs`). [`show`] runs it modally, like a message box.
//!
//! The search box takes the connection manager's search words; a click
//! on a column heading sorts by it (again: the other way). Save CSV
//! writes the rows shown, in their order, to the download folder.
//! Connect (Enter, a double click) closes the window and opens the
//! picked connection.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, DeleteObject, FillRect, HDC, HFONT, HGDIOBJ};
use windows::Win32::UI::Controls::{
    EM_SETCUEBANNER, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_PARAM,
    LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_GETITEMW,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_COLUMNCLICK, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_DBLCLK,
    NMHDR, NMLISTVIEW, SetWindowTheme, WC_BUTTONW, WC_EDITW, WC_LISTVIEWW, WC_STATICW,
};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    DestroyWindow, DispatchMessageW, EN_CHANGE, ES_AUTOHSCROLL, GW_CHILD, GW_HWNDNEXT,
    GetClientRect, GetMessageW, GetWindow, GetWindowRect, HMENU, IDC_ARROW, IDCANCEL, IDOK,
    IsDialogMessageW, LoadCursorW, MSG, PostQuitMessage, RegisterClassW, SW_SHOW, SendMessageW,
    SetForegroundWindow, SetWindowTextW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_ERASEBKGND, WM_NOTIFY, WNDCLASSW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::inventory_view::{self, Column, Row};
use crate::native::{fonts, scale, search_text, set_font, wide};

/// What the user chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Open this connection.
    Connect(String),
}

const ID_SEARCH: usize = 100;
const ID_LIST: usize = 101;
const ID_STATUS: usize = 102;
const ID_CSV: usize = 110;
const SS_NOPREFIX: u32 = 0x80;

/// Width and height of the window's inside, in DIPs.
const WIDTH: i32 = 980;
const HEIGHT: i32 = 560;
/// The least it shrinks to on a small screen, in DIPs: the search box,
/// a few list rows, the status line and the buttons still fit, and so does
/// a 1366 x 768 screen at 250% scaling.
const MIN_WIDTH: i32 = 340;
const MIN_HEIGHT: i32 = 200;

#[derive(Clone, Copy)]
struct Handles {
    search: HWND,
    list: HWND,
    status: HWND,
}

struct State {
    h: Handles,
    rows: Vec<Row>,
    /// Indices into `rows`, in the order shown.
    shown: Vec<usize>,
    sort: Column,
    descending: bool,
    /// Where Save CSV writes.
    save_dir: PathBuf,
    done: Option<Option<Answer>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Shows the window over `owner` with `rows`, the search starting as
/// `query`, and waits for it to close.
pub fn show(
    owner: HWND,
    dpi: u32,
    rows: Vec<Row>,
    query: &str,
    save_dir: PathBuf,
) -> Option<Answer> {
    // SAFETY: a class with valid static strings and a window procedure
    // of ours; the window and its controls are destroyed before return.
    let dlg = unsafe { create(owner, dpi) }.ok()?;
    with(|s| {
        s.rows = rows;
        s.save_dir = save_dir;
    });
    if let Some(h) = with(|s| s.h) {
        set_text(h.search, query);
    }
    fill();
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
            let _ = SetFocus(Some(h.list));
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
        let mut c = GetWindow(dlg, GW_CHILD).ok();
        while let Some(h) = c {
            set_font(h, font);
            c = GetWindow(h, GW_HWNDNEXT).ok();
        }
    }
}

unsafe fn create(owner: HWND, dpi: u32) -> windows::core::Result<HWND> {
    // SAFETY: the caller's promise; see `show`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHInventory");
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
        // The frame's size, then as much of WIDTH x HEIGHT as the owner's
        // monitor has room for (the list scrolls the rest).
        let mut frame = RECT::default();
        let _ = AdjustWindowRectExForDpi(&mut frame, style, false, ex, dpi);
        let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
        let work = work_area(owner);
        let dips = |px: i32| px.max(0) * 96 / dpi.max(1) as i32;
        let (width, height) = match work {
            Some(a) => (
                WIDTH.min(dips(a.right - a.left - fw)).max(MIN_WIDTH),
                HEIGHT.min(dips(a.bottom - a.top - fh)).max(MIN_HEIGHT),
            ),
            None => (WIDTH, HEIGHT),
        };
        let (w, h) = (s(width) + fw, s(height) + fh);
        let mut o = RECT::default();
        let _ = GetWindowRect(owner, &mut o);
        let mut x = o.left + ((o.right - o.left) - w) / 2;
        let mut y = o.top + ((o.bottom - o.top) - h) / 3;
        if let Some(a) = work {
            x = x.min(a.right - w).max(a.left);
            y = y.min(a.bottom - h).max(a.top);
        }
        let dlg = CreateWindowExW(
            ex,
            class,
            w!("Inventory"),
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
        // Destroyed again (with what it holds) unless every control is
        // made.
        match controls(dlg, instance.into(), dpi, width, height) {
            Ok(()) => Ok(dlg),
            Err(e) => {
                let _ = DestroyWindow(dlg);
                Err(e)
            }
        }
    }
}

/// The work area (less the taskbar) of the monitor `hwnd` is on.
fn work_area(hwnd: HWND) -> Option<RECT> {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: a window handle and a local out-pointer with its size set.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(monitor, &mut info)
            .as_bool()
            .then_some(info.rcWork)
    }
}

/// The window's controls, laid out in a `width` x `height` DIP client.
unsafe fn controls(
    dlg: HWND,
    instance: windows::Win32::Foundation::HINSTANCE,
    dpi: u32,
    width: i32,
    height: i32,
) -> windows::core::Result<()> {
    // SAFETY: the caller's promise; see `create`.
    unsafe {
        let s = |d: i32| scale(d, dpi);
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
        let search = child(
            WC_EDITW,
            "",
            WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
            ID_SEARCH,
            (12, 12, width - 24, 24),
        )?;
        let cue = wide("Search: words, tag:prod, folder:db, -tag:legacy, an OS or a fingerprint");
        SendMessageW(
            search,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        let bottom = height - 12 - 28;
        let list = child(
            WC_LISTVIEWW,
            "",
            WS_TABSTOP.0 | WS_BORDER.0 | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            ID_LIST,
            (12, 44, width - 24, bottom - 44 - 44),
        )?;
        let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
        let lex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP;
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(lex as usize)),
            Some(LPARAM(lex as isize)),
        );
        for (i, c) in Column::ALL.iter().enumerate() {
            let mut text = wide(c.title());
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: s(c.width()),
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_INSERTCOLUMNW,
                Some(WPARAM(i)),
                Some(LPARAM(&raw const col as isize)),
            );
        }
        let status = child(
            WC_STATICW,
            "",
            SS_NOPREFIX,
            ID_STATUS,
            (12, bottom - 38, width - 24, 34),
        )?;
        let push = WS_TABSTOP.0 | BS_PUSHBUTTON as u32;
        child(WC_BUTTONW, "Save &CSV", push, ID_CSV, (12, bottom, 100, 28))?;
        child(
            WC_BUTTONW,
            "C&onnect",
            WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
            IDOK.0 as usize,
            (width - 12 - 88 - 8 - 88, bottom, 88, 28),
        )?;
        child(
            WC_BUTTONW,
            "Close",
            push,
            IDCANCEL.0 as usize,
            (width - 12 - 88, bottom, 88, 28),
        )?;
        STATE.with(|st| {
            *st.borrow_mut() = Some(State {
                h: Handles {
                    search,
                    list,
                    status,
                },
                rows: Vec::new(),
                shown: Vec::new(),
                sort: Column::Name,
                descending: false,
                save_dir: PathBuf::new(),
                done: None,
            });
        });
        Ok(())
    }
}

fn set_text(hwnd: HWND, text: &str) {
    let t = wide(text);
    // SAFETY: our own control; the text lives through the call.
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(t.as_ptr()));
    }
}

/// A time as the list shows it: local date and time.
fn local(secs: u64) -> String {
    crate::files_panel::when(i64::try_from(secs).ok())
}

/// The rows the search keeps, in the chosen order, and the summary.
fn fill() {
    let Some((h, cells, summary)) = with(|s| {
        let query = search_text(s.h.search);
        let mut shown = inventory_view::filter(&s.rows, &query);
        inventory_view::sort(&mut shown, s.sort, s.descending);
        let summary = inventory_view::summary(&shown, s.rows.len());
        // Every row shown is one of `s.rows`: its index from its address.
        let base = s.rows.as_ptr() as usize;
        let size = std::mem::size_of::<Row>().max(1);
        s.shown = shown
            .iter()
            .map(|r| (std::ptr::from_ref::<Row>(*r) as usize - base) / size)
            .collect();
        let cells: Vec<Vec<String>> = shown
            .iter()
            .map(|r| {
                Column::ALL
                    .iter()
                    .map(|&c| inventory_view::text(r, c, local))
                    .collect()
            })
            .collect();
        (s.h, cells, summary)
    }) else {
        return;
    };
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        SendMessageW(h.list, LVM_DELETEALLITEMS, None, None);
        for (i, row) in cells.iter().enumerate() {
            let mut label = wide(row.first().map_or("", String::as_str));
            let item = LVITEMW {
                mask: LVIF_TEXT | LVIF_PARAM,
                iItem: i as i32,
                pszText: PWSTR(label.as_mut_ptr()),
                lParam: LPARAM(i as isize),
                ..Default::default()
            };
            let at = SendMessageW(
                h.list,
                LVM_INSERTITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const item as isize)),
            )
            .0;
            for (sub, text) in row.iter().enumerate().skip(1) {
                let mut text = wide(text);
                let item = LVITEMW {
                    iSubItem: sub as i32,
                    pszText: PWSTR(text.as_mut_ptr()),
                    ..Default::default()
                };
                SendMessageW(
                    h.list,
                    LVM_SETITEMTEXTW,
                    Some(WPARAM(at as usize)),
                    Some(LPARAM(&raw const item as isize)),
                );
            }
        }
        if !cells.is_empty() {
            let on = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
            let item = LVITEMW {
                stateMask: on,
                state: on,
                ..Default::default()
            };
            SendMessageW(
                h.list,
                LVM_SETITEMSTATE,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const item as isize)),
            );
        }
    }
    set_text(h.status, &summary);
}

/// The selected connection's name.
fn selected() -> Option<String> {
    let h = with(|s| s.h)?;
    // SAFETY: plain messages to our own control; the struct lives
    // through the call.
    let at = unsafe {
        let i = SendMessageW(
            h.list,
            LVM_GETNEXTITEM,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(LVNI_SELECTED as isize)),
        )
        .0;
        if i < 0 {
            return None;
        }
        let mut item = LVITEMW {
            mask: LVIF_PARAM,
            iItem: i as i32,
            ..Default::default()
        };
        SendMessageW(
            h.list,
            LVM_GETITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&raw mut item as isize)),
        );
        usize::try_from(item.lParam.0).ok()?
    };
    with(|s| {
        s.shown
            .get(at)
            .and_then(|&i| s.rows.get(i))
            .map(|r| r.host.name.clone())
    })
    .flatten()
}

/// Writes the rows shown to the download folder.
fn save_csv() {
    let Some((text, dir)) = with(|s| {
        let rows: Vec<&Row> = s.shown.iter().filter_map(|&i| s.rows.get(i)).collect();
        (inventory_view::csv(&rows), s.save_dir.clone())
    }) else {
        return;
    };
    let stamp = inventory_view::iso_utc(inventory_view::now())
        .replace(['-', ':'], "")
        .replace('T', "-")
        .replace('Z', "");
    let stem = format!("ferossh-inventory-{stamp}");
    let note = match inventory_view::write_new(&dir, &stem, "csv", &text) {
        Ok(path) => format!("Saved the list as {}.", path.display()),
        Err(e) => format!("Could not save the list in {}: {e}", dir.display()),
    };
    if let Some(h) = with(|s| s.h) {
        set_text(h.status, &note);
    }
}

fn connect() {
    if let Some(name) = selected() {
        with(|s| s.done = Some(Some(Answer::Connect(name))));
    }
}

fn command(id: usize, code: u32) {
    match (id, code) {
        (x, _) if x == IDOK.0 as usize => connect(),
        (x, _) if x == IDCANCEL.0 as usize => {
            with(|s| s.done = Some(None));
        }
        (ID_SEARCH, EN_CHANGE) => fill(),
        (ID_CSV, BN_CLICKED) => save_csv(),
        _ => {}
    }
}

fn notify(lparam: LPARAM) -> LRESULT {
    let Some(h) = with(|s| s.h) else {
        return LRESULT(0);
    };
    // SAFETY: Windows passes a valid NMHDR (and the larger struct its code
    // says) for the duration of WM_NOTIFY.
    unsafe {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.hwndFrom != h.list {
            return LRESULT(0);
        }
        match hdr.code {
            NM_DBLCLK => connect(),
            LVN_COLUMNCLICK => {
                let nm = &*(lparam.0 as *const NMLISTVIEW);
                if let Some(&c) = usize::try_from(nm.iSubItem)
                    .ok()
                    .and_then(|i| Column::ALL.get(i))
                {
                    with(|s| {
                        if s.sort == c {
                            s.descending = !s.descending;
                        } else {
                            s.sort = c;
                            // The latest logins first.
                            s.descending = c == Column::LastConnected;
                        }
                    });
                    fill();
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            command(wparam.0 & 0xFFFF, ((wparam.0 >> 16) & 0xFFFF) as u32);
            LRESULT(0)
        }
        WM_NOTIFY => notify(lparam),
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
