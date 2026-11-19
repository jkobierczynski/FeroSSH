//! Tools > Known hosts as native Windows controls: a search box, a list
//! view of the known_hosts entries in columns (host, kind, key,
//! fingerprint, file and line, comment), and Remove, Copy fingerprint and
//! Reload buttons, on a panel over the uncovered part of the tools pane.
//!
//! The rows come from the main window's `KnownHostsView::rows`; what the
//! user does goes back as an [`Event`]. Several rows can be picked (Shift
//! and Ctrl clicks) and removed together. Keys work as in `live_panel`:
//! [`pre_translate`] runs in the message loop, passes Ctrl+Shift
//! shortcuts on to the main window, turns typing in the list into a
//! search, and gives Tab, Enter and Escape their dialog meaning. Del
//! removes the picked rows (after asking), Ctrl+C copies the fingerprint.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, HFONT, HGDIOBJ,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    EM_LIMITTEXT, EM_SETCUEBANNER, EM_SETSEL, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVIF_PARAM, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS,
    LVM_ENSUREVISIBLE, LVM_GETITEMW, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW,
    LVN_KEYDOWN, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP,
    LVS_NOSORTHEADER, LVS_REPORT, LVS_SHOWSELALWAYS, NM_SETFOCUS, NMHDR, NMLVKEYDOWN,
    SetWindowTheme, WC_BUTTONW, WC_EDITW, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_CONTROL, VK_DELETE, VK_DOWN, VK_F5, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, EN_CHANGE, EN_SETFOCUS,
    ES_AUTOHSCROLL, GetClientRect, GetWindowTextLengthW, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsChild,
    IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow, RegisterClassW, SW_HIDE,
    SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR,
    WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_PAINT, WM_SYSCHAR, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::known_hosts_view::Row;
use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};

/// An entry: its file (index) and line.
pub type Pick = (usize, usize);

/// What the user did in the panel.
#[derive(Debug)]
pub enum Event {
    /// Remove these entries, after asking (Del, the Remove button).
    Remove(Vec<Pick>),
    /// Copy an entry's fingerprint (Ctrl+C, the button).
    CopyFingerprint(Pick),
    /// Copy an entry's whole line (the right-click menu).
    CopyLine(Pick),
    /// Read the files again (F5, the Reload button).
    Reload,
    /// The search text changed.
    Query(String),
    /// One of the controls got the keyboard.
    Focused,
    /// Escape with no search: back to the terminal.
    Leave,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_SEARCH: usize = 200;
const ID_LIST: usize = 201;
const ID_REMOVE: usize = 210;
const ID_COPY: usize = 211;
const ID_RELOAD: usize = 212;

/// Columns: title and width in DIPs.
const COLUMNS: [(&str, i32); 6] = [
    ("Host", 170),
    ("Kind", 64),
    ("Key", 90),
    ("Fingerprint", 300),
    ("File", 110),
    ("Comment", 120),
];

/// The height of the line naming files that could not be read, in DIPs.
const NOTE_DIP: i32 = 22;

fn has_note() -> bool {
    STATE.with(|s| {
        s.try_borrow()
            .ok()
            .is_some_and(|s| s.as_ref().is_some_and(|s| !s.note.is_empty()))
    })
}

/// Buttons: width in DIPs.
const BUTTONS: [i32; 3] = [80, 112, 64];

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    search: HWND,
    list: HWND,
    buttons: [HWND; 3],
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    /// The rows as shown, to rebuild the list only when they change.
    rows: Option<Vec<Row>>,
    /// All entries (the title's count).
    total: usize,
    /// Files that could not be read, shown above the buttons.
    note: String,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn handles() -> Option<Handles> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| s.h))
}

/// Creates the panel, hidden, as a child of `main`.
pub fn create(main: HWND, handler: Handler, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering a class and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHKnownHosts");
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
        // The view reads at most 200 characters of the query; the box
        // takes no more, so what is shown is what is searched for.
        SendMessageW(search, EM_LIMITTEXT, Some(WPARAM(200)), None);
        let cue = wide("Search, or type a host name");
        SendMessageW(
            search,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        let list = child(
            WC_LISTVIEWW,
            w!(""),
            LVS_REPORT | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER,
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
        let buttons = [
            child(WC_BUTTONW, w!("Remove..."), BS_PUSHBUTTON as u32, ID_REMOVE)?,
            child(
                WC_BUTTONW,
                w!("Copy fingerprint"),
                BS_PUSHBUTTON as u32,
                ID_COPY,
            )?,
            child(WC_BUTTONW, w!("Reload"), BS_PUSHBUTTON as u32, ID_RELOAD)?,
        ];
        let (font, title_font) = fonts(dpi);
        for c in [search, list].iter().chain(&buttons) {
            set_font(*c, font);
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                h: Handles {
                    main,
                    panel,
                    search,
                    list,
                    buttons,
                },
                handler,
                font,
                title_font,
                dpi,
                rows: None,
                total: 0,
                note: String::new(),
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
    let refont = STATE.with(|s| {
        let mut s = s.try_borrow_mut().ok()?;
        let s = s.as_mut()?;
        if s.dpi == dpi {
            return None;
        }
        s.dpi = dpi;
        let (font, title) = fonts(dpi);
        let old = (
            std::mem::replace(&mut s.font, font),
            std::mem::replace(&mut s.title_font, title),
        );
        Some((font, old))
    });
    if let Some((font, (old, old_title))) = refont {
        for c in [h.search, h.list].iter().chain(&h.buttons) {
            set_font(*c, font);
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
            let _ = DeleteObject(HGDIOBJ(old.0));
            let _ = DeleteObject(HGDIOBJ(old_title.0));
        }
    }
    let pad = scale(8, dpi);
    let title = scale(36, dpi);
    let field = scale(26, dpi);
    let button = scale(28, dpi);
    let gap = scale(6, dpi);
    let inner = (width - 2 * pad).max(0);
    let buttons_y = (at.h - pad - button).max(title + field + gap);
    let list_y = title + field + gap;
    let note = if has_note() { scale(NOTE_DIP, dpi) } else { 0 };
    let list_h = (buttons_y - gap - note - list_y).max(0);
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.search, pad, title, inner, field, true);
        let _ = MoveWindow(h.list, pad, list_y, inner, list_h, true);
        let mut x = pad;
        for (b, w) in h.buttons.iter().zip(BUTTONS) {
            let w = scale(w, dpi);
            let _ = MoveWindow(*b, x, buttons_y, w, button, true);
            x += w + gap;
        }
        let _ = ShowWindow(h.panel, SW_SHOWNA);
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

/// Pastes `text` (as one line) into the search box when it has the
/// keyboard; elsewhere in the tool it goes nowhere.
pub fn paste(text: &str) {
    let Some(h) = handles() else {
        return;
    };
    // SAFETY: a plain query and a message to our own control; the text
    // lives through the call.
    unsafe {
        if GetFocus() != h.search {
            return;
        }
        let text = wide(&text.lines().collect::<Vec<_>>().join(" "));
        SendMessageW(
            h.search,
            windows::Win32::UI::Controls::EM_REPLACESEL,
            Some(WPARAM(1)),
            Some(LPARAM(text.as_ptr() as isize)),
        );
    }
}

fn set_state(list: HWND, index: i32, on: bool) {
    let state = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
    let item = LVITEMW {
        stateMask: state,
        state: if on { state } else { Default::default() },
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
        if on {
            SendMessageW(
                list,
                LVM_ENSUREVISIBLE,
                Some(WPARAM(index as usize)),
                Some(LPARAM(0)),
            );
        }
    }
}

/// The picked rows' indexes, in list order.
fn selected_indexes(list: HWND) -> Vec<i32> {
    let mut out = Vec::new();
    let mut from = usize::MAX;
    loop {
        // SAFETY: plain message to our own control.
        let i = unsafe {
            SendMessageW(
                list,
                LVM_GETNEXTITEM,
                Some(WPARAM(from)),
                Some(LPARAM(LVNI_SELECTED as isize)),
            )
        }
        .0 as i32;
        // The indexes come in rising order; anything else ends the walk.
        if i < 0 || out.last().is_some_and(|&last| i <= last) {
            return out;
        }
        out.push(i);
        from = i as usize;
    }
}

/// The row shown at list index `index` (its lParam is the row's place in
/// the rows given).
fn pick_at(list: HWND, index: i32) -> Option<Pick> {
    row_at(list, index).map(|r| (r.file, r.line))
}

/// The row shown at list index `index`, as it was given.
fn row_at(list: HWND, index: i32) -> Option<Row> {
    let mut item = LVITEMW {
        mask: LVIF_PARAM,
        iItem: index,
        ..Default::default()
    };
    // SAFETY: our own control; the struct is filled by the call.
    let ok = unsafe {
        SendMessageW(
            list,
            LVM_GETITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&raw mut item as isize)),
        )
    };
    if ok.0 == 0 {
        return None;
    }
    let at = usize::try_from(item.lParam.0).ok()?;
    STATE.with(|s| {
        let s = s.try_borrow().ok()?;
        s.as_ref()?.rows.as_ref()?.get(at).cloned()
    })
}

fn picked() -> Vec<Pick> {
    picked_rows().iter().map(|r| (r.file, r.line)).collect()
}

fn picked_rows() -> Vec<Row> {
    let Some(h) = handles() else {
        return Vec::new();
    };
    selected_indexes(h.list)
        .into_iter()
        .filter_map(|i| row_at(h.list, i))
        .collect()
}

/// Whether `new` is the same entry as `old`: same place and same key,
/// so a line that moved or was replaced in the file is not picked again.
fn same_entry(old: &Row, new: &Row) -> bool {
    (
        old.file,
        old.line,
        old.kind,
        &old.key,
        &old.fingerprint,
        &old.comment,
    ) == (
        new.file,
        new.line,
        new.kind,
        &new.key,
        &new.fingerprint,
        &new.comment,
    )
}

/// Shows `rows` (of `total` entries), unless it shows them already, and
/// `note` (files that could not be read; empty for none) above the
/// buttons. The picked entries stay picked where they are still listed;
/// with none, the first row is.
pub fn set_rows(rows: Vec<Row>, total: usize, note: String) {
    let Some(h) = handles() else {
        return;
    };
    let keep = picked_rows();
    let changed = STATE.with(|s| {
        let mut s = s.try_borrow_mut().ok()?;
        let s = s.as_mut()?;
        let title = s.total != total || s.note != note;
        s.total = total;
        s.note = note;
        if s.rows.as_ref() == Some(&rows) {
            return Some((false, title));
        }
        s.rows = Some(rows.clone());
        Some((true, title))
    });
    let Some((rebuild, title)) = changed else {
        return;
    };
    // SAFETY: our own windows; each struct and its text live through the
    // call, which copies them.
    unsafe {
        if title {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(h.panel), None, false);
        }
        if !rebuild {
            return;
        }
        SendMessageW(h.list, LVM_DELETEALLITEMS, None, None);
        for (i, r) in rows.iter().enumerate() {
            let mut label = wide(&r.hosts);
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
            for (sub, text) in [r.kind, &r.key, &r.fingerprint, &r.place, &r.comment]
                .iter()
                .enumerate()
            {
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
    let mut any = false;
    for (i, r) in rows.iter().enumerate() {
        if keep.iter().any(|k| same_entry(k, r)) {
            set_state(h.list, i as i32, true);
            any = true;
        }
    }
    if !any && !rows.is_empty() {
        set_state(h.list, 0, true);
    }
}

/// Shows `query` in the search box when the view changed it. The box's
/// change event then carries the same text.
pub fn set_search(query: &str) {
    let Some(h) = handles() else {
        return;
    };
    if search_text(h.search) != query {
        let text = wide(query);
        // SAFETY: our own control; the text lives through the call.
        unsafe {
            let _ = SetWindowTextW(h.search, PCWSTR(text.as_ptr()));
        }
    }
}

/// Gives the search box the keyboard (as the tool opens).
pub fn focus_search() {
    if let Some(h) = handles() {
        // SAFETY: our own control.
        unsafe {
            let _ = SetFocus(Some(h.search));
        }
    }
}

fn focus_list() {
    if let Some(h) = handles() {
        // SAFETY: our own control.
        unsafe {
            let _ = SetFocus(Some(h.list));
        }
    }
}

fn emit(e: Event) {
    let Some((main, handler)) =
        STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| (s.h.main, s.handler)))
    else {
        return;
    };
    handler(main, e);
}

fn remove() {
    let picks = picked();
    if !picks.is_empty() {
        emit(Event::Remove(picks));
    }
}

fn copy_fingerprint() {
    if let Some(&p) = picked().first() {
        emit(Event::CopyFingerprint(p));
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
                let vk = msg.wParam.0 as u16;
                if key_down(VK_CONTROL.0) && key_down(VK_SHIFT.0) {
                    let mut m = *msg;
                    m.hwnd = h.main;
                    let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&m);
                    windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&m);
                    return true;
                }
                if msg.hwnd == h.search && vk == VK_DOWN.0 {
                    focus_list();
                    return true;
                }
                if vk == VK_F5.0 {
                    emit(Event::Reload);
                    return true;
                }
            }
            WM_CHAR if msg.hwnd == h.list => {
                let c = msg.wParam.0 as u32;
                if c >= 0x20 && c != 0x7f && !key_down(VK_CONTROL.0) {
                    let _ = SetFocus(Some(h.search));
                    let end = GetWindowTextLengthW(h.search) as usize;
                    SendMessageW(
                        h.search,
                        EM_SETSEL,
                        Some(WPARAM(end)),
                        Some(LPARAM(end as isize)),
                    );
                    SendMessageW(h.search, WM_CHAR, Some(msg.wParam), Some(msg.lParam));
                    return true;
                }
            }
            WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR => return false,
            _ => {}
        }
        IsDialogMessageW(h.panel, msg).as_bool()
    }
}

fn command(h: Handles, id: usize, code: u32) {
    match (id, code) {
        (ID_SEARCH, EN_CHANGE) => emit(Event::Query(search_text(h.search))),
        (ID_SEARCH, EN_SETFOCUS) => emit(Event::Focused),
        // Enter in the search box goes to the list: nothing here is
        // harmless enough to do on Enter.
        (id, _) if id == IDOK.0 as usize => focus_list(),
        (id, _) if id == IDCANCEL.0 as usize => {
            if search_text(h.search).is_empty() {
                emit(Event::Leave);
            } else {
                // SAFETY: our own control; EN_CHANGE clears the search.
                unsafe {
                    let _ = SetWindowTextW(h.search, w!(""));
                }
            }
        }
        (ID_REMOVE, BN_CLICKED) => remove(),
        (ID_COPY, BN_CLICKED) => copy_fingerprint(),
        (ID_RELOAD, BN_CLICKED) => emit(Event::Reload),
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
            NM_SETFOCUS => emit(Event::Focused),
            LVN_KEYDOWN => {
                let key = &*(lparam.0 as *const NMLVKEYDOWN);
                if key.wVKey == VK_DELETE.0 {
                    remove();
                } else if key.wVKey == u16::from(b'C') && key_down(VK_CONTROL.0) {
                    copy_fingerprint();
                } else if key.wVKey == u16::from(b'A') && key_down(VK_CONTROL.0) {
                    let n = STATE.with(|s| {
                        s.try_borrow()
                            .ok()
                            .and_then(|s| s.as_ref()?.rows.as_ref().map(Vec::len))
                            .unwrap_or(0)
                    });
                    for i in 0..n {
                        set_state(h.list, i as i32, true);
                    }
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

/// The list's right-click menu (or Shift+F10, the menu key): copy the
/// fingerprint or the line, or remove the picked rows.
fn context_menu(h: Handles, lparam: LPARAM) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
    use windows::Win32::UI::Controls::{
        LVHITTESTINFO, LVIR_LABEL, LVM_GETITEMRECT, LVM_SUBITEMHITTEST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY,
        TPM_RETURNCMD, TrackPopupMenu,
    };
    const COPY_FP: u32 = 1;
    const COPY_LINE: u32 = 2;
    const REMOVE: u32 = 3;
    // SAFETY: our own control and a popup menu of our own, destroyed after
    // use; each struct lives through its call.
    unsafe {
        let keyboard = lparam.0 as i32 == -1;
        let mut at;
        let index = if keyboard {
            let i = selected_indexes(h.list).first().copied();
            let mut rc = RECT {
                left: LVIR_LABEL as i32,
                ..Default::default()
            };
            at = POINT::default();
            if let Some(i) = i
                && SendMessageW(
                    h.list,
                    LVM_GETITEMRECT,
                    Some(WPARAM(i as usize)),
                    Some(LPARAM(&raw mut rc as isize)),
                )
                .0 != 0
            {
                at = POINT {
                    x: rc.left,
                    y: rc.bottom,
                };
            }
            let _ = ClientToScreen(h.list, &mut at);
            i
        } else {
            at = POINT {
                x: i32::from(lparam.0 as u16 as i16),
                y: i32::from((lparam.0 >> 16) as u16 as i16),
            };
            let mut hit = LVHITTESTINFO {
                pt: at,
                ..Default::default()
            };
            let _ = ScreenToClient(h.list, &mut hit.pt);
            let i = SendMessageW(
                h.list,
                LVM_SUBITEMHITTEST,
                Some(WPARAM(0)),
                Some(LPARAM(&raw mut hit as isize)),
            )
            .0 as i32;
            (i >= 0).then_some(i)
        };
        let Some(index) = index else {
            return;
        };
        let Some(pick) = pick_at(h.list, index) else {
            return;
        };
        // A right click on a row that is not picked picks it alone.
        if !selected_indexes(h.list).contains(&index) {
            for i in selected_indexes(h.list) {
                set_state(h.list, i, false);
            }
            set_state(h.list, index, true);
        }
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            COPY_FP as usize,
            w!("Copy &fingerprint\tCtrl+C"),
        );
        let _ = AppendMenuW(menu, MF_STRING, COPY_LINE as usize, w!("Copy &line"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, REMOVE as usize, w!("&Remove...\tDel"));
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            at.x,
            at.y,
            None,
            h.panel,
            None,
        );
        let _ = DestroyMenu(menu);
        match u32::try_from(chosen.0).unwrap_or(0) {
            COPY_FP => emit(Event::CopyFingerprint(pick)),
            COPY_LINE => emit(Event::CopyLine(pick)),
            REMOVE => remove(),
            _ => {}
        }
    }
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, total, note)) = STATE.with(|s| {
        s.try_borrow()
            .ok()?
            .as_ref()
            .map(|s| (s.title_font, s.font, s.dpi, s.total, s.note.clone()))
    }) else {
        return;
    };
    // SAFETY: a BeginPaint/EndPaint pair on our own window; the font is
    // selected back out.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h.panel, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(h.panel, &mut rc);
        FillRect(dc, &rc, crate::ui_theme::sys_brush(COLOR_WINDOW));
        let old = SelectObject(dc, HGDIOBJ(title_font.0));
        SetBkMode(dc, TRANSPARENT);
        let pad = scale(10, dpi);
        let mut tr = RECT {
            left: pad,
            top: 0,
            right: rc.right - pad,
            bottom: scale(36, dpi),
        };
        SetTextColor(
            dc,
            windows::Win32::Foundation::COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT)),
        );
        let mut text: Vec<u16> = "Known hosts".encode_utf16().collect();
        DrawTextW(
            dc,
            &mut text,
            &mut tr,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if total > 0 {
            SetTextColor(
                dc,
                windows::Win32::Foundation::COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT)),
            );
            let mut count: Vec<u16> = total.to_string().encode_utf16().collect();
            DrawTextW(
                dc,
                &mut count,
                &mut tr,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        if !note.is_empty() {
            // Between the list and the buttons (see `place`).
            let bottom = rc.bottom - scale(8 + 28 + 6, dpi);
            let mut nr = RECT {
                left: pad,
                top: bottom - scale(NOTE_DIP, dpi),
                right: rc.right - pad,
                bottom,
            };
            SelectObject(dc, HGDIOBJ(font.0));
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x0000_30C0));
            let mut text: Vec<u16> = note.encode_utf16().collect();
            DrawTextW(
                dc,
                &mut text,
                &mut nr,
                DT_LEFT
                    | DT_VCENTER
                    | DT_SINGLELINE
                    | DT_NOPREFIX
                    | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS,
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
            let from = HWND(lparam.0 as *mut _);
            // Only the panel's own search box and buttons, or the dialog
            // keys (Enter, Escape), which send no control.
            if from.is_invalid() || from == h.search || h.buttons.contains(&from) {
                command(h, wparam.0 & 0xFFFF, ((wparam.0 >> 16) & 0xFFFF) as u32);
            }
            LRESULT(0)
        }
        (WM_NOTIFY, Some(h)) => notify(h, lparam),
        (windows::Win32::UI::WindowsAndMessaging::WM_CONTEXTMENU, Some(h))
            if HWND(wparam.0 as *mut _) == h.list =>
        {
            context_menu(h, lparam);
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
