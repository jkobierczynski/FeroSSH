//! The command palette (Ctrl+Shift+A) as native Windows controls, on a
//! panel over the uncovered part of the tools pane: a search box, a list
//! view of what the search keeps in columns (name, shortcut or details,
//! where it comes from), and Run and Close buttons.
//!
//! The rows come from `CommandPalette::native`; what the user does goes
//! back as an [`Event`]. The keyboard stays in the search box: typing
//! narrows the list, Up, Down, Page Up and Page Down move the selection,
//! Enter runs the selected row (Shift+Enter: a connection in a split, a
//! script without its last Enter), and Escape clears the search, then
//! closes. [`pre_translate`] runs in the message loop and passes the
//! window's shortcuts on to the main window, as on the other tools.

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
    EM_LIMITTEXT, EM_SETCUEBANNER, EM_SETSEL, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVIF_PARAM, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS,
    LVM_ENSUREVISIBLE, LVM_GETCOUNTPERPAGE, LVM_GETITEMCOUNT, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE,
    LVM_SETITEMTEXTW, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP,
    LVS_NOSORTHEADER, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_DBLCLK, NM_SETFOCUS, NMHDR,
    NMITEMACTIVATE, SetWindowTheme, WC_BUTTONW, WC_EDITW, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, SetFocus, VK_CONTROL, VK_DOWN, VK_NEXT, VK_PRIOR, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    EN_CHANGE, EN_SETFOCUS, ES_AUTOHSCROLL, GetClientRect, GetWindowTextLengthW, HMENU, IDC_ARROW,
    IDCANCEL, IDOK, IsChild, IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow,
    RegisterClassW, SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CHAR, WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_PAINT, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};
use crate::palette::{NativePalette, NativeRow};

/// What the user did in the panel.
#[derive(Debug)]
pub enum Event {
    /// The search text changed.
    Query(String),
    /// Run the row at this place in the list; `split` for Shift+Enter.
    Run { row: usize, split: bool },
    /// Escape with no search, or Close: back to the terminal.
    Leave,
    /// One of the controls got the keyboard.
    Focused,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_SEARCH: usize = 1000;
const ID_LIST: usize = 1001;
const ID_RUN: usize = 1010;
const ID_CLOSE: usize = 1011;

/// Columns: title and width in DIPs.
const COLUMNS: [(&str, i32); 3] = [("Name", 230), ("Shortcut or details", 140), ("From", 90)];

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    search: HWND,
    list: HWND,
    run: HWND,
    close: HWND,
}

impl Handles {
    fn controls(&self) -> [HWND; 4] {
        [self.search, self.list, self.run, self.close]
    }
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    shown: Option<NativePalette>,
    /// While the controls are filled: their changes are not the user's.
    filling: bool,
    help_at: RECT,
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
        let class = w!("FeroSSHPalette");
        let wc = WNDCLASSW {
            // Redrawn whole when resized: the title row is laid out on
            // the width.
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
        let search = child(
            WC_EDITW,
            w!(""),
            WS_BORDER.0 | ES_AUTOHSCROLL as u32,
            ID_SEARCH,
        )?;
        // No longer than the palette keeps, so what shows is what runs (a
        // remote app's command line).
        SendMessageW(
            search,
            EM_LIMITTEXT,
            Some(WPARAM(crate::palette::MAX_FILTER)),
            None,
        );
        let list = child(
            WC_LISTVIEWW,
            w!(""),
            LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER,
            ID_LIST,
        )?;
        let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
        let ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP;
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
        let run = child(WC_BUTTONW, w!("&Run"), BS_DEFPUSHBUTTON as u32, ID_RUN)?;
        let close = child(WC_BUTTONW, w!("Close"), BS_PUSHBUTTON as u32, ID_CLOSE)?;
        let h = Handles {
            main,
            panel,
            search,
            list,
            run,
            close,
        };
        let (font, title_font) = fonts(dpi);
        for c in h.controls() {
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
            });
        });
    }
    if let Some(h) = handles() {
        pass_edge(h.panel);
    }
    Ok(())
}

/// Puts the panel over `at`, laid out for the tool's whole `width`;
/// `None` hides it.
pub fn place(at: Option<Rect>, width: i32, dpi: u32) {
    let Some(h) = handles() else {
        return;
    };
    let Some(at) = at else {
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(h.panel, SW_HIDE);
        }
        return;
    };
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
        for c in h.controls() {
            set_font(c, font);
        }
        for (i, (_, w)) in COLUMNS.iter().enumerate() {
            // SAFETY: our own control.
            unsafe {
                SendMessageW(
                    h.list,
                    LVM_SETCOLUMNWIDTH,
                    Some(WPARAM(i)),
                    Some(LPARAM(scale(*w, dpi) as isize)),
                );
            }
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
    let field = scale(26, dpi);
    let button = scale(28, dpi);
    let gap = scale(6, dpi);
    let line = scale(18, dpi);
    let inner = (width - 2 * pad).max(0);
    let buttons_y = (at.h - pad - button).max(title + field + gap);
    // Two lines of key help over the buttons, while there is room for
    // some of the list too.
    let help_h = if buttons_y - title - field - 2 * gap > 4 * line {
        2 * line
    } else {
        0
    };
    let help_y = buttons_y - gap - help_h;
    let list_y = title + field + gap;
    let list_end = if help_h > 0 {
        help_y - gap
    } else {
        buttons_y - gap
    };
    let list_h = (list_end - list_y).max(0);
    with(|s| {
        s.help_at = RECT {
            left: pad,
            top: help_y,
            right: pad + inner,
            bottom: help_y + help_h,
        };
    });
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.search, pad, title, inner, field, true);
        let _ = MoveWindow(h.list, pad, list_y, inner, list_h, true);
        fit_columns(h.list, inner, dpi);
        let w = scale(72, dpi);
        let _ = MoveWindow(h.run, pad, buttons_y, w, button, true);
        let _ = MoveWindow(h.close, pad + w + gap, buttons_y, w, button, true);
        let _ = ShowWindow(h.panel, SW_SHOWNA);
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
}

/// The columns over the list's `width`: From and the shortcut keep a
/// readable width while there is room, and Name takes the rest.
fn fit_columns(list: HWND, width: i32, dpi: u32) {
    let room = (width - scale(24, dpi)).max(0);
    let from = scale(COLUMNS[2].1, dpi).min(room / 5);
    let hint = scale(COLUMNS[1].1, dpi).min(room * 3 / 10);
    let name = (room - from - hint).max(scale(60, dpi));
    for (i, w) in [name, hint, from].into_iter().enumerate() {
        // SAFETY: our own control.
        unsafe {
            SendMessageW(
                list,
                LVM_SETCOLUMNWIDTH,
                Some(WPARAM(i)),
                Some(LPARAM(w as isize)),
            );
        }
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

/// Gives the search box the keyboard, the caret after what is typed.
pub fn focus_first() {
    let Some(h) = handles() else {
        return;
    };
    // SAFETY: our own control.
    unsafe {
        let _ = SetFocus(Some(h.search));
        let end = GetWindowTextLengthW(h.search) as usize;
        SendMessageW(
            h.search,
            EM_SETSEL,
            Some(WPARAM(end)),
            Some(LPARAM(end as isize)),
        );
    }
}

/// Shows `view`: a palette opened afresh empties the search box; a new
/// search fills the list again with its first row selected.
pub fn set_view(view: NativePalette) {
    let Some(h) = handles() else {
        return;
    };
    let before = with(|s| s.shown.replace(view.clone())).flatten();
    if before.as_ref() == Some(&view) {
        return;
    }
    let fresh = before
        .as_ref()
        .is_none_or(|b| b.serial != view.serial || b.apps != view.apps);
    with(|s| s.filling = true);
    if fresh || search_text(h.search) != view.filter {
        let text = wide(&view.filter);
        let cue = wide(if view.apps {
            "Type a program and its arguments"
        } else {
            "Type an action, connection or script"
        });
        // SAFETY: our own control; the texts live through the calls.
        unsafe {
            let _ = SetWindowTextW(h.search, PCWSTR(text.as_ptr()));
            SendMessageW(
                h.search,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(cue.as_ptr() as isize)),
            );
        }
    }
    // A new search starts on its first row, even with the same rows.
    let same_rows = before
        .as_ref()
        .is_some_and(|b| b.rows == view.rows && b.filter == view.filter)
        && !fresh;
    if !same_rows {
        fill(&h, &view.rows);
    }
    // SAFETY: our own controls.
    unsafe {
        let _ = EnableWindow(h.run, !view.rows.is_empty());
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
    with(|s| s.filling = false);
}

fn fill(h: &Handles, rows: &[NativeRow]) {
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        SendMessageW(h.list, LVM_DELETEALLITEMS, None, None);
        for (i, r) in rows.iter().enumerate() {
            let mut label = wide(&r.name);
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
            for (sub, text) in [&r.hint, &r.group].iter().enumerate() {
                let mut text = wide(text);
                let item = LVITEMW {
                    iSubItem: sub as i32 + 1,
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
    }
    if !rows.is_empty() {
        select(h.list, 0);
    }
}

fn select(list: HWND, index: i32) {
    let on = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
    let item = LVITEMW {
        stateMask: on,
        state: on,
        ..Default::default()
    };
    // SAFETY: our own control; the struct lives through the call.
    unsafe {
        SendMessageW(
            list,
            LVM_SETITEMSTATE,
            Some(WPARAM(index as usize)),
            Some(LPARAM(&raw const item as isize)),
        );
        SendMessageW(
            list,
            LVM_ENSUREVISIBLE,
            Some(WPARAM(index as usize)),
            Some(LPARAM(0)),
        );
    }
}

/// The selected row's place in the list.
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

/// Up, Down, Page Up or Page Down typed in the search box: the list's
/// selection moves, the keyboard stays where it is.
fn move_selection(h: &Handles, vk: u16) {
    // SAFETY: plain messages to our own control.
    let (count, page) = unsafe {
        (
            SendMessageW(h.list, LVM_GETITEMCOUNT, None, None).0,
            SendMessageW(h.list, LVM_GETCOUNTPERPAGE, None, None)
                .0
                .max(1),
        )
    };
    if count <= 0 {
        return;
    }
    let now = selected(h.list).map_or(-1, |i| i as isize);
    let to = match vk {
        v if v == VK_UP.0 => now - 1,
        v if v == VK_DOWN.0 => now + 1,
        v if v == VK_PRIOR.0 => now - page,
        v if v == VK_NEXT.0 => now + page,
        _ => return,
    };
    select(h.list, to.clamp(0, count - 1) as i32);
}

fn emit(e: Event) {
    let Some((main, handler)) = with(|s| (s.h.main, s.handler)) else {
        return;
    };
    handler(main, e);
}

/// Enter, Run or a double click: the selected row, or the first.
fn run(h: &Handles) {
    // SAFETY: a plain query.
    let count = unsafe { SendMessageW(h.list, LVM_GETITEMCOUNT, None, None).0 };
    if count <= 0 {
        return;
    }
    let row = selected(h.list).unwrap_or(0);
    // Shift+Enter, and Shift with a click on Run: in a split, or a
    // script without its last Enter.
    emit(Event::Run {
        row,
        split: key_down(VK_SHIFT.0),
    });
}

/// Escape: the search goes first, then the palette.
fn escape(h: &Handles) {
    if search_text(h.search).is_empty() {
        emit(Event::Leave);
    } else {
        // SAFETY: our own control; EN_CHANGE clears the search.
        unsafe {
            let _ = SetWindowTextW(h.search, w!(""));
        }
    }
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
            WM_KEYDOWN => {
                // The window's own shortcuts were sent to it by the
                // message loop already; other Ctrl+Shift keys (selecting
                // words in the search box) stay here.
                let vk = msg.wParam.0 as u16;
                if msg.hwnd == h.search && [VK_UP.0, VK_DOWN.0, VK_PRIOR.0, VK_NEXT.0].contains(&vk)
                {
                    move_selection(&h, vk);
                    return true;
                }
            }
            // Typing in the list goes on in the search box.
            WM_CHAR if msg.hwnd == h.list => {
                let c = msg.wParam.0 as u32;
                if c >= 0x20 && c != 0x7f && !key_down(VK_CONTROL.0) {
                    focus_first();
                    SendMessageW(h.search, WM_CHAR, Some(msg.wParam), Some(msg.lParam));
                    return true;
                }
            }
            WM_SYSCHAR if press_mnemonic(&h, msg.wParam.0 as u32) => return true,
            // Other Alt keys reach the menu bar.
            WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR => return false,
            _ => {}
        }
        IsDialogMessageW(h.panel, msg).as_bool()
    }
}

/// Alt+R runs, as a dialog's button would (R is no menu's letter).
fn press_mnemonic(h: &Handles, c: u32) -> bool {
    if char::from_u32(c).is_some_and(|c| c.eq_ignore_ascii_case(&'r')) {
        run(h);
        return true;
    }
    false
}

fn command(h: Handles, id: usize, code: u32) {
    match (id, code) {
        (ID_SEARCH, EN_CHANGE) if !filling() => emit(Event::Query(search_text(h.search))),
        (ID_SEARCH, EN_SETFOCUS) => emit(Event::Focused),
        (id, _) if id == IDOK.0 as usize => run(&h),
        (id, _) if id == IDCANCEL.0 as usize => escape(&h),
        (ID_RUN, BN_CLICKED) => run(&h),
        (ID_CLOSE, BN_CLICKED) => emit(Event::Leave),
        _ => {}
    }
}

fn notify(h: Handles, lparam: LPARAM) -> LRESULT {
    // SAFETY: Windows passes a valid NMHDR for the duration of WM_NOTIFY.
    unsafe {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.hwndFrom != h.list {
            return LRESULT(0);
        }
        match hdr.code {
            NM_SETFOCUS => emit(Event::Focused),
            NM_DBLCLK => {
                // Only on a row: blank space under the rows runs nothing.
                let item = &*(lparam.0 as *const NMITEMACTIVATE);
                if item.iItem >= 0 {
                    run(&h);
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, view, help_at)) =
        with(|s| (s.title_font, s.font, s.dpi, s.shown.clone(), s.help_at))
    else {
        return;
    };
    // SAFETY: a BeginPaint/EndPaint pair on our own window; the fonts are
    // selected back out.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h.panel, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(h.panel, &mut rc);
        FillRect(dc, &rc, crate::ui_theme::sys_brush(COLOR_WINDOW));
        let Some(view) = view else {
            let _ = EndPaint(h.panel, &ps);
            return;
        };
        let old = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        let pad = scale(10, dpi);
        let mut tr = RECT {
            left: pad,
            top: 0,
            right: rc.right - pad,
            bottom: scale(36, dpi),
        };
        let text_colour = COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT));
        let grey = COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT));
        let line = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        // The count on the right of the title row: "12 of 230".
        SetTextColor(dc, grey);
        let count = if view.filter.trim().is_empty() || view.apps {
            view.total.to_string()
        } else {
            format!("{} of {}", view.rows.len(), view.total)
        };
        let mut c: Vec<u16> = count.encode_utf16().collect();
        let mut size = tr;
        DrawTextW(dc, &mut c, &mut size, DT_LEFT | line | DT_CALCRECT);
        DrawTextW(dc, &mut c, &mut tr, DT_RIGHT | line);
        tr.right -= size.right - size.left + pad;
        SelectObject(dc, HGDIOBJ(title_font.0));
        SetTextColor(dc, text_colour);
        let title = if view.apps {
            "Run a remote app"
        } else {
            "Command palette"
        };
        let mut t: Vec<u16> = title.encode_utf16().collect();
        DrawTextW(dc, &mut t, &mut tr, DT_LEFT | line | DT_END_ELLIPSIS);
        SelectObject(dc, HGDIOBJ(font.0));
        SetTextColor(dc, grey);
        let help = if view.apps {
            "Enter runs the program on this tab's host, with what you typed after it. \
             Esc closes."
        } else if view.rows.is_empty() && !view.filter.is_empty() {
            "Nothing matches. Esc clears the search."
        } else {
            "Enter runs. Shift+Enter opens a connection in a split, or types a script \
             without its last Enter. Esc closes."
        };
        let mut t: Vec<u16> = help.encode_utf16().collect();
        let mut at = help_at;
        if at.bottom > at.top {
            DrawTextW(
                dc,
                &mut t,
                &mut at,
                DT_LEFT | DT_WORDBREAK | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
        }
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
