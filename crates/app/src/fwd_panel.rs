//! Port forwards as native Windows controls: a small form to add a
//! forward (Type, Listen, To, and the connection it goes on), a list view
//! of every connection's forwards and X11 in columns (connection, type,
//! listen, to, state), the selected one's details under it, and Edit,
//! Retry and Stop buttons, on a panel over the uncovered part of the
//! tools pane.
//!
//! The rows come from `fwdview::native_rows`; what the user asks for goes
//! back to the main window as a `FwdAction`, the same the drawn view
//! sends. Keys work as in the other native tools: [`pre_translate`] runs
//! in the message loop, passes Ctrl+Shift shortcuts on to the main
//! window, and gives Tab, Enter and Escape their dialog meaning. Enter in
//! the form adds (or saves), Enter or F2 in the list edits, Del stops.

#![allow(unsafe_code)]

use std::cell::RefCell;

use ferossh_config::ForwardSpec;
use ferossh_ipc::ChannelId;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FillRect, HFONT,
    HGDIOBJ, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, EM_SETCUEBANNER,
    EM_SETSEL, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_PARAM, LVIF_TEXT,
    LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE, LVM_GETITEMW,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_ITEMCHANGED, LVN_KEYDOWN,
    LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_CUSTOMDRAW, NM_DBLCLK, NM_SETFOCUS, NMHDR, NMLVCUSTOMDRAW,
    NMLVKEYDOWN, SetWindowTheme, WC_BUTTONW, WC_COMBOBOXW, WC_EDITW, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, SetFocus, VK_CONTROL, VK_DELETE, VK_F2, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT,
    CB_SETCURSEL, CBN_SELCHANGE, CBN_SETFOCUS, CBS_DROPDOWNLIST, CBS_HASSTRINGS, CS_HREDRAW,
    CS_VREDRAW, CreateWindowExW, EN_SETFOCUS, ES_AUTOHSCROLL, GetClientRect, HMENU, IDC_ARROW,
    IDCANCEL, IDOK, IsChild, IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow,
    RegisterClassW, SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_COMMAND, WM_CONTEXTMENU, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_PAINT,
    WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::forwards::ConnView;
use crate::fwdview::{FormInput, FwdAction, FwdRow, KINDS, form_fields, form_input, native_rows};
use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};

/// What the user did in the panel.
#[derive(Debug)]
pub enum Event {
    /// Add, change, retry, stop, or back to the terminal.
    Action(FwdAction),
    /// One of the controls got the keyboard.
    Focused,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_KIND: usize = 300;
const ID_LISTEN: usize = 301;
const ID_TO: usize = 302;
const ID_ON: usize = 303;
const ID_ADD: usize = 304;
const ID_CANCEL: usize = 305;
const ID_LIST: usize = 306;
const ID_EDIT: usize = 310;
const ID_RETRY: usize = 311;
const ID_STOP: usize = 312;

/// Columns: title and width in DIPs.
const COLUMNS: [(&str, i32); 5] = [
    ("Connection", 110),
    ("Type", 60),
    ("Listen", 110),
    ("To", 120),
    ("State", 90),
];

/// The first entry of the On box: the connection is the focused tab's.
const ON_FOCUSED: &str = "On the focused tab's connection";

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    kind: HWND,
    listen: HWND,
    to: HWND,
    on: HWND,
    add: HWND,
    cancel: HWND,
    list: HWND,
    /// Edit, Retry, Stop.
    buttons: [HWND; 3],
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    /// The rows as shown, to rebuild the list only when they change.
    rows: Option<Vec<FwdRow>>,
    /// The On box's connections, after its first entry.
    conns: Vec<(ChannelId, String)>,
    /// The forward the form changes, and its connection.
    editing: Option<(ChannelId, ForwardSpec)>,
    /// The line under the form, and whether it is a problem.
    message: Option<(String, bool)>,
    /// Where the message and the selected row's details are painted.
    message_at: RECT,
    details_at: RECT,
    /// Where the panel was last placed, and the width it was laid out
    /// for, to lay it out again when the form changes.
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

/// Creates the panel, hidden, as a child of `main`.
pub fn create(main: HWND, handler: Handler, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering a class and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHForwards");
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
        let combo = WS_VSCROLL.0 | (CBS_DROPDOWNLIST | CBS_HASSTRINGS) as u32;
        let kind = child(WC_COMBOBOXW, w!(""), combo, ID_KIND)?;
        for (name, _) in KINDS {
            let text = wide(name);
            SendMessageW(
                kind,
                CB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
        }
        SendMessageW(kind, CB_SETCURSEL, Some(WPARAM(0)), None);
        let edit = WS_BORDER.0 | ES_AUTOHSCROLL as u32;
        let listen = child(WC_EDITW, w!(""), edit, ID_LISTEN)?;
        let to = child(WC_EDITW, w!(""), edit, ID_TO)?;
        for (c, cue) in [
            (listen, "Listen: 8080, or 127.0.0.1:8080"),
            (to, "To: host:port, such as db:5432"),
        ] {
            let cue = wide(cue);
            SendMessageW(
                c,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(cue.as_ptr() as isize)),
            );
        }
        let on = child(WC_COMBOBOXW, w!(""), combo, ID_ON)?;
        let add = child(WC_BUTTONW, w!("Add"), BS_DEFPUSHBUTTON as u32, ID_ADD)?;
        let cancel = child(WC_BUTTONW, w!("Cancel"), BS_PUSHBUTTON as u32, ID_CANCEL)?;
        let _ = ShowWindow(cancel, SW_HIDE);
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
        let buttons = [
            child(WC_BUTTONW, w!("Edit"), BS_PUSHBUTTON as u32, ID_EDIT)?,
            child(WC_BUTTONW, w!("Retry"), BS_PUSHBUTTON as u32, ID_RETRY)?,
            child(WC_BUTTONW, w!("Stop"), BS_PUSHBUTTON as u32, ID_STOP)?,
        ];
        let h = Handles {
            main,
            panel,
            kind,
            listen,
            to,
            on,
            add,
            cancel,
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
                conns: Vec::new(),
                editing: None,
                message: None,
                message_at: RECT::default(),
                details_at: RECT::default(),
                placed: None,
            });
        });
        fill_on(&h, &[]);
        enable_buttons(&h);
    }
    if let Some(h) = handles() {
        pass_edge(h.panel);
    }
    Ok(())
}

fn controls(h: &Handles) -> Vec<HWND> {
    let mut v = vec![h.kind, h.listen, h.to, h.on, h.add, h.cancel, h.list];
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
    let editing = with(|s| s.editing.is_some()).unwrap_or(false);
    let pad = scale(8, dpi);
    let title = scale(36, dpi);
    let field = scale(26, dpi);
    let button = scale(28, dpi);
    let gap = scale(6, dpi);
    let line = scale(20, dpi);
    let details = scale(52, dpi);
    let inner = (width - 2 * pad).max(0);
    let kind_w = scale(112, dpi).min(inner / 2);
    let button_w = scale(64, dpi);
    let drop = scale(160, dpi);
    let row2 = title + field + gap;
    let row3 = row2 + field + gap;
    let message_y = row3 + button + gap / 2;
    let list_y = message_y + line;
    let buttons_y = (at.h - pad - button).max(list_y + details + gap);
    let details_y = buttons_y - gap - details;
    let list_h = (details_y - gap - list_y).max(0);
    let on_w = (inner - (button_w + gap) * if editing { 2 } else { 1 }).max(0);
    with(|s| {
        s.message_at = RECT {
            left: pad,
            top: message_y,
            right: pad + inner,
            bottom: message_y + line,
        };
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
        // A combo box's height includes its drop-down list.
        let _ = MoveWindow(h.kind, pad, title, kind_w, field + drop, true);
        let _ = MoveWindow(
            h.listen,
            pad + kind_w + gap,
            title,
            (inner - kind_w - gap).max(0),
            field,
            true,
        );
        let _ = MoveWindow(h.to, pad, row2, inner, field, true);
        let _ = MoveWindow(h.on, pad, row3, on_w, field + drop, true);
        let _ = MoveWindow(h.add, pad + on_w + gap, row3, button_w, button, true);
        let _ = MoveWindow(
            h.cancel,
            pad + on_w + gap + button_w + gap,
            row3,
            button_w,
            button,
            true,
        );
        let _ = ShowWindow(h.cancel, if editing { SW_SHOWNA } else { SW_HIDE });
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

/// Gives the list the keyboard when it lists anything, else the Listen
/// box.
pub fn focus_first() {
    let Some(h) = handles() else {
        return;
    };
    let any = with(|s| s.rows.as_ref().is_some_and(|r| !r.is_empty())).unwrap_or(false);
    // SAFETY: our own controls.
    unsafe {
        let _ = SetFocus(Some(if any { h.list } else { h.listen }));
    }
}

/// Shows `view`'s forwards, unless it shows them already. The selection
/// stays on its forward.
pub fn set_view(view: &[ConnView]) {
    let Some(h) = handles() else {
        return;
    };
    let rows = native_rows(view);
    let conns: Vec<(ChannelId, String)> = view
        .iter()
        .map(|c| (c.connection, c.label.clone()))
        .collect();
    let keep = selected_row().map(|r| (r.connection, r.forward));
    let changed = with(|s| {
        let conns_changed = s.conns != conns;
        if s.rows.as_ref() == Some(&rows) {
            return (false, conns_changed);
        }
        s.rows = Some(rows.clone());
        (true, conns_changed)
    });
    let Some((rebuild, conns_changed)) = changed else {
        return;
    };
    if conns_changed {
        fill_on(&h, &conns);
    }
    if !rebuild {
        return;
    }
    // SAFETY: our own windows; each struct and its text live through the
    // call, which copies them.
    unsafe {
        SendMessageW(h.list, LVM_DELETEALLITEMS, None, None);
        for (i, r) in rows.iter().enumerate() {
            let [first, rest @ ..] = &r.cells;
            let mut label = wide(first);
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
            for (sub, text) in rest.iter().enumerate() {
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
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
    let pick = keep
        .and_then(|k| rows.iter().position(|r| (r.connection, r.forward) == k))
        .or((!rows.is_empty()).then_some(0));
    if let Some(i) = pick {
        set_state(h.list, i as i32, true);
    }
    enable_buttons(&h);
}

/// The On box: the focused tab's connection, then each connection, with
/// the choice kept on its connection.
fn fill_on(h: &Handles, conns: &[(ChannelId, String)]) {
    let kept = picked_connection();
    with(|s| s.conns = conns.to_vec());
    // SAFETY: our own control; each text lives through its call.
    unsafe {
        SendMessageW(h.on, CB_RESETCONTENT, None, None);
        for text in std::iter::once(ON_FOCUSED).chain(conns.iter().map(|c| c.1.as_str())) {
            let text = wide(text);
            SendMessageW(
                h.on,
                CB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
        }
    }
    select_on(h, kept);
}

fn select_on(h: &Handles, connection: Option<ChannelId>) {
    let at = connection
        .and_then(|c| {
            with(|s| s.conns.iter().position(|x| x.0 == c))
                .flatten()
                .map(|i| i + 1)
        })
        .unwrap_or(0);
    // SAFETY: our own control.
    unsafe {
        SendMessageW(h.on, CB_SETCURSEL, Some(WPARAM(at)), None);
    }
}

/// The connection chosen in the On box; `None` for the focused tab's.
fn picked_connection() -> Option<ChannelId> {
    let h = handles()?;
    // SAFETY: plain message to our own control.
    let at = unsafe { SendMessageW(h.on, CB_GETCURSEL, None, None) }.0;
    let at = usize::try_from(at).ok()?.checked_sub(1)?;
    with(|s| s.conns.get(at).map(|c| c.0)).flatten()
}

/// Shows what the main window refused or has to say: a refused forward
/// goes back in the form's boxes to try again.
pub fn notice(spec: Option<ForwardSpec>, why: String) {
    let Some(h) = handles() else {
        return;
    };
    if let Some(spec) = spec {
        fill_form(&h, &spec);
    }
    say(&h, why, true);
}

fn say(h: &Handles, text: String, bad: bool) {
    with(|s| s.message = Some((text, bad)));
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
}

fn quiet(h: &Handles) {
    if with(|s| s.message.take().is_some()).unwrap_or(false) {
        // SAFETY: our own window.
        unsafe {
            let _ = InvalidateRect(Some(h.panel), None, false);
        }
    }
}

fn fill_form(h: &Handles, spec: &ForwardSpec) {
    let (kind, listen, to) = form_fields(spec);
    let (listen, to) = (wide(&listen), wide(&to));
    // SAFETY: our own controls; the texts live through the calls.
    unsafe {
        SendMessageW(h.kind, CB_SETCURSEL, Some(WPARAM(kind)), None);
        let _ = SetWindowTextW(h.listen, PCWSTR(listen.as_ptr()));
        let _ = SetWindowTextW(h.to, PCWSTR(to.as_ptr()));
    }
    kind_changed(h);
}

fn clear_form(h: &Handles) {
    // SAFETY: our own controls.
    unsafe {
        let _ = SetWindowTextW(h.listen, w!(""));
        let _ = SetWindowTextW(h.to, w!(""));
    }
}

/// The To box is for Local and Remote; a SOCKS forward goes anywhere.
fn kind_changed(h: &Handles) {
    // SAFETY: plain messages to our own controls.
    unsafe {
        let kind = SendMessageW(h.kind, CB_GETCURSEL, None, None).0;
        let socks = KINDS
            .get(usize::try_from(kind).unwrap_or(0))
            .is_some_and(|k| k.1 == ferossh_config::ForwardKind::Dynamic);
        let _ = EnableWindow(h.to, !socks);
    }
}

/// Starts or ends changing a forward: Add becomes Save, Cancel shows,
/// and the On box shows (and keeps) the forward's connection.
fn set_editing(h: &Handles, editing: Option<(ChannelId, ForwardSpec)>) {
    let on = editing.as_ref().map(|e| e.0);
    let now = editing.is_some();
    with(|s| s.editing = editing);
    if let Some(c) = on {
        select_on(h, Some(c));
    }
    // SAFETY: our own controls.
    unsafe {
        let _ = SetWindowTextW(h.add, if now { w!("Save") } else { w!("Add") });
        let _ = EnableWindow(h.on, !now);
    }
    // Lays the On row out again for the Cancel button.
    if let Some((Some((at, width)), dpi)) = with(|s| (s.placed, s.dpi)) {
        place(Some(at), width, dpi);
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
fn row_at(list: HWND, index: i32) -> Option<FwdRow> {
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

fn selected_row() -> Option<FwdRow> {
    let h = handles()?;
    row_at(h.list, selected_index(h.list)?)
}

/// Edit for a forward (not a desktop's tunnel), Retry for a stopped one
/// (not a desktop's tunnel either: it would become a saved forward), Stop
/// for any forward.
fn enable_buttons(h: &Handles) {
    let row = selected_row();
    let forward = row.as_ref().is_some_and(|r| r.spec.is_some());
    let edit = forward && row.as_ref().is_some_and(|r| !r.desktop);
    let retry = edit && row.as_ref().is_some_and(|r| r.bad);
    // SAFETY: our own controls.
    unsafe {
        for (b, on) in h.buttons.iter().zip([edit, retry, forward]) {
            let _ = EnableWindow(*b, on);
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

/// Add or Save: what the form says, on the chosen connection.
fn submit(h: &Handles) {
    let kind = usize::try_from(
        // SAFETY: plain message to our own control.
        unsafe { SendMessageW(h.kind, CB_GETCURSEL, None, None) }.0,
    )
    .unwrap_or(0);
    let input = form_input(kind, &search_text(h.listen), &search_text(h.to));
    let editing = with(|s| s.editing.clone()).flatten();
    let action = match (input, editing) {
        (Err(why), _) => {
            say(h, why, true);
            return;
        }
        (Ok(FormInput::Cancel(_)), Some(_)) => {
            say(h, "Stop a forward with the Stop button.".into(), true);
            return;
        }
        (Ok(FormInput::Cancel(cancel)), None) => FwdAction::Cancel {
            cancel,
            connection: picked_connection(),
        },
        (Ok(FormInput::Add(new)), Some((connection, old))) => FwdAction::Replace {
            connection,
            old,
            new,
        },
        (Ok(FormInput::Add(spec)), None) => FwdAction::Add {
            spec,
            connection: picked_connection(),
        },
    };
    quiet(h);
    // Emptied first: a refusal puts the forward back.
    clear_form(h);
    if with(|s| s.editing.is_some()).unwrap_or(false) {
        set_editing(h, None);
    }
    emit(Event::Action(action));
}

/// Puts the selected forward in the form to change it.
fn edit_selected(h: &Handles) {
    let Some(row) = selected_row() else {
        return;
    };
    let Some(spec) = row.spec.clone() else {
        say(h, "X11 is set in the connection's settings.".into(), false);
        return;
    };
    if row.desktop {
        say(
            h,
            "A remote desktop's tunnel ends with its tab.".into(),
            false,
        );
        return;
    }
    fill_form(h, &spec);
    set_editing(h, Some((row.connection, spec.clone())));
    say(
        h,
        format!("Changing {spec}: Save, or Escape to leave it."),
        false,
    );
    // SAFETY: our own controls.
    unsafe {
        let _ = SetFocus(Some(h.listen));
        SendMessageW(h.listen, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
}

/// Starts the selected forward again, unchanged.
fn retry_selected(h: &Handles) {
    let Some(row) = selected_row() else {
        return;
    };
    // A desktop's tunnel is not in the connection's forwards: replacing
    // it would add it there for good.
    if row.desktop {
        return;
    }
    if let Some(spec) = row.spec {
        quiet(h);
        emit(Event::Action(FwdAction::Replace {
            connection: row.connection,
            old: spec.clone(),
            new: spec,
        }));
    }
}

fn stop_selected(h: &Handles) {
    let Some(row) = selected_row() else {
        return;
    };
    if let Some(index) = row.forward {
        quiet(h);
        emit(Event::Action(FwdAction::Remove {
            connection: row.connection,
            index,
        }));
    }
}

/// Escape: ends a change or empties the form, else back to the terminal.
fn escape(h: &Handles) {
    let editing = with(|s| s.editing.is_some()).unwrap_or(false);
    let typed = !search_text(h.listen).is_empty() || !search_text(h.to).is_empty();
    quiet(h);
    if editing {
        clear_form(h);
        set_editing(h, None);
    } else if typed {
        clear_form(h);
    } else {
        emit(Event::Action(FwdAction::Leave));
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
            WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR => return false,
            _ => {}
        }
        IsDialogMessageW(h.panel, msg).as_bool()
    }
}

fn command(h: Handles, id: usize, code: u32) {
    match (id, code) {
        (ID_KIND, CBN_SELCHANGE) => kind_changed(&h),
        (ID_KIND | ID_ON, CBN_SETFOCUS) | (ID_LISTEN | ID_TO, EN_SETFOCUS) => {
            emit(Event::Focused);
        }
        (id, _) if id == IDOK.0 as usize => {
            // SAFETY: a plain query.
            if unsafe { GetFocus() } == h.list {
                edit_selected(&h);
            } else {
                submit(&h);
            }
        }
        (id, _) if id == IDCANCEL.0 as usize => escape(&h),
        (ID_ADD, BN_CLICKED) => submit(&h),
        (ID_CANCEL, BN_CLICKED) => {
            clear_form(&h);
            quiet(&h);
            set_editing(&h, None);
        }
        (ID_EDIT, BN_CLICKED) => edit_selected(&h),
        (ID_RETRY, BN_CLICKED) => retry_selected(&h),
        (ID_STOP, BN_CLICKED) => stop_selected(&h),
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
                    let i = cd.nmcd.lItemlParam.0;
                    let bad = with(|s| {
                        s.rows
                            .as_ref()
                            .and_then(|r| r.get(usize::try_from(i).ok()?))
                            .is_some_and(|r| r.bad)
                    })
                    .unwrap_or(false);
                    if bad {
                        cd.clrText = crate::ui_theme::error_color();
                    }
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
            NM_SETFOCUS => emit(Event::Focused),
            NM_DBLCLK => edit_selected(&h),
            LVN_ITEMCHANGED => enable_buttons(&h),
            LVN_KEYDOWN => {
                let key = &*(lparam.0 as *const NMLVKEYDOWN);
                if key.wVKey == VK_F2.0 {
                    edit_selected(&h);
                } else if key.wVKey == VK_DELETE.0 {
                    stop_selected(&h);
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

/// The list's right-click menu (or Shift+F10, the menu key): Edit, Retry
/// and Stop for the forward under the mouse or selected.
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
    const EDIT: u32 = 1;
    const RETRY: u32 = 2;
    const STOP: u32 = 3;
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
        let Some(index) = index else {
            return;
        };
        let Some(row) = row_at(h.list, index) else {
            return;
        };
        set_state(h.list, index, true);
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let flag = |on: bool| if on { MF_STRING } else { MF_STRING | MF_GRAYED };
        let forward = row.spec.is_some();
        let _ = AppendMenuW(
            menu,
            flag(forward && !row.desktop),
            EDIT as usize,
            w!("&Edit\tEnter"),
        );
        let _ = AppendMenuW(
            menu,
            flag(row.bad && forward && !row.desktop),
            RETRY as usize,
            w!("&Retry"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, flag(forward), STOP as usize, w!("&Stop\tDel"));
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
        // The menu ran its own message loop: the list may have changed,
        // so the row is found again by what it is.
        let again = with(|s| {
            s.rows
                .as_ref()?
                .iter()
                .position(|r| r.connection == row.connection && r.spec == row.spec)
        })
        .flatten();
        let Some(i) = again else {
            return;
        };
        set_state(h.list, i as i32, true);
        match u32::try_from(chosen.0).unwrap_or(0) {
            EDIT => edit_selected(&h),
            RETRY => retry_selected(&h),
            STOP => stop_selected(&h),
            _ => {}
        }
    }
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, count, message, message_at, details_at)) = with(|s| {
        (
            s.title_font,
            s.font,
            s.dpi,
            s.rows.as_ref().map_or(0, Vec::len),
            s.message.clone(),
            s.message_at,
            s.details_at,
        )
    }) else {
        return;
    };
    let details = selected_row().map(|r| r.details).unwrap_or_default();
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
        let mut text: Vec<u16> = "Port forwards".encode_utf16().collect();
        let line = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        DrawTextW(dc, &mut text, &mut tr, DT_LEFT | line);
        if count > 0 {
            SetTextColor(dc, grey);
            let mut n: Vec<u16> = count.to_string().encode_utf16().collect();
            DrawTextW(dc, &mut n, &mut tr, DT_RIGHT | line);
        }
        SelectObject(dc, HGDIOBJ(font.0));
        if let Some((m, bad)) = message {
            SetTextColor(
                dc,
                if bad {
                    crate::ui_theme::error_color()
                } else {
                    grey
                },
            );
            let mut m: Vec<u16> = m.encode_utf16().collect();
            let mut at = message_at;
            DrawTextW(dc, &mut m, &mut at, DT_LEFT | line);
        }
        if !details.is_empty() {
            SetTextColor(dc, grey);
            let mut d: Vec<u16> = details.encode_utf16().collect();
            let mut at = details_at;
            DrawTextW(dc, &mut d, &mut at, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
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
