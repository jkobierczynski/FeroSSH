//! The page that picks the hosts to run a script on, as native Windows
//! controls: a search box, the saved connections with a tick box each
//! (grouped by folder), Pick shown and Clear shown, the At once and
//! Password boxes, and Run and Cancel, on a panel over the uncovered part
//! of the tools pane.
//!
//! The hosts come from `SnippetsView::native_picker`; ticks and the
//! search go back to it at once, the At once and Password boxes only with
//! Run (the password never leaves this page otherwise). Space ticks the
//! selected host, Enter runs, Escape goes back to the scripts.
//! [`pre_translate`] runs in the message loop like the other native
//! tools' and passes Ctrl+Shift shortcuts on to the main window.

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
    CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_SKIPDEFAULT, EM_SETCUEBANNER, EM_SETSEL,
    LIST_VIEW_ITEM_STATE_FLAGS, LVCDI_GROUP, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVGF_GROUPID,
    LVGF_HEADER, LVGROUP, LVIF_GROUPID, LVIF_PARAM, LVIF_STATE, LVIF_TEXT, LVIS_FOCUSED,
    LVIS_SELECTED, LVIS_STATEIMAGEMASK, LVITEMW, LVM_DELETEALLITEMS, LVM_ENABLEGROUPVIEW,
    LVM_GETITEMW, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTGROUP, LVM_INSERTITEMW,
    LVM_REMOVEALLGROUPS, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW,
    LVN_ITEMCHANGED, LVNI_SELECTED, LVS_EX_CHECKBOXES, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_NOSORTHEADER, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_CUSTOMDRAW, NMHDR,
    NMLISTVIEW, NMLVCUSTOMDRAW, SetWindowTheme, WC_BUTTONW, WC_EDITW, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    EN_CHANGE, ES_AUTOHSCROLL, ES_NUMBER, ES_PASSWORD, GetClientRect, HMENU, IDC_ARROW, IDCANCEL,
    IDOK, IsChild, IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow, RegisterClassW,
    SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_PAINT, WM_SYSCHAR, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};
use zeroize::Zeroizing;

use crate::batch::Password;
use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};
use crate::snippets_view::NativePicker;

/// What the user did on the page.
#[derive(Debug)]
pub enum Event {
    /// The search box says this.
    Filter(String),
    /// A host's tick box: the host's place in the picker, and whether it
    /// is ticked.
    Pick(usize, bool),
    /// Pick shown (`true`) or Clear shown.
    PickShown(bool),
    /// Run, with what At once and Password say.
    Run {
        parallel: String,
        password: Password,
    },
    /// Cancel or Escape.
    Cancel,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_SEARCH: usize = 700;
const ID_LIST: usize = 701;
const ID_ALL: usize = 702;
const ID_NONE: usize = 703;
const ID_PARALLEL: usize = 704;
const ID_PASSWORD: usize = 705;
const ID_RUN: usize = 710;
const ID_CANCEL: usize = 711;

/// The list's columns and their widths at 96 DPI.
const COLUMNS: [(&str, i32); 2] = [("Connection", 150), ("User and host", 200)];

/// A tick box's state image: 1 empty, 2 ticked.
const fn ticked(on: bool) -> u32 {
    (if on { 2 } else { 1 }) << 12
}

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    search: HWND,
    list: HWND,
    all: HWND,
    none: HWND,
    parallel: HWND,
    password: HWND,
    run: HWND,
    cancel: HWND,
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    shown: Option<NativePicker>,
    /// While the list or the boxes are filled: their changes are not the
    /// user's.
    filling: bool,
    /// Each group heading's folder name and picked/total count.
    groups: Vec<(String, String)>,
    help_at: RECT,
    labels: [RECT; 2],
    hint_at: RECT,
    error_at: RECT,
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
        let class = w!("FeroSSHScriptHosts");
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
        let edit = WS_BORDER.0 | ES_AUTOHSCROLL as u32;
        let search = child(WC_EDITW, w!(""), edit, ID_SEARCH)?;
        let cue = wide("Search connections and folders");
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
        let ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_CHECKBOXES;
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
        let all = child(WC_BUTTONW, w!("Pick shown"), BS_PUSHBUTTON as u32, ID_ALL)?;
        let none = child(WC_BUTTONW, w!("Clear shown"), BS_PUSHBUTTON as u32, ID_NONE)?;
        let parallel = child(WC_EDITW, w!(""), edit | ES_NUMBER as u32, ID_PARALLEL)?;
        SendMessageW(
            parallel,
            windows::Win32::UI::Controls::EM_LIMITTEXT,
            Some(WPARAM(2)),
            None,
        );
        let password = child(WC_EDITW, w!(""), edit | ES_PASSWORD as u32, ID_PASSWORD)?;
        let cue = wide("Only for hosts with no password source");
        SendMessageW(
            password,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        let run = child(WC_BUTTONW, w!("Run"), BS_DEFPUSHBUTTON as u32, ID_RUN)?;
        let cancel = child(WC_BUTTONW, w!("Cancel"), BS_PUSHBUTTON as u32, ID_CANCEL)?;
        let h = Handles {
            main,
            panel,
            search,
            list,
            all,
            none,
            parallel,
            password,
            run,
            cancel,
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
                groups: Vec::new(),
                help_at: RECT::default(),
                labels: [RECT::default(); 2],
                hint_at: RECT::default(),
                error_at: RECT::default(),
                placed: None,
            });
        });
    }
    if let Some(h) = handles() {
        pass_edge(h.panel);
    }
    Ok(())
}

fn controls(h: &Handles) -> [HWND; 8] {
    [
        h.search, h.list, h.all, h.none, h.parallel, h.password, h.run, h.cancel,
    ]
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
    let field = scale(26, dpi);
    let button = scale(28, dpi);
    let gap = scale(6, dpi);
    let line = scale(20, dpi);
    let inner = (width - 2 * pad).max(0);
    let help_h = 2 * line;
    let search_y = title + help_h;
    let list_y = search_y + field + gap;
    let buttons_y = (at.h - pad - button).max(list_y + 6 * field);
    let error_y = buttons_y - gap - 2 * line;
    let password_y = error_y - gap - field;
    let parallel_y = password_y - gap - field;
    let picks_y = parallel_y - gap - button;
    let list_h = (picks_y - gap - list_y).max(field);
    let label_w = scale(72, dpi).min(inner / 3);
    let box_x = pad + label_w + gap;
    let pick_w = scale(96, dpi);
    let button_w = scale(72, dpi);
    let parallel_w = scale(48, dpi);
    with(|s| {
        s.help_at = RECT {
            left: pad,
            top: title,
            right: pad + inner,
            bottom: title + help_h,
        };
        s.labels = [parallel_y, password_y].map(|y| RECT {
            left: pad,
            top: y,
            right: pad + label_w,
            bottom: y + field,
        });
        s.hint_at = RECT {
            left: box_x + parallel_w + gap,
            top: parallel_y,
            right: pad + inner,
            bottom: parallel_y + field,
        };
        s.error_at = RECT {
            left: pad,
            top: error_y,
            right: pad + inner,
            bottom: error_y + 2 * line,
        };
    });
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.search, pad, search_y, inner, field, true);
        let _ = MoveWindow(h.list, pad, list_y, inner, list_h, true);
        let _ = MoveWindow(h.all, pad, picks_y, pick_w, button, true);
        let _ = MoveWindow(h.none, pad + pick_w + gap, picks_y, pick_w, button, true);
        let _ = MoveWindow(h.parallel, box_x, parallel_y, parallel_w, field, true);
        let _ = MoveWindow(
            h.password,
            box_x,
            password_y,
            (inner - label_w - gap).max(0),
            field,
            true,
        );
        let _ = MoveWindow(h.run, pad, buttons_y, button_w, button, true);
        let _ = MoveWindow(
            h.cancel,
            pad + button_w + gap,
            buttons_y,
            button_w,
            button,
            true,
        );
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
    let Some(h) = handles() else {
        return;
    };
    // SAFETY: our own control.
    unsafe {
        let _ = SetFocus(Some(h.list));
    }
}

/// Shows `picker`: another picker fills the boxes and the list; the same
/// one refills the list when its hosts or ticks changed.
pub fn set_picker(picker: NativePicker) {
    let Some(h) = handles() else {
        return;
    };
    let before = with(|s| s.shown.replace(picker.clone())).flatten();
    let fresh = before.as_ref().is_none_or(|b| b.serial != picker.serial);
    with(|s| s.filling = true);
    if fresh {
        // SAFETY: our own controls; each text lives through its call.
        unsafe {
            let text = wide(&picker.filter);
            let _ = SetWindowTextW(h.search, PCWSTR(text.as_ptr()));
            let text = wide(&picker.parallel);
            let _ = SetWindowTextW(h.parallel, PCWSTR(text.as_ptr()));
            let _ = SetWindowTextW(h.password, w!(""));
        }
    }
    let rows = |p: &NativePicker| p.hosts.iter().map(|h| h.index).collect::<Vec<_>>();
    if fresh || before.as_ref().map(rows) != Some(rows(&picker)) {
        fill_list(&h, &picker);
    } else if before.as_ref().map(|b| &b.hosts) != Some(&picker.hosts) {
        // Only ticks changed (perhaps inside the list's own notification):
        // set them in place rather than rebuild the list.
        retick(&h, &picker);
    }
    with(|s| s.filling = false);
    let new_error = picker.error.is_some()
        && before.as_ref().map(|b| &b.error) != Some(&picker.error)
        && !fresh;
    if new_error {
        // SAFETY: our own controls.
        unsafe {
            if picker.fault == 1 {
                let _ = SetFocus(Some(h.parallel));
                SendMessageW(h.parallel, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            } else {
                let _ = SetFocus(Some(h.list));
            }
        }
    }
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
}

/// The selected row's place in the picker.
fn selected_host(list: HWND) -> Option<usize> {
    // SAFETY: plain messages to our own control; the struct lives
    // through the call.
    unsafe {
        let i = SendMessageW(
            list,
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
            list,
            LVM_GETITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&raw mut item as isize)),
        );
        usize::try_from(item.lParam.0).ok()
    }
}

fn fill_list(h: &Handles, picker: &NativePicker) {
    let keep = selected_host(h.list);
    let (folders, headings) = headings(picker);
    let grouped = folders.iter().any(Option::is_some);
    with(|s| s.groups = headings.clone());
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
            for (id, (name, _)) in headings.iter().enumerate() {
                let mut header = wide(name);
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
        let mut select = None;
        for (i, host) in picker.hosts.iter().enumerate() {
            let group = folders.iter().position(|f| *f == host.folder).unwrap_or(0);
            let mut label = wide(&host.name);
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
                lParam: LPARAM(host.index as isize),
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
            let mut text = wide(&host.detail);
            let sub = LVITEMW {
                iSubItem: 1,
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                h.list,
                LVM_SETITEMTEXTW,
                Some(WPARAM(at as usize)),
                Some(LPARAM(&raw const sub as isize)),
            );
            set_item_state(
                h.list,
                at as i32,
                ticked(host.picked),
                LVIS_STATEIMAGEMASK.0,
            );
            if keep == Some(host.index) || (select.is_none() && keep.is_none()) {
                select = Some(at as i32);
            }
        }
        if let Some(at) = select {
            let on = LVIS_SELECTED.0 | LVIS_FOCUSED.0;
            set_item_state(h.list, at, on, on);
        }
    }
}

/// The ticks and the headings' counts as `picker` says, the rows being
/// the same.
fn retick(h: &Handles, picker: &NativePicker) {
    with(|s| {
        s.groups = headings(picker).1;
    });
    for (i, host) in picker.hosts.iter().enumerate() {
        // SAFETY: a plain message to our own control.
        let state = unsafe {
            SendMessageW(
                h.list,
                windows::Win32::UI::Controls::LVM_GETITEMSTATE,
                Some(WPARAM(i)),
                Some(LPARAM(LVIS_STATEIMAGEMASK.0 as isize)),
            )
        }
        .0 as u32;
        if state != ticked(host.picked) {
            set_item_state(h.list, i as i32, ticked(host.picked), LVIS_STATEIMAGEMASK.0);
        }
    }
    // SAFETY: our own control; the headings repaint with their counts.
    unsafe {
        let _ = InvalidateRect(Some(h.list), None, false);
    }
}

/// The folders in the list's order, and each one's heading: its name and
/// picked/total count.
fn headings(picker: &NativePicker) -> (Vec<Option<String>>, Vec<(String, String)>) {
    let mut folders: Vec<Option<String>> = Vec::new();
    for host in &picker.hosts {
        if !folders.contains(&host.folder) {
            folders.push(host.folder.clone());
        }
    }
    let headings = folders
        .iter()
        .map(|f| {
            let members = picker.hosts.iter().filter(|h| h.folder == *f);
            let picked = members.clone().filter(|h| h.picked).count();
            (
                f.clone().unwrap_or_else(|| "No folder".to_owned()),
                format!("{picked}/{}", members.count()),
            )
        })
        .collect();
    (folders, headings)
}

fn set_item_state(list: HWND, index: i32, state: u32, mask: u32) {
    let item = LVITEMW {
        mask: LVIF_STATE,
        state: LIST_VIEW_ITEM_STATE_FLAGS(state),
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(mask),
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
    }
}

/// Pastes `text` into the box that has the keyboard, as one line.
pub fn paste(text: &str) {
    let Some(h) = handles() else {
        return;
    };
    // SAFETY: plain queries and a message to our own control; the text
    // lives through the call.
    unsafe {
        let f = GetFocus();
        if ![h.search, h.parallel, h.password].contains(&f) {
            return;
        }
        let text = wide(&text.lines().collect::<Vec<_>>().join(" "));
        SendMessageW(
            f,
            windows::Win32::UI::Controls::EM_REPLACESEL,
            Some(WPARAM(1)),
            Some(LPARAM(text.as_ptr() as isize)),
        );
    }
}

fn emit(e: Event) {
    let Some((main, handler)) = with(|s| (s.h.main, s.handler)) else {
        return;
    };
    handler(main, e);
}

fn run(h: &Handles) {
    let parallel = search_text(h.parallel);
    let password = Password(Zeroizing::new(search_text(h.password)));
    emit(Event::Run { parallel, password });
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
                        .any(|k| msg.wParam.0 == usize::from(k.0)) =>
            {
                SendMessageW(h.list, WM_KEYDOWN, Some(msg.wParam), Some(msg.lParam));
                return true;
            }
            // Enter runs from the boxes and the list; a focused button
            // does its own thing.
            WM_KEYDOWN
                if msg.wParam.0 == usize::from(VK_RETURN.0)
                    && [h.search, h.list, h.parallel, h.password].contains(&msg.hwnd) =>
            {
                run(&h);
                return true;
            }
            WM_KEYDOWN if msg.wParam.0 == usize::from(VK_ESCAPE.0) => {
                emit(Event::Cancel);
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
        (id, _) if id == IDOK.0 as usize => run(&h),
        (id, _) if id == IDCANCEL.0 as usize => emit(Event::Cancel),
        (ID_RUN, BN_CLICKED) => run(&h),
        (ID_CANCEL, BN_CLICKED) => emit(Event::Cancel),
        (ID_ALL, BN_CLICKED) => emit(Event::PickShown(true)),
        (ID_NONE, BN_CLICKED) => emit(Event::PickShown(false)),
        (ID_SEARCH, EN_CHANGE) if !filling() => emit(Event::Filter(search_text(h.search))),
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
                if cd.nmcd.dwDrawStage == CDDS_PREPAINT && cd.dwItemType == LVCDI_GROUP {
                    let id = cd.nmcd.dwItemSpec;
                    if let Some((name, count)) = with(|s| s.groups.get(id).cloned()).flatten() {
                        crate::scripts_panel::draw_group_heading(h.list, cd, &name, &count);
                        return LRESULT(CDRF_SKIPDEFAULT as isize);
                    }
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
            LVN_ITEMCHANGED => {
                let nm = &*(lparam.0 as *const NMLISTVIEW);
                let mask = LVIS_STATEIMAGEMASK.0;
                let (old, new) = (nm.uOldState & mask, nm.uNewState & mask);
                // A tick box clicked or toggled with Space; the list's own
                // filling is not the user's.
                if old != 0
                    && new != 0
                    && old != new
                    && !filling()
                    && let Ok(index) = usize::try_from(nm.lParam.0)
                {
                    emit(Event::Pick(index, new == ticked(true)));
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, picker, help_at, labels, hint_at, error_at)) = with(|s| {
        (
            s.title_font,
            s.font,
            s.dpi,
            s.shown.clone(),
            s.help_at,
            s.labels,
            s.hint_at,
            s.error_at,
        )
    }) else {
        return;
    };
    let Some(picker) = picker else {
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
        let text_colour = COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT));
        let grey = COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT));
        let line = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        SelectObject(dc, HGDIOBJ(font.0));
        SetTextColor(dc, grey);
        let count = format!("{} of {} picked", picker.picked, picker.total);
        let mut c: Vec<u16> = count.encode_utf16().collect();
        let mut size = tr;
        DrawTextW(
            dc,
            &mut c,
            &mut size,
            DT_LEFT | line | windows::Win32::Graphics::Gdi::DT_CALCRECT,
        );
        DrawTextW(dc, &mut c, &mut tr, DT_RIGHT | line);
        tr.right -= size.right - size.left + pad;
        SelectObject(dc, HGDIOBJ(title_font.0));
        SetTextColor(dc, text_colour);
        let mut t: Vec<u16> = format!("Run {} on hosts", picker.name)
            .encode_utf16()
            .collect();
        DrawTextW(dc, &mut t, &mut tr, DT_LEFT | line | DT_END_ELLIPSIS);
        SelectObject(dc, HGDIOBJ(font.0));
        for (label, at) in ["At once", "Password"].iter().zip(labels) {
            let mut l: Vec<u16> = label.encode_utf16().collect();
            let mut at = at;
            DrawTextW(dc, &mut l, &mut at, DT_LEFT | line);
        }
        SetTextColor(dc, grey);
        let block = DT_LEFT | DT_WORDBREAK | DT_NOPREFIX;
        let help = "Tick the connections to run the script on; each runs in a hidden \
                    session and the report shows how it went. Enter runs, Escape goes back.";
        let mut text: Vec<u16> = help.encode_utf16().collect();
        let mut at = help_at;
        DrawTextW(dc, &mut text, &mut at, block);
        let mut hint: Vec<u16> = format!("hosts at a time, 1 to {}", crate::batch::MAX_PARALLEL)
            .encode_utf16()
            .collect();
        let mut at = hint_at;
        DrawTextW(dc, &mut hint, &mut at, DT_LEFT | line | DT_END_ELLIPSIS);
        if let Some(e) = &picker.error {
            SetTextColor(dc, crate::ui_theme::error_color());
            let mut e: Vec<u16> = e.encode_utf16().collect();
            let mut at = error_at;
            DrawTextW(dc, &mut e, &mut at, block);
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
