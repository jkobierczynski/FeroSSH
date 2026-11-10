//! The session manager as native Windows controls: a search box, a list
//! view of the open sessions in columns (session, kind, tab, state,
//! details), and Show, Rename and Close buttons, on a panel over the
//! uncovered part of the tools pane.
//!
//! The rows come from the main window's `LiveView::rows`; what the user
//! does goes back as an [`Event`]. F2, the Rename button and Window >
//! Rename session edit a session's name in place, in the list itself.
//! Keys work as in `conn_view`: [`pre_translate`] runs in the message
//! loop, passes Ctrl+Shift shortcuts on to the main window, turns typing
//! in the list into a search, and gives Tab, Enter and Escape their dialog
//! meaning (but not while a name is being edited).

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, HFONT, HGDIOBJ,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    EM_SETCUEBANNER, EM_SETSEL, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
    LVIF_PARAM, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS,
    LVM_EDITLABELW, LVM_ENSUREVISIBLE, LVM_GETEDITCONTROL, LVM_GETITEMW, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_ENDLABELEDITW, LVN_KEYDOWN, LVNI_SELECTED,
    LVS_EDITLABELS, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_DBLCLK, NM_SETFOCUS, NMHDR, NMLVDISPINFOW, NMLVKEYDOWN,
    SetWindowTheme, WC_BUTTONW, WC_EDITW, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_CONTROL, VK_DELETE, VK_DOWN, VK_F2, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    EN_CHANGE, EN_SETFOCUS, ES_AUTOHSCROLL, GetClientRect, GetWindowTextLengthW, HMENU, IDC_ARROW,
    IDCANCEL, IDOK, IsChild, IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow,
    PostMessageW, RegisterClassW, SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN,
    WM_NOTIFY, WM_PAINT, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::layout::{PaneId, Rect};
use crate::live::LiveRow;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};

/// What the user did in the panel.
#[derive(Debug)]
pub enum Event {
    /// Show the session (Enter, a double click, the Show button).
    Show(PaneId),
    /// Close it, after asking (Del, the Close button).
    Close(PaneId),
    /// A name was typed for it (`text` as typed; empty clears the name).
    Rename(PaneId, String),
    /// Escape ended the typing of a name.
    RenameCancelled,
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
const ID_SHOW: usize = 210;
const ID_RENAME: usize = 211;
const ID_CLOSE: usize = 212;

/// Posted to the panel itself: a name edit ended. Handled after the list
/// view's notification returns, since the rename rebuilds the list.
const WM_EDITED: u32 = WM_APP + 1;

/// Columns: title and width in DIPs.
const COLUMNS: [(&str, i32); 5] = [
    ("Session", 150),
    ("Kind", 56),
    ("Tab", 36),
    ("State", 84),
    ("Details", 220),
];

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
    rows: Option<Vec<LiveRow>>,
    /// All open sessions (the title's count).
    total: usize,
    /// A name edit that ended, waiting for `WM_EDITED`: `None` inside
    /// when it was cancelled.
    edited: Option<(PaneId, Option<String>)>,
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
        let class = w!("FeroSSHSessions");
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
        let cue = wide("Search sessions");
        SendMessageW(
            search,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        let list = child(
            WC_LISTVIEWW,
            w!(""),
            LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_EDITLABELS | LVS_NOSORTHEADER,
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
        let buttons = [
            child(WC_BUTTONW, w!("Show"), BS_DEFPUSHBUTTON as u32, ID_SHOW)?,
            child(WC_BUTTONW, w!("Rename"), BS_PUSHBUTTON as u32, ID_RENAME)?,
            child(WC_BUTTONW, w!("Close..."), BS_PUSHBUTTON as u32, ID_CLOSE)?,
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
                edited: None,
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
    let list_h = (buttons_y - gap - list_y).max(0);
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.search, pad, title, inner, field, true);
        let _ = MoveWindow(h.list, pad, list_y, inner, list_h, true);
        let mut x = pad;
        for (b, w) in h.buttons.iter().zip([64, 72, 72]) {
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

/// A session's name is being edited in the list.
pub fn editing() -> bool {
    // SAFETY: plain message to our own control.
    handles()
        .is_some_and(|h| unsafe { SendMessageW(h.list, LVM_GETEDITCONTROL, None, None).0 != 0 })
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

/// The selected row's index.
fn selected_index(list: HWND) -> Option<i32> {
    // SAFETY: plain message to our own control.
    let i = unsafe {
        SendMessageW(
            list,
            LVM_GETNEXTITEM,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(LVNI_SELECTED as isize)),
        )
    }
    .0 as i32;
    (i >= 0).then_some(i)
}

fn pane_at(list: HWND, index: i32) -> Option<PaneId> {
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
    (ok.0 != 0).then(|| PaneId::try_from(item.lParam.0).unwrap_or(0))
}

fn selected_pane() -> Option<PaneId> {
    let h = handles()?;
    pane_at(h.list, selected_index(h.list)?)
}

/// The row of `pane`, if it is listed.
fn index_of(pane: PaneId) -> Option<i32> {
    STATE.with(|s| {
        let s = s.try_borrow().ok()?;
        let rows = s.as_ref()?.rows.as_ref()?;
        rows.iter()
            .position(|r| r.pane == pane)
            .and_then(|i| i32::try_from(i).ok())
    })
}

/// Shows `rows` (of `total` open sessions), unless it shows them already.
/// The selection stays on its session; with none, it starts on the
/// focused pane's.
pub fn set_rows(rows: Vec<LiveRow>, total: usize) {
    let Some(h) = handles() else {
        return;
    };
    // A rebuild would end a name edit.
    if editing() {
        return;
    }
    let keep = selected_pane();
    let changed = STATE.with(|s| {
        let mut s = s.try_borrow_mut().ok()?;
        let s = s.as_mut()?;
        let title = s.total != total;
        s.total = total;
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
            let mut label = wide(&r.label);
            let item = LVITEMW {
                mask: LVIF_TEXT | LVIF_PARAM,
                iItem: i as i32,
                pszText: PWSTR(label.as_mut_ptr()),
                lParam: LPARAM(r.pane as isize),
                ..Default::default()
            };
            let at = SendMessageW(
                h.list,
                LVM_INSERTITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const item as isize)),
            )
            .0;
            for (sub, text) in [r.kind, &r.tab, r.state, &r.details].iter().enumerate() {
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
    let pick = keep
        .and_then(index_of)
        .or_else(|| rows.iter().position(|r| r.focused).map(|i| i as i32))
        .or((!rows.is_empty()).then_some(0));
    if let Some(i) = pick {
        set_state(h.list, i, true);
    }
}

/// Shows `query` in the search box when the view changed it (a reset or a
/// rename of a session the filter hid). The box's change event then
/// carries the same text.
pub fn set_search(query: &str) {
    let Some(h) = handles() else {
        return;
    };
    // The view keeps 60 characters; longer typing is left alone.
    let shown: String = search_text(h.search).chars().take(60).collect();
    if shown != query {
        let text = wide(query);
        // SAFETY: our own control; the text lives through the call.
        unsafe {
            let _ = SetWindowTextW(h.search, PCWSTR(text.as_ptr()));
        }
    }
}

/// Gives the list the keyboard.
pub fn focus_list() {
    if let Some(h) = handles() {
        // SAFETY: our own control.
        unsafe {
            let _ = SetFocus(Some(h.list));
        }
    }
}

/// Starts editing `pane`'s name in place (F2, Rename, Window > Rename
/// session).
pub fn edit_name(pane: PaneId) {
    let Some(h) = handles() else {
        return;
    };
    let Some(i) = index_of(pane) else {
        return;
    };
    set_state(h.list, i, true);
    // SAFETY: our own control; it takes the keyboard for the edit.
    unsafe {
        let _ = SetFocus(Some(h.list));
        SendMessageW(h.list, LVM_EDITLABELW, Some(WPARAM(i as usize)), None);
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
        // While a name is edited, its edit box gets every key: Enter keeps
        // the name and Escape drops it.
        if editing() {
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

/// Enter or Show: the selected session, or from the search box the
/// first one listed.
fn show(h: Handles, from_search: bool) {
    let pane = if from_search {
        pane_at(h.list, 0)
    } else {
        selected_pane().or_else(|| pane_at(h.list, 0))
    };
    if let Some(p) = pane {
        emit(Event::Show(p));
    }
}

/// Whether a `WM_COMMAND` came from the panel's own search box or
/// buttons, or from the dialog keys (Enter, Escape), which send none.
/// The list passes its name edit's notifications on to us too, and that
/// box's id can be IDOK: its first change (one typed character) read as
/// Enter, showed the session and ended the rename.
fn ours(h: Handles, from: HWND) -> bool {
    from.is_invalid() || from == h.search || h.buttons.contains(&from)
}

fn command(h: Handles, id: usize, code: u32) {
    match (id, code) {
        (ID_SEARCH, EN_CHANGE) => emit(Event::Query(search_text(h.search))),
        (ID_SEARCH, EN_SETFOCUS) => emit(Event::Focused),
        (id, _) if id == IDOK.0 as usize => {
            // SAFETY: a plain query.
            let from_search = unsafe { GetFocus() } == h.search;
            show(h, from_search);
        }
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
        (ID_SHOW, BN_CLICKED) => show(h, false),
        (ID_RENAME, BN_CLICKED) => {
            if let Some(p) = selected_pane() {
                edit_name(p);
            }
        }
        (ID_CLOSE, BN_CLICKED) => {
            if let Some(p) = selected_pane() {
                emit(Event::Close(p));
            }
        }
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
            NM_DBLCLK => show(h, false),
            LVN_KEYDOWN => {
                let key = &*(lparam.0 as *const NMLVKEYDOWN);
                if key.wVKey == VK_F2.0 {
                    if let Some(p) = selected_pane() {
                        edit_name(p);
                    }
                } else if key.wVKey == VK_DELETE.0
                    && let Some(p) = selected_pane()
                {
                    emit(Event::Close(p));
                }
            }
            LVN_ENDLABELEDITW => {
                let info = &*(lparam.0 as *const NMLVDISPINFOW);
                let pane = pane_at(h.list, info.item.iItem);
                let text = (!info.item.pszText.is_null())
                    .then(|| info.item.pszText.to_string().unwrap_or_default());
                STATE.with(|s| {
                    if let Ok(mut s) = s.try_borrow_mut()
                        && let Some(s) = s.as_mut()
                    {
                        s.edited = pane.map(|p| (p, text));
                    }
                });
                // The rename rebuilds the list: after this returns.
                let _ = PostMessageW(Some(h.panel), WM_EDITED, WPARAM(0), LPARAM(0));
                // The list keeps its own text; the rebuild shows the name.
                return LRESULT(0);
            }
            _ => {}
        }
    }
    LRESULT(0)
}

/// The list's right-click menu (or Shift+F10, the menu key): Show,
/// Rename and Close for the session under the mouse or selected.
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
    const SHOW: u32 = 1;
    const RENAME: u32 = 2;
    const CLOSE: u32 = 3;
    // SAFETY: our own control and a popup menu of our own, destroyed after
    // use; each struct lives through its call.
    unsafe {
        let keyboard = lparam.0 as i32 == -1;
        let mut at;
        let index = if keyboard {
            let i = selected_index(h.list);
            let mut rc = RECT {
                left: LVIR_LABEL as i32,
                ..Default::default()
            };
            at = POINT::default();
            // Below the row's name; at the list's corner if that fails.
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
        // Only on a session; empty space has no menu.
        let Some(index) = index else {
            return;
        };
        let Some(pane) = pane_at(h.list, index) else {
            return;
        };
        set_state(h.list, index, true);
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, SHOW as usize, w!("&Show\tEnter"));
        let _ = AppendMenuW(menu, MF_STRING, RENAME as usize, w!("&Rename\tF2"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CLOSE as usize, w!("&Close...\tDel"));
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
            SHOW => emit(Event::Show(pane)),
            RENAME => edit_name(pane),
            CLOSE => emit(Event::Close(pane)),
            _ => {}
        }
    }
}

fn paint(h: Handles) {
    let Some((title_font, dpi, total)) = STATE.with(|s| {
        s.try_borrow()
            .ok()?
            .as_ref()
            .map(|s| (s.title_font, s.dpi, s.total))
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
        let mut text: Vec<u16> = "Sessions".encode_utf16().collect();
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
            if ours(h, from) {
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
        (WM_EDITED, Some(_)) => {
            let edited = STATE.with(|s| {
                s.try_borrow_mut()
                    .ok()?
                    .as_mut()
                    .and_then(|s| s.edited.take())
            });
            match edited {
                Some((pane, Some(text))) => emit(Event::Rename(pane, text)),
                _ => emit(Event::RenameCancelled),
            }
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
