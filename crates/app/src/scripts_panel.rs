//! Scripts as native Windows controls: a search box, a list view of the
//! scripts grouped by category (name, keys, first line; the ones for
//! other hosts dimmed), the selected script's lines under it, and Run,
//! Edit, New, Delete and On hosts buttons, on a panel over the uncovered
//! part of the tools pane.
//!
//! The rows come from `SnippetsView::native_rows`; what the user does
//! goes back to the main window as the [`SideKey`] the drawn view takes,
//! so a script runs, opens in the editor or asks for its arguments
//! exactly as before. The editor, the argument page, the host picker and
//! a run's report are still drawn: the panel hides while one is open.
//!
//! Keys work as in the other native tools: [`pre_translate`] runs in the
//! message loop, passes Ctrl+Shift shortcuts on to the main window, and
//! gives Tab, Enter and Escape their dialog meaning. Enter runs the
//! selected script, Shift+Enter without its last Enter; F2 edits, Insert
//! adds, Del removes, F5 runs it on picked hosts. Typing in the list
//! searches; Up and Down in the search box move in the list.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW,
    EndPaint, FillRect, HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT,
    EM_SETCUEBANNER, EM_SETSEL, LIST_VIEW_ITEM_STATE_FLAGS, LVCDI_GROUP, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVGF_GROUPID, LVGF_HEADER, LVGGR_HEADER, LVGROUP, LVIF_GROUPID, LVIF_PARAM,
    LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_ENABLEGROUPVIEW,
    LVM_ENSUREVISIBLE, LVM_GETGROUPRECT, LVM_GETITEMW, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW,
    LVM_INSERTGROUP, LVM_INSERTITEMW, LVM_REMOVEALLGROUPS, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_ITEMCHANGED, LVN_KEYDOWN,
    LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_CUSTOMDRAW, NM_DBLCLK, NMHDR, NMLVCUSTOMDRAW, NMLVKEYDOWN,
    SetWindowTheme, WC_BUTTONW, WC_EDITW, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, SetFocus, VK_CONTROL, VK_DELETE, VK_DOWN, VK_F2, VK_F5, VK_INSERT,
    VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    EN_CHANGE, ES_AUTOHSCROLL, GetClientRect, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsChild,
    IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow, RegisterClassW, SW_HIDE,
    SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR,
    WM_COMMAND, WM_CONTEXTMENU, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_PAINT, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};
use crate::sessions::SideKey;
use crate::snippets_view::NativeScript;

/// What the user did in the panel.
#[derive(Debug)]
pub enum Event {
    /// The script at this place among the shown ones was selected.
    Select(usize),
    /// The search box now says this.
    Filter(String),
    /// A key for the scripts view, with the selected script's place
    /// first: Enter runs, F2 edits, New, Delete, Refresh (on hosts),
    /// Escape leaves.
    Key(Option<usize>, SideKey),
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_SEARCH: usize = 400;
const ID_LIST: usize = 401;
const ID_RUN: usize = 410;
const ID_EDIT: usize = 411;
const ID_NEW: usize = 412;
const ID_DELETE: usize = 413;
const ID_HOSTS: usize = 414;

/// Columns: title and width in DIPs.
const COLUMNS: [(&str, i32); 3] = [("Name", 150), ("Keys", 70), ("First line", 220)];

/// The buttons' labels and ids, left to right.
const BUTTONS: [(PCWSTR, usize); 5] = [
    (w!("Run"), ID_RUN),
    (w!("Edit"), ID_EDIT),
    (w!("New"), ID_NEW),
    (w!("Delete"), ID_DELETE),
    (w!("On hosts…"), ID_HOSTS),
];

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    search: HWND,
    list: HWND,
    /// Run, Edit, New, Delete, On hosts.
    buttons: [HWND; 5],
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    /// The rows as shown, to rebuild the list only when they change.
    rows: Option<Vec<NativeScript>>,
    /// How many scripts there are, shown or not.
    total: usize,
    /// The list or the search box is being filled from the view: what
    /// that changes is not news to it.
    filling: bool,
    /// Where the selected script's lines are painted.
    details_at: RECT,
    /// The list's category headings by group id, with how many scripts
    /// each has; drawn here in the window's text colour.
    groups: Vec<(String, usize)>,
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
    with(|s| s.filling).unwrap_or(false)
}

/// Creates the panel, hidden, as a child of `main`.
pub fn create(main: HWND, handler: Handler, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering a class and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHScripts");
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
        let cue = wide("Search names, categories and lines");
        SendMessageW(
            search,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
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
            let style = if id == ID_RUN {
                BS_DEFPUSHBUTTON
            } else {
                BS_PUSHBUTTON
            };
            *b = child(WC_BUTTONW, label, style as u32, id)?;
        }
        let h = Handles {
            main,
            panel,
            search,
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
                rows: None,
                total: 0,
                filling: false,
                details_at: RECT::default(),
                groups: Vec::new(),
            });
        });
        enable_buttons(&h);
    }
    if let Some(h) = handles() {
        pass_edge(h.panel);
    }
    Ok(())
}

fn controls(h: &Handles) -> Vec<HWND> {
    let mut v = vec![h.search, h.list];
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
        let old = (
            std::mem::replace(&mut s.font, font),
            std::mem::replace(&mut s.title_font, title),
        );
        Some((font, old))
    })
    .flatten();
    if let Some((font, (old, old_title))) = refont {
        for c in controls(&h) {
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
            let _ = DeleteObject(HGDIOBJ(old.0));
            let _ = DeleteObject(HGDIOBJ(old_title.0));
        }
    }
    let pad = scale(8, dpi);
    let title = scale(36, dpi);
    let field = scale(26, dpi);
    let button = scale(28, dpi);
    let gap = scale(6, dpi);
    let details = scale(96, dpi);
    let inner = (width - 2 * pad).max(0);
    let list_y = title + field + gap;
    let buttons_y = (at.h - pad - button).max(list_y + details + gap);
    let details_y = buttons_y - gap - details;
    let list_h = (details_y - gap - list_y).max(0);
    let n = BUTTONS.len() as i32;
    let button_w = ((inner - gap * (n - 1)) / n).clamp(0, scale(84, dpi));
    with(|s| {
        s.details_at = RECT {
            left: pad,
            top: details_y,
            right: pad + inner,
            bottom: details_y + details,
        };
    });
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.search, pad, title, inner, field, true);
        let _ = MoveWindow(h.list, pad, list_y, inner, list_h, true);
        let mut x = pad;
        for b in h.buttons {
            let _ = MoveWindow(b, x, buttons_y, button_w, button, true);
            x += button_w + gap;
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

/// Gives the list the keyboard when it lists anything, else the search
/// box.
pub fn focus_first() {
    let Some(h) = handles() else {
        return;
    };
    let any = with(|s| s.rows.as_ref().is_some_and(|r| !r.is_empty())).unwrap_or(false);
    // SAFETY: our own controls.
    unsafe {
        let _ = SetFocus(Some(if any { h.list } else { h.search }));
    }
}

/// Shows `rows` with the one at `selected` selected, `filter` in the
/// search box, and `total`, how many scripts there are, in the title.
pub fn set_view(rows: Vec<NativeScript>, selected: usize, filter: &str, total: usize) {
    let Some(h) = handles() else {
        return;
    };
    let typed = search_text(h.search);
    let rebuild = with(|s| {
        s.filling = true;
        s.total = total;
        if s.rows.as_ref() == Some(&rows) {
            return false;
        }
        s.rows = Some(rows.clone());
        true
    })
    .unwrap_or(false);
    // SAFETY: our own windows; each struct and its text live through the
    // call, which copies them.
    unsafe {
        if typed != filter {
            // The view's filter changed without the box: New took it as
            // the name, or a paste went to it.
            let text = wide(filter);
            let _ = SetWindowTextW(h.search, PCWSTR(text.as_ptr()));
            SendMessageW(
                h.search,
                EM_SETSEL,
                Some(WPARAM(0x7FFF_FFFF)),
                Some(LPARAM(0x7FFF_FFFF)),
            );
        }
        if rebuild {
            fill_list(&h, &rows);
        }
        let current = selected_index(h.list)
            .and_then(|i| row_at(h.list, i))
            .map(|r| r.pos);
        if (rebuild || current != Some(selected)) && selected < rows.len() {
            set_state(h.list, selected as i32, true);
        }
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
    with(|s| s.filling = false);
    enable_buttons(&h);
}

/// Fills the list with `rows`, under a heading per category when any
/// has one.
fn fill_list(h: &Handles, rows: &[NativeScript]) {
    let grouped = rows.iter().any(|r| !r.category.is_empty());
    // The groups, in the rows' order: those without a category first.
    let mut groups: Vec<&str> = Vec::new();
    for r in rows {
        if !groups.iter().any(|g| g.eq_ignore_ascii_case(&r.category)) {
            groups.push(&r.category);
        }
    }
    let headings: Vec<(String, usize)> = groups
        .iter()
        .map(|g| {
            let name = if g.is_empty() { "No category" } else { g };
            let n = rows
                .iter()
                .filter(|r| r.category.eq_ignore_ascii_case(g))
                .count();
            (name.to_owned(), n)
        })
        .collect();
    with(|s| s.groups = headings);
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        SendMessageW(h.list, LVM_DELETEALLITEMS, None, None);
        SendMessageW(h.list, LVM_REMOVEALLGROUPS, None, None);
        SendMessageW(
            h.list,
            LVM_ENABLEGROUPVIEW,
            Some(WPARAM(usize::from(grouped))),
            None,
        );
        if grouped {
            for (id, g) in groups.iter().enumerate() {
                let mut header = wide(if g.is_empty() { "No category" } else { g });
                let group = LVGROUP {
                    cbSize: size_of::<LVGROUP>() as u32,
                    mask: LVGF_HEADER | LVGF_GROUPID,
                    pszHeader: PWSTR(header.as_mut_ptr()),
                    iGroupId: id as i32,
                    ..Default::default()
                };
                SendMessageW(
                    h.list,
                    LVM_INSERTGROUP,
                    Some(WPARAM(usize::MAX)),
                    Some(LPARAM(&raw const group as isize)),
                );
            }
        }
        for (i, r) in rows.iter().enumerate() {
            let group = groups
                .iter()
                .position(|g| g.eq_ignore_ascii_case(&r.category))
                .unwrap_or(0);
            let mut label = wide(&r.name);
            let item = LVITEMW {
                mask: LVIF_TEXT
                    | LVIF_PARAM
                    | if grouped {
                        LVIF_GROUPID
                    } else {
                        Default::default()
                    },
                iItem: i as i32,
                pszText: PWSTR(label.as_mut_ptr()),
                lParam: LPARAM(i as isize),
                iGroupId: group as i32,
                ..Default::default()
            };
            let at = SendMessageW(
                h.list,
                LVM_INSERTITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const item as isize)),
            )
            .0;
            for (sub, text) in [&r.keys, &r.first].into_iter().enumerate() {
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

/// The row shown at list index `index`.
fn row_at(list: HWND, index: i32) -> Option<NativeScript> {
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
    let i = usize::try_from(item.lParam.0).ok().filter(|_| ok.0 != 0)?;
    with(|s| s.rows.as_ref()?.get(i).cloned()).flatten()
}

fn selected_row() -> Option<NativeScript> {
    let h = handles()?;
    row_at(h.list, selected_index(h.list)?)
}

/// New always; the others for a selected script.
fn enable_buttons(h: &Handles) {
    let any = selected_row().is_some();
    // SAFETY: our own controls.
    unsafe {
        for (b, (_, id)) in h.buttons.iter().zip(BUTTONS) {
            let _ = EnableWindow(*b, any || id == ID_NEW);
        }
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
}

fn emit(e: Event) {
    let Some((main, handler)) = with(|s| (s.h.main, s.handler)) else {
        return;
    };
    handler(main, e);
}

/// `key` for the selected script (New and Escape need none).
fn key(key: SideKey) {
    let pos = selected_row().map(|r| r.pos);
    let needs = !matches!(key, SideKey::New | SideKey::Escape);
    if needs && pos.is_none() {
        return;
    }
    emit(Event::Key(pos, key));
}

fn run(split: bool) {
    key(SideKey::Enter { split });
}

/// Escape: empties the search box, else back to the terminal.
fn escape(h: &Handles) {
    if search_text(h.search).is_empty() {
        key(SideKey::Escape);
    } else {
        // SAFETY: our own control; its EN_CHANGE tells the view.
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
            WM_KEYDOWN if key_down(VK_CONTROL.0) && key_down(VK_SHIFT.0) => {
                let mut m = *msg;
                m.hwnd = h.main;
                let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&m);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&m);
                return true;
            }
            // Up and Down in the search box move in the list.
            WM_KEYDOWN
                if msg.hwnd == h.search
                    && [VK_UP, VK_DOWN, VK_PRIOR, VK_NEXT]
                        .iter()
                        .any(|k| usize::from(k.0) == msg.wParam.0) =>
            {
                SendMessageW(h.list, WM_KEYDOWN, Some(msg.wParam), Some(msg.lParam));
                return true;
            }
            // Left and Right in the list jump between categories.
            WM_KEYDOWN
                if msg.hwnd == h.list
                    && (msg.wParam.0 == usize::from(VK_LEFT.0)
                        || msg.wParam.0 == usize::from(VK_RIGHT.0)) =>
            {
                key(if msg.wParam.0 == usize::from(VK_LEFT.0) {
                    SideKey::Left
                } else {
                    SideKey::Right
                });
                return true;
            }
            // Typing in the list searches.
            WM_CHAR
                if msg.hwnd == h.list
                    && msg.wParam.0 >= 0x20
                    && msg.wParam.0 != 0x7F
                    && !key_down(VK_CONTROL.0)
                    && !key_down(VK_MENU.0) =>
            {
                let _ = SetFocus(Some(h.search));
                SendMessageW(
                    h.search,
                    EM_SETSEL,
                    Some(WPARAM(0x7FFF_FFFF)),
                    Some(LPARAM(0x7FFF_FFFF)),
                );
                SendMessageW(h.search, WM_CHAR, Some(msg.wParam), Some(msg.lParam));
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
        (ID_SEARCH, EN_CHANGE) => {
            if !filling() {
                emit(Event::Filter(search_text(h.search)));
            }
        }
        (id, _) if id == IDOK.0 as usize => run(key_down(VK_SHIFT.0)),
        (id, _) if id == IDCANCEL.0 as usize => escape(&h),
        (ID_RUN, BN_CLICKED) => run(false),
        (ID_EDIT, BN_CLICKED) => key(SideKey::Edit),
        (ID_NEW, BN_CLICKED) => key(SideKey::New),
        (ID_DELETE, BN_CLICKED) => key(SideKey::Delete),
        (ID_HOSTS, BN_CLICKED) => key(SideKey::Refresh),
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
                if stage == CDDS_PREPAINT && cd.dwItemType == LVCDI_GROUP {
                    // The theme draws headings in its own blue, which a
                    // dark window colour makes hard to read.
                    draw_heading(h.list, cd);
                    return LRESULT(CDRF_SKIPDEFAULT as isize);
                }
                if stage == CDDS_PREPAINT {
                    return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
                }
                if stage == CDDS_ITEMPREPAINT {
                    // Scripts for other hosts are dimmed.
                    let i = cd.nmcd.lItemlParam.0;
                    let other = with(|s| {
                        s.rows
                            .as_ref()
                            .and_then(|r| r.get(usize::try_from(i).ok()?))
                            .is_some_and(|r| r.other_host)
                    })
                    .unwrap_or(false);
                    if other {
                        cd.clrText = COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT));
                    }
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
            NM_DBLCLK => run(false),
            LVN_ITEMCHANGED => {
                if !filling()
                    && let Some(r) = selected_row()
                {
                    emit(Event::Select(r.pos));
                }
                enable_buttons(&h);
            }
            LVN_KEYDOWN => {
                let k = &*(lparam.0 as *const NMLVKEYDOWN);
                let vk = k.wVKey;
                match vk {
                    v if v == VK_F2.0 => key(SideKey::Edit),
                    v if v == VK_INSERT.0 => key(SideKey::New),
                    v if v == VK_DELETE.0 => key(SideKey::Delete),
                    v if v == VK_F5.0 => key(SideKey::Refresh),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

/// Draws a category heading in the window's text colour, its count in
/// grey after it, and a thin line to the right edge.
fn draw_heading(list: HWND, cd: &NMLVCUSTOMDRAW) {
    let id = cd.nmcd.dwItemSpec;
    let Some((name, count)) = with(|s| s.groups.get(id).cloned()).flatten() else {
        return;
    };
    draw_group_heading(list, cd, &name, &count.to_string());
}

/// Draws group heading `cd` of `list` in the window's colours: `name`,
/// `count` in grey and a thin line to the right edge.
pub(crate) fn draw_group_heading(list: HWND, cd: &NMLVCUSTOMDRAW, name: &str, count: &str) {
    use windows::Win32::Graphics::Gdi::{
        COLOR_BTNSHADOW, DT_CALCRECT, HDC, PatBlt, SelectObject as Select,
    };
    use windows::Win32::UI::WindowsAndMessaging::WM_GETFONT;
    let id = cd.nmcd.dwItemSpec;
    let mut rc = RECT {
        top: LVGGR_HEADER as i32,
        ..Default::default()
    };
    // SAFETY: the list's own DC for this paint, and our own control; the
    // font is selected back out.
    unsafe {
        let dc: HDC = cd.nmcd.hdc;
        if SendMessageW(
            list,
            LVM_GETGROUPRECT,
            Some(WPARAM(id)),
            Some(LPARAM(&raw mut rc as isize)),
        )
        .0 == 0
        {
            rc = cd.rcText;
        }
        FillRect(dc, &rc, crate::ui_theme::sys_brush(COLOR_WINDOW));
        let font = SendMessageW(list, WM_GETFONT, None, None).0;
        let old = Select(dc, HGDIOBJ(font as *mut _));
        SetBkMode(dc, TRANSPARENT);
        let pad = 6;
        let mut at = RECT {
            left: rc.left + pad,
            ..rc
        };
        let flags = DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        let mut text: Vec<u16> = name.encode_utf16().collect();
        SetTextColor(dc, COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT)));
        let mut size = at;
        DrawTextW(dc, &mut text, &mut size, flags | DT_CALCRECT);
        DrawTextW(dc, &mut text, &mut at, flags);
        at.left = size.right + pad;
        let mut n: Vec<u16> = count.encode_utf16().collect();
        SetTextColor(dc, COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT)));
        let mut size = at;
        DrawTextW(dc, &mut n, &mut size, flags | DT_CALCRECT);
        DrawTextW(dc, &mut n, &mut at, flags);
        // A one-pixel line in the middle of the rest of the heading.
        let left = size.right + pad;
        let y = (rc.top + rc.bottom) / 2;
        if left < rc.right - pad {
            let old_brush = Select(dc, HGDIOBJ(crate::ui_theme::sys_brush(COLOR_BTNSHADOW).0));
            let _ = PatBlt(
                dc,
                left,
                y,
                rc.right - pad - left,
                1,
                windows::Win32::Graphics::Gdi::PATCOPY,
            );
            Select(dc, old_brush);
        }
        Select(dc, old);
    }
}

/// The list's right-click menu (or Shift+F10, the menu key) for the
/// script under the mouse or selected; New when there is none.
fn context_menu(h: Handles, lparam: LPARAM) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
    use windows::Win32::UI::Controls::{
        LVHITTESTINFO, LVIR_LABEL, LVM_GETITEMRECT, LVM_SUBITEMHITTEST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, MF_GRAYED, MF_SEPARATOR, MF_STRING,
        TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu,
    };
    const RUN: u32 = 1;
    const RUN_SPLIT: u32 = 2;
    const EDIT: u32 = 3;
    const NEW: u32 = 4;
    const DELETE: u32 = 5;
    const HOSTS: u32 = 6;
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
        let row = index.and_then(|i| row_at(h.list, i));
        if let (Some(i), Some(_)) = (index, &row) {
            set_state(h.list, i, true);
        }
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let any = row.is_some();
        let flag = if any {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        };
        let _ = AppendMenuW(menu, flag, RUN as usize, w!("&Run\tEnter"));
        let _ = AppendMenuW(
            menu,
            flag,
            RUN_SPLIT as usize,
            w!("Run &without the last Enter\tShift+Enter"),
        );
        let _ = AppendMenuW(menu, flag, HOSTS as usize, w!("Run on &hosts…\tF5"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, flag, EDIT as usize, w!("&Edit\tF2"));
        let _ = AppendMenuW(menu, MF_STRING, NEW as usize, w!("&New\tIns"));
        let _ = AppendMenuW(menu, flag, DELETE as usize, w!("&Delete\tDel"));
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
        let chosen = u32::try_from(chosen.0).unwrap_or(0);
        if chosen == NEW {
            key(SideKey::New);
            return;
        }
        // The menu ran its own message loop: the list may have changed,
        // so the script is found again by its name and category.
        let Some(row) = row else {
            return;
        };
        // The same row if it is still there, else one of that name in
        // that category.
        let again = with(|s| {
            let rows = s.rows.as_ref()?;
            rows.iter().position(|r| *r == row).or_else(|| {
                rows.iter()
                    .position(|r| r.name == row.name && r.category == row.category)
            })
        })
        .flatten();
        let Some(i) = again else {
            return;
        };
        set_state(h.list, i as i32, true);
        match chosen {
            RUN => run(false),
            RUN_SPLIT => run(true),
            EDIT => key(SideKey::Edit),
            DELETE => key(SideKey::Delete),
            HOSTS => key(SideKey::Refresh),
            _ => {}
        }
    }
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, shown, total, details_at)) = with(|s| {
        (
            s.title_font,
            s.font,
            s.dpi,
            s.rows.as_ref().map_or(0, Vec::len),
            s.total,
            s.details_at,
        )
    }) else {
        return;
    };
    let details = match selected_row() {
        Some(r) => r.details,
        None if total == 0 => "No scripts yet. New adds one.".to_owned(),
        None if shown == 0 => "Nothing matches.".to_owned(),
        None => String::new(),
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
        let text_colour = COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT));
        let grey = COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT));
        SetTextColor(dc, text_colour);
        let mut text: Vec<u16> = "Scripts".encode_utf16().collect();
        let line = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        DrawTextW(dc, &mut text, &mut tr, DT_LEFT | line);
        if total > 0 {
            SetTextColor(dc, grey);
            let count = if shown == total {
                total.to_string()
            } else {
                format!("{shown} of {total}")
            };
            let mut n: Vec<u16> = count.encode_utf16().collect();
            DrawTextW(dc, &mut n, &mut tr, DT_RIGHT | line);
        }
        SelectObject(dc, HGDIOBJ(font.0));
        if !details.is_empty() {
            SetTextColor(dc, grey);
            let mut d: Vec<u16> = details.encode_utf16().collect();
            let mut at = details_at;
            DrawTextW(
                dc,
                &mut d,
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
        (WM_CONTEXTMENU, Some(h)) if HWND(wparam.0 as *mut _) == h.list => {
            context_menu(h, lparam);
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
