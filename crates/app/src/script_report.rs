//! A script run's report, as native Windows controls: a row per host
//! with how it went and what its `@capture` steps kept, and Show
//! session, Copy, Save CSV, Retry failed and Stop run (Close once over),
//! on a panel over the uncovered part of the tools pane.
//!
//! The rows come from `SnippetsView::native_report` and follow the run as
//! it goes. Keys go back to the main window as the [`SideKey`] the drawn
//! report took: Enter (or a double-click) shows the host's session, C
//! copies, S saves CSV, R runs the failed hosts again, Escape stops the
//! run or closes the report. [`pre_translate`] runs in the message loop
//! like the other native tools' and passes Ctrl+Shift shortcuts on to the
//! main window.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_CALCRECT, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject,
    DrawTextW, EndPaint, FillRect, HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW,
    LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_PARAM, LVIF_STATE,
    LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_ITEMCHANGED, LVN_KEYDOWN, LVNI_SELECTED,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER, LVS_REPORT, LVS_SHOWSELALWAYS,
    LVS_SINGLESEL, NM_CUSTOMDRAW, NM_DBLCLK, NMHDR, NMLVCUSTOMDRAW, NMLVKEYDOWN, SetWindowTheme,
    WC_BUTTONW, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, GetClientRect, HMENU,
    IDC_ARROW, IDCANCEL, IsChild, IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow,
    RegisterClassW, SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_PAINT, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, set_font, wide};
use crate::sessions::SideKey;
use crate::snippets_view::{NativeReport, RowMark};

/// What the user did on the report.
#[derive(Debug)]
pub enum Event {
    /// Row `i` selected.
    Select(usize),
    /// A key or button, as the drawn report took it.
    Key(SideKey),
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_LIST: usize = 800;
const ID_SHOW: usize = 810;
const ID_COPY: usize = 811;
const ID_CSV: usize = 812;
const ID_RETRY: usize = 813;
const ID_STOP: usize = 814;

/// The list's columns and their widths at 96 DPI.
const COLUMNS: [(&str, i32); 3] = [("Host", 130), ("Status", 150), ("Captured", 200)];

const BUTTONS: [(PCWSTR, usize); 5] = [
    (w!("Show session"), ID_SHOW),
    (w!("Copy"), ID_COPY),
    (w!("Save CSV"), ID_CSV),
    (w!("Retry failed"), ID_RETRY),
    (w!("Stop run"), ID_STOP),
];

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    list: HWND,
    /// Show session, Copy, Save CSV, Retry failed, Stop run or Close.
    buttons: [HWND; 5],
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    shown: Option<NativeReport>,
    filling: bool,
    help_at: RECT,
    placed: Option<(Rect, i32)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn handles() -> Option<Handles> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| s.h))
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

fn filling() -> bool {
    with(|s| s.filling).unwrap_or(true)
}

/// Creates the panel, hidden, as a child of `main`.
pub fn create(main: HWND, handler: Handler, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering a class and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHScriptReport");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let panel = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class,
            w!(""),
            WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            Some(main),
            None,
            Some(instance.into()),
            None,
        )?;
        let child = |class: PCWSTR, text: PCWSTR, style: u32, id: usize| {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                text,
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(style),
                0,
                0,
                0,
                0,
                Some(panel),
                Some(HMENU(id as *mut _)),
                Some(instance.into()),
                None,
            )
        };
        let list = child(
            WC_LISTVIEWW,
            w!(""),
            LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER,
            ID_LIST,
        )?;
        let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
        let ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER;
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(ex as usize)),
            Some(LPARAM(ex as isize)),
        );
        for (i, (title, width)) in COLUMNS.iter().enumerate() {
            let mut text = wide(title);
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: scale(*width, dpi),
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
        let mut buttons = [HWND::default(); 5];
        for (b, (label, id)) in buttons.iter_mut().zip(BUTTONS) {
            *b = child(WC_BUTTONW, label, BS_PUSHBUTTON as u32, id)?;
        }
        let h = Handles {
            main,
            panel,
            list,
            buttons,
        };
        let (font, title_font) = fonts(dpi);
        for c in controls(&h) {
            set_font(c, font);
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                h,
                handler,
                font,
                title_font,
                dpi,
                shown: None,
                filling: false,
                help_at: RECT::default(),
                placed: None,
            });
        });
    }
    if let Some(h) = handles() {
        pass_edge(h.panel);
    }
    Ok(())
}

fn controls(h: &Handles) -> Vec<HWND> {
    let mut v = vec![h.list];
    v.extend(h.buttons);
    v
}

/// Puts the panel over `at`, laid out for the tool's whole `width`;
/// `None` hides it.
pub fn place(at: Option<Rect>, width: i32, dpi: u32) {
    let Some(h) = handles() else {
        return;
    };
    let Some(at) = at else {
        with(|s| s.placed = None);
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(h.panel, SW_HIDE);
        }
        return;
    };
    with(|s| s.placed = Some((at, width)));
    let refont = with(|s| {
        if s.dpi == dpi {
            return None;
        }
        s.dpi = dpi;
        let (font, title) = fonts(dpi);
        let old = [
            std::mem::replace(&mut s.font, font),
            std::mem::replace(&mut s.title_font, title),
        ];
        Some((font, old))
    })
    .flatten();
    if let Some((font, old)) = refont {
        for c in controls(&h) {
            set_font(c, font);
        }
        // SAFETY: no control uses the old fonts any more.
        unsafe {
            for f in old {
                let _ = DeleteObject(HGDIOBJ(f.0));
            }
        }
    }
    let pad = scale(8, dpi);
    let title = scale(36, dpi);
    let button = scale(28, dpi);
    let gap = scale(6, dpi);
    let line = scale(20, dpi);
    let inner = (width - 2 * pad).max(0);
    let help_h = 2 * line;
    let list_y = title + help_h;
    // The buttons wrap onto as many rows as the width needs.
    let widths = [104, 64, 80, 96, 80].map(|w| scale(w, dpi));
    let mut rows = 1;
    let mut x = 0;
    for w in widths {
        if x > 0 && x + w > inner {
            rows += 1;
            x = 0;
        }
        x += w + gap;
    }
    let buttons_h = rows * button + (rows - 1) * gap;
    let buttons_y = (at.h - pad - buttons_h).max(list_y + 4 * button);
    let list_h = (buttons_y - gap - list_y).max(button);
    with(|s| {
        s.help_at = RECT {
            left: pad,
            top: title,
            right: pad + inner,
            bottom: title + help_h,
        };
    });
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.list, pad, list_y, inner, list_h, true);
        let (mut x, mut y) = (pad, buttons_y);
        for (b, w) in h.buttons.iter().zip(widths) {
            if x > pad && x + w > pad + inner {
                x = pad;
                y += button + gap;
            }
            let _ = MoveWindow(*b, x, y, w, button, true);
            x += w + gap;
        }
        let _ = ShowWindow(h.panel, SW_SHOWNA);
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
}

/// Whether one of the panel's controls has the keyboard.
pub fn has_focus() -> bool {
    let Some(h) = handles() else {
        return false;
    };
    // SAFETY: plain queries.
    unsafe {
        let f = GetFocus();
        !f.is_invalid() && (f == h.panel || IsChild(h.panel, f).as_bool())
    }
}

/// Gives the keyboard to the list.
pub fn focus_first() {
    if let Some(h) = handles() {
        // SAFETY: our own control.
        unsafe {
            let _ = SetFocus(Some(h.list));
        }
    }
}

/// The host column's text: a mark for how it went, then the host.
fn host_text(row: &crate::snippets_view::NativeReportRow) -> String {
    let mark = match row.mark {
        RowMark::Waiting => "…",
        RowMark::Done => "✓",
        RowMark::Failed => "✗",
        RowMark::Stopped => "–",
    };
    format!("{mark} {}", row.label)
}

fn set_text(list: HWND, row: usize, column: i32, text: &str) {
    let mut text = wide(text);
    let item = LVITEMW {
        iSubItem: column,
        pszText: PWSTR(text.as_mut_ptr()),
        ..Default::default()
    };
    // SAFETY: our own control; the struct and its text live through the
    // call, which copies them.
    unsafe {
        SendMessageW(
            list,
            LVM_SETITEMTEXTW,
            Some(WPARAM(row)),
            Some(LPARAM(&raw const item as isize)),
        );
    }
}

fn select_row(list: HWND, i: usize) {
    let on = LVIS_SELECTED.0 | LVIS_FOCUSED.0;
    let item = LVITEMW {
        mask: LVIF_STATE,
        state: LIST_VIEW_ITEM_STATE_FLAGS(on),
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(on),
        ..Default::default()
    };
    // SAFETY: our own control; the struct lives through the call.
    unsafe {
        SendMessageW(
            list,
            LVM_SETITEMSTATE,
            Some(WPARAM(i)),
            Some(LPARAM(&raw const item as isize)),
        );
        SendMessageW(list, LVM_ENSUREVISIBLE, Some(WPARAM(i)), Some(LPARAM(0)));
    }
}

/// Shows `report`: another report, or other rows, fill the list anew;
/// the same rows only get their texts.
pub fn set_report(report: NativeReport) {
    let Some(h) = handles() else {
        return;
    };
    let before = with(|s| s.shown.replace(report.clone())).flatten();
    if before.as_ref() == Some(&report) {
        return;
    }
    let refill = before.as_ref().is_none_or(|b| {
        b.serial != report.serial
            || b.rows.len() != report.rows.len()
            || b.rows
                .iter()
                .zip(&report.rows)
                .any(|(x, y)| x.label != y.label)
    });
    with(|s| s.filling = true);
    if refill {
        // SAFETY: our own control; each struct and its text live through
        // the call, which copies them.
        unsafe {
            SendMessageW(h.list, LVM_DELETEALLITEMS, None, None);
            for (i, row) in report.rows.iter().enumerate() {
                let mut label = wide(&host_text(row));
                let item = LVITEMW {
                    mask: LVIF_TEXT | LVIF_PARAM,
                    iItem: i as i32,
                    pszText: PWSTR(label.as_mut_ptr()),
                    lParam: LPARAM(i as isize),
                    ..Default::default()
                };
                SendMessageW(
                    h.list,
                    LVM_INSERTITEMW,
                    Some(WPARAM(0)),
                    Some(LPARAM(&raw const item as isize)),
                );
                set_text(h.list, i, 1, &row.status);
                set_text(h.list, i, 2, &row.values);
            }
        }
        select_row(h.list, report.selected);
    } else if let Some(b) = &before {
        for (i, (old, new)) in b.rows.iter().zip(&report.rows).enumerate() {
            if old.mark != new.mark {
                set_text(h.list, i, 0, &host_text(new));
            }
            if old.status != new.status {
                set_text(h.list, i, 1, &new.status);
            }
            if old.values != new.values {
                set_text(h.list, i, 2, &new.values);
            }
        }
    }
    with(|s| s.filling = false);
    enable_buttons(&h);
    // SAFETY: our own windows.
    unsafe {
        let stop = if report.running {
            w!("Stop run")
        } else {
            w!("Close")
        };
        if let Some(b) = h.buttons.get(4) {
            let _ = SetWindowTextW(*b, stop);
        }
        let _ = InvalidateRect(Some(h.panel), None, false);
        let _ = InvalidateRect(Some(h.list), None, false);
    }
}

fn selected(list: HWND) -> Option<usize> {
    // SAFETY: a plain message to our own control.
    let i = unsafe {
        SendMessageW(
            list,
            LVM_GETNEXTITEM,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(LVNI_SELECTED as isize)),
        )
    }
    .0;
    usize::try_from(i).ok()
}

fn enable_buttons(h: &Handles) {
    let Some(r) = with(|s| s.shown.clone()).flatten() else {
        return;
    };
    let shown = selected(h.list)
        .and_then(|i| r.rows.get(i))
        .is_some_and(|row| row.shown);
    let on = [shown, true, true, r.can_retry && !r.running, true];
    for (b, on) in h.buttons.iter().zip(on) {
        // SAFETY: our own control.
        unsafe {
            let _ = EnableWindow(*b, on);
        }
    }
}

fn emit(e: Event) {
    let Some((main, handler)) = with(|s| (s.h.main, s.handler)) else {
        return;
    };
    handler(main, e);
}

/// Sends `key` for the selected row.
fn key(h: &Handles, key: SideKey) {
    if let Some(i) = selected(h.list) {
        emit(Event::Select(i));
    }
    emit(Event::Key(key));
}

/// Runs in the message loop before a message is translated and sent;
/// `true` when it was handled here.
pub fn pre_translate(msg: &MSG) -> bool {
    let Some(h) = handles() else {
        return false;
    };
    // SAFETY: plain queries and messages to our own windows.
    unsafe {
        if !IsWindowVisible(h.panel).as_bool()
            || !(msg.hwnd == h.panel || IsChild(h.panel, msg.hwnd).as_bool())
        {
            return false;
        }
        match msg.message {
            WM_KEYDOWN if key_down(VK_CONTROL.0) && key_down(VK_SHIFT.0) => {
                let mut m = *msg;
                m.hwnd = h.main;
                let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&m);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&m);
                return true;
            }
            WM_KEYDOWN if msg.hwnd == h.list && msg.wParam.0 == usize::from(VK_RETURN.0) => {
                key(&h, SideKey::Enter { split: false });
                return true;
            }
            WM_KEYDOWN if msg.wParam.0 == usize::from(VK_ESCAPE.0) => {
                key(&h, SideKey::Escape);
                return true;
            }
            WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR => return false,
            _ => {}
        }
        IsDialogMessageW(h.panel, msg).as_bool()
    }
}

fn command(h: Handles, id: usize, code: u32) {
    match (id, code) {
        (id, _) if id == IDCANCEL.0 as usize => key(&h, SideKey::Escape),
        (ID_SHOW, BN_CLICKED) => key(&h, SideKey::Enter { split: false }),
        (ID_COPY, BN_CLICKED) => key(&h, SideKey::Char('c')),
        (ID_CSV, BN_CLICKED) => key(&h, SideKey::Char('s')),
        (ID_RETRY, BN_CLICKED) => key(&h, SideKey::Char('r')),
        (ID_STOP, BN_CLICKED) => key(&h, SideKey::Escape),
        _ => {}
    }
}

fn notify(h: Handles, lparam: LPARAM) -> LRESULT {
    // SAFETY: Windows passes a valid NMHDR (and the larger struct its code
    // says) for the duration of WM_NOTIFY.
    unsafe {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.hwndFrom != h.list {
            return LRESULT(0);
        }
        match hdr.code {
            NM_CUSTOMDRAW => {
                let cd = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
                let stage = cd.nmcd.dwDrawStage;
                if stage == CDDS_PREPAINT {
                    return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
                }
                if stage == CDDS_ITEMPREPAINT {
                    // Failed hosts in the error colour, waiting ones grey.
                    let i = usize::try_from(cd.nmcd.lItemlParam.0).unwrap_or(usize::MAX);
                    let mark = with(|s| {
                        s.shown
                            .as_ref()
                            .and_then(|r| r.rows.get(i))
                            .map(|row| row.mark)
                    })
                    .flatten();
                    match mark {
                        Some(RowMark::Failed) => cd.clrText = crate::ui_theme::error_color(),
                        Some(RowMark::Waiting | RowMark::Stopped) => {
                            cd.clrText = COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT));
                        }
                        _ => {}
                    }
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
            NM_DBLCLK => key(&h, SideKey::Enter { split: false }),
            LVN_ITEMCHANGED => {
                if !filling()
                    && let Some(i) = selected(h.list)
                {
                    emit(Event::Select(i));
                }
                enable_buttons(&h);
            }
            LVN_KEYDOWN => {
                let k = &*(lparam.0 as *const NMLVKEYDOWN);
                let vk = k.wVKey;
                match u8::try_from(vk).map(char::from) {
                    Ok('C') if !key_down(VK_CONTROL.0) => key(&h, SideKey::Char('c')),
                    Ok('S') => key(&h, SideKey::Char('s')),
                    Ok('R') => key(&h, SideKey::Char('r')),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, report, help_at)) =
        with(|s| (s.title_font, s.font, s.dpi, s.shown.clone(), s.help_at))
    else {
        return;
    };
    let Some(report) = report else {
        return;
    };
    let help = if report.running {
        "Each host runs in a hidden session. Enter or a double-click shows a host's \
         session in a tab; Escape stops the run."
    } else {
        "Enter or a double-click shows a host's session in a tab. C copies the report \
         as a table, S saves it as CSV, R runs the failed hosts again, Escape closes it."
    };
    // SAFETY: a BeginPaint/EndPaint pair on our own window; the font is
    // selected back out.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h.panel, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(h.panel, &mut rc);
        FillRect(dc, &rc, crate::ui_theme::sys_brush(COLOR_WINDOW));
        let old = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        let pad = scale(10, dpi);
        let mut tr = RECT {
            left: pad,
            top: 0,
            right: rc.right - pad,
            bottom: scale(36, dpi),
        };
        let line = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        let grey = COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT));
        SetTextColor(dc, grey);
        let count = format!("{} of {} over", report.over, report.rows.len());
        let mut c: Vec<u16> = count.encode_utf16().collect();
        let mut size = tr;
        DrawTextW(dc, &mut c, &mut size, DT_LEFT | line | DT_CALCRECT);
        DrawTextW(dc, &mut c, &mut tr, DT_RIGHT | line);
        tr.right -= size.right - size.left + pad;
        SelectObject(dc, HGDIOBJ(title_font.0));
        SetTextColor(dc, COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT)));
        let mut t: Vec<u16> = report.name.encode_utf16().collect();
        DrawTextW(dc, &mut t, &mut tr, DT_LEFT | line | DT_END_ELLIPSIS);
        SelectObject(dc, HGDIOBJ(font.0));
        SetTextColor(dc, grey);
        let mut text: Vec<u16> = help.encode_utf16().collect();
        let mut at = help_at;
        DrawTextW(dc, &mut text, &mut at, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
        SelectObject(dc, old);
        let _ = EndPaint(h.panel, &ps);
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let h = handles().filter(|h| h.panel == hwnd);
    match (msg, h) {
        (WM_PAINT, Some(h)) => {
            paint(h);
            LRESULT(0)
        }
        (WM_ERASEBKGND, _) => LRESULT(1),
        (WM_COMMAND, Some(h)) => {
            command(h, wparam.0 & 0xFFFF, ((wparam.0 >> 16) & 0xFFFF) as u32);
            LRESULT(0)
        }
        (WM_NOTIFY, Some(h)) => notify(h, lparam),
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
