//! Tools > Connection log as native Windows controls, on a panel over the
//! uncovered part of the tools pane like Known hosts: every connection
//! event (connected, disconnected, lost, failed, opened), newest first
//! (see `connlog_view.rs`).
//!
//! The search box takes words (`web`, `failed`, `-lost`, `"two words"`);
//! a click on a column heading sorts by it (again: the other way). Save
//! CSV asks where to write the rows shown, in their order. Connect (Enter
//! in the list, a double click) opens the picked entry's connection;
//! Reload (F5) reads the log again. Delete... (the Delete key) removes the
//! picked entries after a question; it is not there when machine policy
//! keeps every entry. Keys work as in `known_hosts_panel`:
//! [`pre_translate`] runs in the message loop, passes Ctrl+Shift shortcuts
//! on to the main window, turns typing in the list into a search, and
//! gives Tab, Enter and Escape their dialog meaning.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect,
    HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    EM_LIMITTEXT, EM_SETCUEBANNER, EM_SETSEL, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVIF_PARAM, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS,
    LVM_ENSUREVISIBLE, LVM_GETITEMW, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW,
    LVN_COLUMNCLICK, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP,
    LVS_REPORT, LVS_SHOWSELALWAYS, NM_DBLCLK, NM_SETFOCUS, NMHDR, NMLISTVIEW, SetWindowTheme,
    WC_BUTTONW, WC_EDITW, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_CONTROL, VK_DELETE, VK_DOWN, VK_F5, VK_RETURN, VK_SHIFT,
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

use ferossh_config::connlog::Entry;

use crate::connlog_view::{self, Column};
use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};

/// What the user did in the panel.
#[derive(Debug)]
pub enum Event {
    /// Open this connection (Connect, Enter, a double click).
    Connect(String),
    /// Read the log again (F5, the Reload button).
    Reload,
    /// Remove these entries (Delete..., the Delete key), when policy
    /// allows it.
    Delete(Vec<Entry>),
    /// Save this CSV text (Save CSV).
    SaveCsv(String),
    /// One of the controls got the keyboard.
    Focused,
    /// Escape with no search: back to the terminal.
    Leave,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_SEARCH: usize = 200;
const ID_LIST: usize = 201;
const ID_CONNECT: usize = 210;
const ID_CSV: usize = 211;
const ID_RELOAD: usize = 212;
const ID_DELETE: usize = 213;

/// Buttons: width in DIPs. The last, Delete..., shows only when policy
/// allows it.
const BUTTONS: [i32; 4] = [80, 88, 64, 72];

/// The height of the summary line above the buttons, in DIPs.
const STATUS_DIP: i32 = 22;

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    search: HWND,
    list: HWND,
    buttons: [HWND; 4],
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    rows: Vec<Entry>,
    /// A sealed part of the log could not be read.
    locked: bool,
    /// Indices into `rows`, in the order shown.
    shown: Vec<usize>,
    sort: Column,
    descending: bool,
    /// What the line above the buttons says: the summary, or what Save
    /// CSV did.
    status: String,
    /// Where the summary line ends: just above the buttons (see `place`).
    status_bottom: i32,
    /// Whether Delete... shows (machine policy may keep every entry).
    can_delete: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
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
        let class = w!("FeroSSHConnectionLog");
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
        // A control that cannot be made takes the panel down with it.
        let made = (|| -> windows::core::Result<(HWND, HWND, [HWND; 4])> {
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
            SendMessageW(search, EM_LIMITTEXT, Some(WPARAM(200)), None);
            let cue = wide("Search: words, a host, failed, -lost, \"two words\"");
            SendMessageW(
                search,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(cue.as_ptr() as isize)),
            );
            let list = child(
                WC_LISTVIEWW,
                w!(""),
                LVS_REPORT | LVS_SHOWSELALWAYS,
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
            for (i, c) in Column::ALL.iter().enumerate() {
                let mut text = wide(c.title());
                let col = LVCOLUMNW {
                    mask: LVCF_TEXT | LVCF_WIDTH,
                    cx: scale(c.width(), dpi),
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
                child(WC_BUTTONW, w!("Connect"), BS_PUSHBUTTON as u32, ID_CONNECT)?,
                child(WC_BUTTONW, w!("Save CSV"), BS_PUSHBUTTON as u32, ID_CSV)?,
                child(WC_BUTTONW, w!("Reload"), BS_PUSHBUTTON as u32, ID_RELOAD)?,
                child(WC_BUTTONW, w!("Delete..."), BS_PUSHBUTTON as u32, ID_DELETE)?,
            ];
            Ok((search, list, buttons))
        })();
        let (search, list, buttons) = match made {
            Ok(made) => made,
            Err(e) => {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(panel);
                return Err(e);
            }
        };
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
                rows: Vec::new(),
                locked: false,
                shown: Vec::new(),
                sort: Column::Time,
                descending: true,
                status: String::new(),
                status_bottom: 0,
                can_delete: false,
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
        let old = (
            std::mem::replace(&mut s.font, font),
            std::mem::replace(&mut s.title_font, title),
        );
        Some((font, old))
    })
    .flatten();
    if let Some((font, (old, old_title))) = refont {
        for c in [h.search, h.list].iter().chain(&h.buttons) {
            set_font(*c, font);
        }
        for (i, c) in Column::ALL.iter().enumerate() {
            // SAFETY: our own control.
            unsafe {
                SendMessageW(
                    h.list,
                    LVM_SETCOLUMNWIDTH,
                    Some(WPARAM(i)),
                    Some(LPARAM(scale(c.width(), dpi) as isize)),
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
    let status = scale(STATUS_DIP, dpi);
    let inner = (width - 2 * pad).max(0);
    let list_y = title + field + gap;
    // The buttons in a row, wrapped onto more rows when the tool is too
    // narrow for one.
    let can_delete = with(|s| s.can_delete).unwrap_or(false);
    let count = if can_delete {
        BUTTONS.len()
    } else {
        BUTTONS.len() - 1
    };
    let mut spots = Vec::with_capacity(count);
    let (mut x, mut row) = (pad, 0);
    for w in BUTTONS.into_iter().take(count) {
        // Never wider than the tool: a very narrow one squeezes them.
        let w = scale(w, dpi).min(inner.max(1));
        if x > pad && x + w > pad + inner {
            row += 1;
            x = pad;
        }
        spots.push((x, row, w));
        x += w + gap;
    }
    let rows = row + 1;
    let block = rows * button + (rows - 1) * gap;
    let buttons_y = (at.h - pad - block).max(list_y);
    let list_h = (buttons_y - gap - status - list_y).max(0);
    with(|s| s.status_bottom = buttons_y - gap);
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.search, pad, title, inner, field, true);
        let _ = MoveWindow(h.list, pad, list_y, inner, list_h, true);
        for (b, (x, row, w)) in h.buttons.iter().zip(spots) {
            let y = buttons_y + row * (button + gap);
            let _ = MoveWindow(*b, x, y, w, button, true);
        }
        let [.., delete] = h.buttons;
        let _ = ShowWindow(delete, if can_delete { SW_SHOWNA } else { SW_HIDE });
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

/// Shows `rows` (read afresh, oldest first); `locked` when a sealed part
/// could not be read. The picked entries stay picked when they are still
/// listed (all of them, when several are).
pub fn set_rows(rows: Vec<Entry>, locked: bool, can_delete: bool) {
    let keep = selected();
    with(|s| {
        s.rows = rows;
        s.locked = locked;
        s.can_delete = can_delete;
    });
    fill(&keep);
}

/// Shows `text` on the line above the buttons (what Save CSV did).
pub fn set_note(text: String) {
    set_status(text);
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

/// A time as the list shows it: local date and time.
fn local(secs: u64) -> String {
    crate::files_panel::when_seconds(i64::try_from(secs).ok())
}

/// Shows `text` on the line above the buttons until the list is filled
/// again (a file that could not be read).
pub fn set_status(text: String) {
    if let Some(h) = with(|s| {
        s.status = text;
        s.h
    }) {
        // SAFETY: our own window.
        unsafe {
            let _ = InvalidateRect(Some(h.panel), None, false);
        }
    }
}

/// Picks row `index`; `focus` also gives it the focus and scrolls to it.
fn set_state(list: HWND, index: usize, focus: bool) {
    let on = LIST_VIEW_ITEM_STATE_FLAGS(if focus {
        LVIS_SELECTED.0 | LVIS_FOCUSED.0
    } else {
        LVIS_SELECTED.0
    });
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
            Some(WPARAM(index)),
            Some(LPARAM(&raw const item as isize)),
        );
        if !focus {
            return;
        }
        SendMessageW(
            list,
            LVM_ENSUREVISIBLE,
            Some(WPARAM(index)),
            Some(LPARAM(0)),
        );
    }
}

/// The rows the search keeps, in the chosen order, and the summary; the
/// entries in `keep` still shown stay picked, else the first row is.
fn fill(keep: &[Entry]) {
    let Some((h, cells, entries)) = with(|s| {
        let query = search_text(s.h.search);
        let mut shown = connlog_view::filter(&s.rows, &query, local);
        connlog_view::sort(&mut shown, s.sort, s.descending);
        s.status = connlog_view::summary(&shown, s.rows.len(), s.locked);
        if !s.can_delete {
            s.status.push_str(" Your organisation keeps every entry.");
        }
        // Whether the policy's copy to the Windows event log is on, and
        // working: what an administrator checks first.
        if crate::machine_policy::log().event_log {
            match crate::eventlog::last_error() {
                None => s.status.push_str(" Copied to the Windows event log."),
                Some(e) => s
                    .status
                    .push_str(&format!(" Not copied to the Windows event log: {e}")),
            }
        }
        // Every row shown is one of `s.rows`: its index from its address.
        let base = s.rows.as_ptr() as usize;
        let size = std::mem::size_of::<Entry>().max(1);
        s.shown = shown
            .iter()
            .map(|r| (std::ptr::from_ref::<Entry>(*r) as usize - base) / size)
            .collect();
        let cells: Vec<Vec<String>> = shown
            .iter()
            .map(|r| {
                Column::ALL
                    .iter()
                    .map(|&c| connlog_view::text(r, c, local))
                    .collect()
            })
            .collect();
        let entries: Vec<Entry> = shown.iter().map(|r| (*r).clone()).collect();
        (s.h, cells, entries)
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
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
    // As many of each entry as were picked: two alike, one picked, stays
    // one picked.
    let mut left: Vec<Option<&Entry>> = keep.iter().map(Some).collect();
    let at: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            left.iter_mut()
                .find(|k| k.is_some_and(|k| k == *e))
                .map(|k| k.take())
                .is_some()
        })
        .map(|(i, _)| i)
        .collect();
    if at.is_empty() {
        if !entries.is_empty() {
            set_state(h.list, 0, true);
        }
    } else {
        // The first picked row gets the focus and is scrolled to.
        for (n, &i) in at.iter().enumerate().rev() {
            set_state(h.list, i, n == 0);
        }
    }
}

/// The picked entries, in the list's order.
fn selected() -> Vec<Entry> {
    let Some(h) = handles() else {
        return Vec::new();
    };
    let mut at = Vec::new();
    // SAFETY: plain messages to our own control; the struct lives
    // through the call.
    unsafe {
        let mut i = -1isize;
        loop {
            i = SendMessageW(
                h.list,
                LVM_GETNEXTITEM,
                Some(WPARAM(i as usize)),
                Some(LPARAM(LVNI_SELECTED as isize)),
            )
            .0;
            if i < 0 {
                break;
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
            if let Ok(n) = usize::try_from(item.lParam.0) {
                at.push(n);
            }
        }
    }
    with(|s| {
        at.iter()
            .filter_map(|&n| s.shown.get(n).and_then(|&i| s.rows.get(i)).cloned())
            .collect()
    })
    .unwrap_or_default()
}

/// Asks the main window to save the rows shown as CSV.
fn save_csv() {
    let Some(text) = with(|s| {
        let rows: Vec<&Entry> = s.shown.iter().filter_map(|&i| s.rows.get(i)).collect();
        connlog_view::csv(&rows)
    }) else {
        return;
    };
    emit(Event::SaveCsv(text));
}

fn emit(e: Event) {
    let Some((main, handler)) =
        STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| (s.h.main, s.handler)))
    else {
        return;
    };
    handler(main, e);
}

fn connect() {
    // What was typed to connect, when that was not the name shown.
    if let Some(e) = selected().into_iter().next() {
        let to = if e.target.is_empty() {
            e.name
        } else {
            e.target
        };
        if !to.is_empty() {
            emit(Event::Connect(to));
        }
    }
}

fn delete() {
    if with(|s| s.can_delete) != Some(true) {
        return;
    }
    let picked = selected();
    if !picked.is_empty() {
        emit(Event::Delete(picked));
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
                if msg.hwnd == h.list && vk == VK_RETURN.0 {
                    connect();
                    return true;
                }
                if msg.hwnd == h.list && vk == VK_DELETE.0 && with(|s| s.can_delete) == Some(true) {
                    delete();
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
        (ID_SEARCH, EN_CHANGE) => {
            let keep = selected();
            fill(&keep);
        }
        (ID_SEARCH, EN_SETFOCUS) => emit(Event::Focused),
        // Enter in the search box goes to the list, where it connects.
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
        (ID_CONNECT, BN_CLICKED) => connect(),
        (ID_CSV, BN_CLICKED) => save_csv(),
        (ID_RELOAD, BN_CLICKED) => emit(Event::Reload),
        (ID_DELETE, BN_CLICKED) => delete(),
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
                            // The latest first.
                            s.descending = c == Column::Time;
                        }
                    });
                    let keep = selected();
                    fill(&keep);
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, total, status, bottom)) = STATE.with(|s| {
        s.try_borrow().ok()?.as_ref().map(|s| {
            (
                s.title_font,
                s.font,
                s.dpi,
                s.rows.len(),
                s.status.clone(),
                s.status_bottom,
            )
        })
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
        let text_colour =
            windows::Win32::Foundation::COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT));
        let grey = windows::Win32::Foundation::COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT));
        SetTextColor(dc, text_colour);
        let mut text: Vec<u16> = "Connection log".encode_utf16().collect();
        DrawTextW(
            dc,
            &mut text,
            &mut tr,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if total > 0 {
            SetTextColor(dc, grey);
            let mut count: Vec<u16> = total.to_string().encode_utf16().collect();
            DrawTextW(
                dc,
                &mut count,
                &mut tr,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        if !status.is_empty() {
            // Between the list and the buttons (see `place`).
            let mut sr = RECT {
                left: pad,
                top: bottom - scale(STATUS_DIP, dpi),
                right: rc.right - pad,
                bottom,
            };
            SelectObject(dc, HGDIOBJ(font.0));
            SetTextColor(dc, grey);
            let mut text: Vec<u16> = status.encode_utf16().collect();
            DrawTextW(
                dc,
                &mut text,
                &mut sr,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
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
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
