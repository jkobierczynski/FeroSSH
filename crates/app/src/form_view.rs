//! The connection form (New connection, Edit connection) as native
//! Windows controls: a tab per section (Connection, Look, Extras), each a
//! scrolling page of labelled text boxes and drop-down lists, and Save,
//! Connect, Cancel and Delete buttons below, on a panel over the
//! uncovered part of the tools pane.
//!
//! The rows come from the main window's `SessionForm::native_rows`; what
//! the user does goes back as an [`Event`], and the form model checks and
//! saves as it always has. Keys work as in `conn_view`: [`pre_translate`]
//! runs in the message loop, passes Ctrl+Shift shortcuts on to the main
//! window, and gives Tab, Enter (Save; Shift+Enter Connect) and Escape
//! (Cancel) their dialog meaning. Ctrl+Tab and Ctrl+PageDown/PageUp
//! switch tabs.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_CALCRECT, DT_LEFT, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC,
    HDC, HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    EM_SETCUEBANNER, EM_SETREADONLY, EM_SETSEL, NMHDR, SetScrollInfo, TCIF_TEXT, TCITEMW,
    TCM_DELETEALLITEMS, TCM_GETCURSEL, TCM_INSERTITEMW, TCM_SETCURSEL, TCN_SELCHANGE, WC_BUTTONW,
    WC_COMBOBOXW, WC_EDITW, WC_STATICW, WC_TABCONTROLW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_CONTROL, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BN_SETFOCUS, BS_DEFPUSHBUTTON, BS_NOTIFY, BS_PUSHBUTTON, CB_ADDSTRING,
    CB_GETCURSEL, CB_SETCURSEL, CBN_SELCHANGE, CBN_SETFOCUS, CBS_DROPDOWNLIST, CBS_HASSTRINGS,
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DestroyWindow, EN_CHANGE, EN_SETFOCUS, ES_AUTOHSCROLL,
    GetClientRect, GetParent, GetWindowTextLengthW, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsChild,
    IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow, PostMessageW, RegisterClassW,
    SB_BOTTOM, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION, SB_THUMBTRACK,
    SB_TOP, SB_VERT, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SW_HIDE, SW_SHOWNA, SendMessageW,
    SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_COMMAND,
    WM_CTLCOLORSTATIC, WM_ERASEBKGND, WM_KEYDOWN, WM_MOUSEWHEEL, WM_NOTIFY, WM_PAINT, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_VSCROLL, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};
use crate::session_form::{FieldId, NativeKind, NativeRow};

/// What the user did in the form.
#[derive(Debug)]
pub enum Event {
    /// Text typed into a field.
    Text(FieldId, String),
    /// Option `usize` picked in a drop-down.
    Choice(FieldId, usize),
    /// Browse... for the key file.
    Browse,
    /// Save, or with `true` Connect (Enter, Shift+Enter, the buttons).
    Save(bool),
    /// Cancel or Escape.
    Cancel,
    /// Delete, for a saved connection (the window asks first).
    Delete,
    /// One of the controls got the keyboard.
    Focused,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

/// What the form shows, from the main window's form model.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    /// "New connection" or "Edit connection".
    pub title: String,
    /// The sections the form has, by number, with their names.
    pub sections: Vec<(usize, &'static str)>,
    pub rows: Vec<NativeRow>,
    /// The problem the last Save found.
    pub error: Option<String>,
    /// A saved connection: Delete shows.
    pub delete: bool,
}

const ID_TABS: usize = 300;
const ID_SAVE: usize = 310;
const ID_CONNECT: usize = 311;
const ID_CANCEL: usize = 312;
const ID_DELETE: usize = 313;
/// Row `i`'s control is `ID_ROW + i`.
const ID_ROW: usize = 1000;

/// Posted to the panel: the page's controls are made again after the
/// notification that changed them returns (it may be from one of them).
const WM_REBUILD: u32 = WM_APP + 2;

const SS_NOPREFIX: u32 = 0x80;
const LABEL_DIP: i32 = 92;
const ROW_DIP: i32 = 30;
const FIELD_DIP: i32 = 24;

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    tabs: HWND,
    page: HWND,
    buttons: [HWND; 4],
}

/// One row's controls on the page.
#[derive(Clone, Copy)]
struct Placed {
    label: Option<HWND>,
    control: HWND,
    note: bool,
    /// Browse...: a button's width, not the whole column.
    button: bool,
    /// A drop-down: its height includes the list.
    combo: bool,
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    form: Option<Form>,
    /// The section the page shows.
    section: usize,
    /// The rows the page's controls were made for, and the controls.
    built: Vec<NativeRow>,
    placed: Vec<Placed>,
    scroll: i32,
    /// The tool's whole width, which the controls are laid out for.
    width: i32,
    /// Changing the controls' text ourselves: their change events are not
    /// the user's.
    updating: bool,
    /// The field to give the keyboard after a rebuild.
    refocus: Option<FieldId>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn handles() -> Option<Handles> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| s.h))
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Creates the panel, hidden, as a child of `main`.
pub fn create(main: HWND, handler: Handler, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering classes and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHForm");
        let page_class = w!("FeroSSHFormPage");
        for (name, proc) in [
            (class, wndproc as unsafe extern "system" fn(_, _, _, _) -> _),
            (page_class, page_proc),
        ] {
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: name,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
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
        let tabs = child(WC_TABCONTROLW, w!(""), WS_CLIPSIBLINGS.0, ID_TABS)?;
        let page = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            page_class,
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_CLIPCHILDREN,
            0,
            0,
            0,
            0,
            Some(panel),
            None,
            Some(instance.into()),
            None,
        )?;
        let notify = BS_NOTIFY as u32;
        let buttons = [
            child(
                WC_BUTTONW,
                w!("Save"),
                BS_DEFPUSHBUTTON as u32 | notify,
                ID_SAVE,
            )?,
            child(
                WC_BUTTONW,
                w!("Connect"),
                BS_PUSHBUTTON as u32 | notify,
                ID_CONNECT,
            )?,
            child(
                WC_BUTTONW,
                w!("Cancel"),
                BS_PUSHBUTTON as u32 | notify,
                ID_CANCEL,
            )?,
            child(
                WC_BUTTONW,
                w!("Delete..."),
                BS_PUSHBUTTON as u32 | notify,
                ID_DELETE,
            )?,
        ];
        let (font, title_font) = fonts(dpi);
        for c in [tabs].iter().chain(&buttons) {
            set_font(*c, font);
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                h: Handles {
                    main,
                    panel,
                    tabs,
                    page,
                    buttons,
                },
                handler,
                font,
                title_font,
                dpi,
                form: None,
                section: 0,
                built: Vec::new(),
                placed: Vec::new(),
                scroll: 0,
                width: 0,
                updating: false,
                refocus: None,
            });
        });
        pass_edge(panel);
    }
    Ok(())
}

/// The rows of the page's section.
fn page_rows(form: &Form, section: usize) -> Vec<NativeRow> {
    form.rows
        .iter()
        .filter(|r| r.section == section)
        .cloned()
        .collect()
}

/// Whether two rows need the same controls (only their values differ).
fn same_shape(a: &NativeRow, b: &NativeRow) -> bool {
    // A hint is only a cue banner: it changes in place (the Host hint
    // follows the Name as it is typed).
    a.field == b.field
        && a.label == b.label
        && match (&a.kind, &b.kind) {
            (NativeKind::Text { locked: l1, .. }, NativeKind::Text { locked: l2, .. }) => l1 == l2,
            (NativeKind::Choice { options: o1, .. }, NativeKind::Choice { options: o2, .. }) => {
                o1 == o2
            }
            (NativeKind::Note(_), NativeKind::Note(_))
            | (NativeKind::Browse, NativeKind::Browse) => true,
            _ => false,
        }
}

/// Shows `form`: the tabs, the page's controls and the buttons follow it.
/// Controls whose values changed are updated in place; when fields come
/// or go the page is made again, after the current message.
pub fn set_form(form: Form) {
    let Some(h) = handles() else {
        return;
    };
    let Some((tabs_changed, rebuild, update, error_changed, delete)) = with_state(|s| {
        if s.form.as_ref() == Some(&form) {
            return None;
        }
        let old = s.form.replace(form.clone());
        let tabs_changed = old.as_ref().is_none_or(|o| o.sections != form.sections);
        if !form.sections.iter().any(|(n, _)| *n == s.section) {
            s.section = form.sections.first().map_or(0, |(n, _)| *n);
        }
        let rows = page_rows(&form, s.section);
        let rebuild = old.is_none()
            || tabs_changed
            || rows.len() != s.built.len()
            || rows.iter().zip(&s.built).any(|(a, b)| !same_shape(a, b));
        let update = if rebuild {
            Vec::new()
        } else {
            let changed: Vec<(Placed, NativeRow)> = rows
                .iter()
                .zip(&s.built)
                .zip(&s.placed)
                .filter(|((new, old), _)| new != old)
                .map(|((new, _), p)| (*p, new.clone()))
                .collect();
            s.built = rows;
            changed
        };
        let error_changed = old.as_ref().is_none_or(|o| o.error != form.error);
        Some((tabs_changed, rebuild, update, error_changed, form.delete))
    })
    .flatten() else {
        return;
    };
    // SAFETY: our own windows; each struct and text lives through its call.
    unsafe {
        if tabs_changed {
            fill_tabs(h);
        }
        let _ = ShowWindow(h.buttons[3], if delete { SW_SHOWNA } else { SW_HIDE });
        if error_changed {
            layout(h);
            let _ = InvalidateRect(Some(h.panel), None, true);
        }
        with_state(|s| s.updating = true);
        for (p, row) in update {
            set_value(p, &row);
        }
        with_state(|s| s.updating = false);
        if rebuild {
            let _ = PostMessageW(Some(h.panel), WM_REBUILD, WPARAM(0), LPARAM(0));
        }
    }
}

/// Fills the tab control with the form's sections.
unsafe fn fill_tabs(h: Handles) {
    let Some((sections, current)) = with_state(|s| {
        (
            s.form
                .as_ref()
                .map(|f| f.sections.clone())
                .unwrap_or_default(),
            s.section,
        )
    }) else {
        return;
    };
    // SAFETY: our own control; each struct and its text live through the
    // call.
    unsafe {
        SendMessageW(h.tabs, TCM_DELETEALLITEMS, None, None);
        for (i, (_, name)) in sections.iter().enumerate() {
            let mut text = wide(name);
            let item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                h.tabs,
                TCM_INSERTITEMW,
                Some(WPARAM(i)),
                Some(LPARAM(&raw const item as isize)),
            );
        }
        let at = sections
            .iter()
            .position(|(n, _)| *n == current)
            .unwrap_or(0);
        SendMessageW(h.tabs, TCM_SETCURSEL, Some(WPARAM(at)), None);
    }
}

/// Puts a row's value into its control.
unsafe fn set_value(p: Placed, row: &NativeRow) {
    // SAFETY: our own controls; the text lives through each call.
    unsafe {
        match &row.kind {
            NativeKind::Text { value, hint, .. } => {
                let cue = wide(hint);
                SendMessageW(
                    p.control,
                    EM_SETCUEBANNER,
                    Some(WPARAM(1)),
                    Some(LPARAM(cue.as_ptr() as isize)),
                );
                if search_text(p.control) != *value {
                    let text = wide(value);
                    let _ = SetWindowTextW(p.control, PCWSTR(text.as_ptr()));
                }
            }
            NativeKind::Choice { selected, .. } => {
                let now = SendMessageW(p.control, CB_GETCURSEL, None, None).0;
                if now != *selected as isize {
                    SendMessageW(p.control, CB_SETCURSEL, Some(WPARAM(*selected)), None);
                }
            }
            NativeKind::Note(text) => {
                let text = wide(text);
                let _ = SetWindowTextW(p.control, PCWSTR(text.as_ptr()));
            }
            NativeKind::Browse => {}
        }
    }
}

/// Makes the page's controls again for its section's rows, and gives the
/// keyboard back to the field that had it.
fn rebuild() {
    let Some(h) = handles() else {
        return;
    };
    // SAFETY: plain query.
    let focused = unsafe { GetFocus() };
    let Some((old, rows, font, refocus)) = with_state(|s| {
        let rows = s
            .form
            .as_ref()
            .map(|f| page_rows(f, s.section))
            .unwrap_or_default();
        // The field with the keyboard, to find again.
        let had = s
            .placed
            .iter()
            .zip(&s.built)
            .find(|(p, _)| p.control == focused)
            .and_then(|(_, r)| r.field);
        let refocus = s.refocus.take().or(had);
        s.built = rows.clone();
        (std::mem::take(&mut s.placed), rows, s.font, refocus)
    }) else {
        return;
    };
    // SAFETY: making and destroying our own controls on our own page;
    // each string lives through its call.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map(Into::into)
            .ok();
        for p in &old {
            if let Some(l) = p.label {
                let _ = DestroyWindow(l);
            }
            let _ = DestroyWindow(p.control);
        }
        let make = |class: PCWSTR, text: &str, style: u32, id: usize| {
            let text = wide(text);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                0,
                0,
                0,
                0,
                Some(h.page),
                Some(HMENU(id as *mut _)),
                instance,
                None,
            )
            .ok()?;
            set_font(hwnd, font);
            Some(hwnd)
        };
        let mut placed = Vec::new();
        let mut focus_to = None;
        for (i, row) in rows.iter().enumerate() {
            let id = ID_ROW + i;
            let label = (!row.label.is_empty())
                .then(|| make(WC_STATICW, &row.label, SS_NOPREFIX, 0))
                .flatten();
            let control = match &row.kind {
                NativeKind::Text {
                    value,
                    hint,
                    locked,
                } => {
                    let c = make(
                        WC_EDITW,
                        value,
                        WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                        id,
                    );
                    if let Some(c) = c {
                        let cue = wide(hint);
                        SendMessageW(
                            c,
                            EM_SETCUEBANNER,
                            Some(WPARAM(1)),
                            Some(LPARAM(cue.as_ptr() as isize)),
                        );
                        if *locked {
                            SendMessageW(c, EM_SETREADONLY, Some(WPARAM(1)), None);
                        }
                    }
                    c
                }
                NativeKind::Choice { options, selected } => {
                    let c = make(
                        WC_COMBOBOXW,
                        "",
                        WS_TABSTOP.0 | WS_VSCROLL.0 | (CBS_DROPDOWNLIST | CBS_HASSTRINGS) as u32,
                        id,
                    );
                    if let Some(c) = c {
                        for o in options {
                            let o = wide(o);
                            SendMessageW(c, CB_ADDSTRING, None, Some(LPARAM(o.as_ptr() as isize)));
                        }
                        SendMessageW(c, CB_SETCURSEL, Some(WPARAM(*selected)), None);
                    }
                    c
                }
                NativeKind::Note(text) => make(WC_STATICW, text, SS_NOPREFIX, id),
                NativeKind::Browse => make(
                    WC_BUTTONW,
                    "Browse...",
                    WS_TABSTOP.0 | (BS_PUSHBUTTON | BS_NOTIFY) as u32,
                    id,
                ),
            };
            let Some(control) = control else {
                continue;
            };
            if refocus.is_some() && row.field == refocus {
                focus_to = Some(control);
            }
            placed.push(Placed {
                label,
                control,
                note: matches!(row.kind, NativeKind::Note(_)),
                button: row.kind == NativeKind::Browse,
                combo: matches!(row.kind, NativeKind::Choice { .. }),
            });
        }
        with_state(|s| s.placed = placed);
        // The new controls let the mouse through at the tools pane's edge.
        pass_edge(h.page);
        layout(h);
        if let Some(c) = focus_to {
            let _ = SetFocus(Some(c));
            let end = GetWindowTextLengthW(c) as usize;
            SendMessageW(c, EM_SETSEL, Some(WPARAM(end)), Some(LPARAM(end as isize)));
            scroll_into_view(c);
        }
    }
}

/// The height a note takes at `width`, wrapped.
unsafe fn note_height(dc: HDC, hwnd: HWND, width: i32) -> i32 {
    let len = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    // SAFETY: a buffer sized from the control's own length.
    let n = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut buf) };
    let mut text: Vec<u16> = buf.into_iter().take(n.max(0) as usize).collect();
    let mut rc = RECT {
        right: width,
        ..Default::default()
    };
    // SAFETY: measuring only.
    unsafe {
        DrawTextW(
            dc,
            &mut text,
            &mut rc,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
    }
    rc.bottom
}

/// Lays out the panel: title, tabs, page, error, buttons; and the page's
/// rows at its scroll position.
fn layout(h: Handles) {
    let Some((dpi, font, error, placed, scroll, full)) = with_state(|s| {
        (
            s.dpi,
            s.font,
            s.form.as_ref().and_then(|f| f.error.clone()),
            s.placed.clone(),
            s.scroll,
            s.width,
        )
    }) else {
        return;
    };
    // SAFETY: moving and measuring our own windows; the DC is released.
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(h.panel, &mut rc);
        let width = full.max(rc.right);
        let pad = scale(8, dpi);
        let title = scale(36, dpi);
        let tabs_h = scale(26, dpi);
        let button = scale(28, dpi);
        let gap = scale(6, dpi);
        let error_h = if error.is_some() { scale(40, dpi) } else { 0 };
        let inner = (width - 2 * pad).max(0);
        let buttons_y = (rc.bottom - pad - button).max(title + tabs_h + gap);
        let page_y = title + tabs_h;
        let page_h = (buttons_y - gap - error_h - page_y).max(0);
        let _ = MoveWindow(h.tabs, pad, title, inner, tabs_h, true);
        let _ = MoveWindow(h.page, pad, page_y, inner, page_h, true);
        let mut x = pad;
        for (b, w) in h.buttons.iter().zip([64, 72, 64, 72]) {
            let w = scale(w, dpi);
            let _ = MoveWindow(*b, x, buttons_y, w, button, true);
            x += w + gap;
        }
        // The page's rows.
        let mut prc = RECT::default();
        let _ = GetClientRect(h.page, &mut prc);
        let label_w = scale(LABEL_DIP, dpi);
        let row_h = scale(ROW_DIP, dpi);
        let field_h = scale(FIELD_DIP, dpi);
        let value_x = scale(4, dpi) + label_w;
        let value_w = (prc.right - value_x - scale(4, dpi)).max(0);
        let dc = GetDC(Some(h.page));
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let mut y = scale(6, dpi);
        for p in &placed {
            let top = y - scroll;
            let mid = top + (row_h - field_h) / 2;
            if let Some(l) = p.label {
                let _ = MoveWindow(
                    l,
                    scale(4, dpi),
                    mid + scale(3, dpi),
                    label_w,
                    field_h,
                    true,
                );
            }
            if p.note {
                let nh = note_height(dc, p.control, value_w).max(field_h - scale(6, dpi));
                let _ = MoveWindow(p.control, value_x, top, value_w, nh, true);
                y += nh + scale(6, dpi);
            } else {
                let w = if p.button { scale(84, dpi) } else { value_w };
                // A drop-down's height includes its list.
                let hgt = if p.combo { scale(240, dpi) } else { field_h };
                let _ = MoveWindow(p.control, value_x, mid, w, hgt, true);
                y += row_h;
            }
        }
        SelectObject(dc, old);
        ReleaseDC(Some(h.page), dc);
        let content = y + scale(6, dpi);
        let max_scroll = (content - prc.bottom).max(0);
        if scroll > max_scroll {
            with_state(|s| s.scroll = max_scroll);
            return layout(h);
        }
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: content.max(1) - 1,
            nPage: prc.bottom.max(0) as u32,
            nPos: scroll,
            ..Default::default()
        };
        SetScrollInfo(h.page, SB_VERT, &info, true);
        let _ = InvalidateRect(Some(h.page), None, true);
    }
}

fn is_combo(hwnd: HWND) -> bool {
    let mut name = [0u16; 16];
    // SAFETY: a fixed buffer for the class name.
    let n = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut name) };
    String::from_utf16_lossy(name.get(..n.max(0) as usize).unwrap_or_default())
        .eq_ignore_ascii_case("ComboBox")
}

/// Scrolls the page so control `c` shows.
fn scroll_into_view(c: HWND) {
    let Some(h) = handles() else {
        return;
    };
    // SAFETY: plain queries on our own windows.
    let (top, bottom, page) = unsafe {
        let mut rc = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(c, &mut rc);
        let mut pts = [
            windows::Win32::Foundation::POINT {
                x: rc.left,
                y: rc.top,
            },
            windows::Win32::Foundation::POINT {
                x: rc.right,
                y: rc.bottom,
            },
        ];
        windows::Win32::Graphics::Gdi::MapWindowPoints(None, Some(h.page), &mut pts);
        let mut prc = RECT::default();
        let _ = GetClientRect(h.page, &mut prc);
        (pts[0].y, pts[0].y + 28, prc.bottom)
    };
    let delta = if top < 0 {
        top - 6
    } else if bottom > page {
        bottom - page + 6
    } else {
        return;
    };
    with_state(|s| s.scroll = (s.scroll + delta).max(0));
    layout(h);
}

fn scroll_to(h: Handles, to: i32) {
    with_state(|s| s.scroll = to.max(0));
    layout(h);
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
        with_state(|s| {
            // A form opened again starts on its first tab.
            s.form = None;
            s.section = 0;
            s.scroll = 0;
        });
        return;
    };
    let refont = with_state(|s| {
        if s.dpi == dpi {
            return None;
        }
        s.dpi = dpi;
        let (font, title) = fonts(dpi);
        let old = (
            std::mem::replace(&mut s.font, font),
            std::mem::replace(&mut s.title_font, title),
        );
        let controls: Vec<HWND> = s
            .placed
            .iter()
            .flat_map(|p| [Some(p.control), p.label])
            .flatten()
            .collect();
        Some((font, old, controls))
    })
    .flatten();
    if let Some((font, (old, old_title), controls)) = refont {
        for c in [h.tabs].iter().chain(&h.buttons).chain(&controls) {
            set_font(*c, font);
        }
        // SAFETY: no control uses the old fonts any more.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(old.0));
            let _ = DeleteObject(HGDIOBJ(old_title.0));
        }
    }
    with_state(|s| s.width = width);
    // SAFETY: moving our own window.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = ShowWindow(h.panel, SW_SHOWNA);
    }
    layout(h);
}

/// Whether one of the form's controls has the keyboard.
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

/// Gives the keyboard to the page's first field.
pub fn focus_first() {
    let Some(first) =
        with_state(|s| s.placed.iter().find(|p| !p.note).map(|p| p.control)).flatten()
    else {
        if let Some(h) = handles() {
            // SAFETY: our own control.
            unsafe {
                let _ = SetFocus(Some(h.tabs));
            }
        }
        return;
    };
    // SAFETY: our own control.
    unsafe {
        let _ = SetFocus(Some(first));
    }
}

fn emit(e: Event) {
    let Some((main, handler)) = with_state(|s| (s.h.main, s.handler)) else {
        return;
    };
    handler(main, e);
}

/// Shows section tab `index` (in tab order).
fn show_tab(h: Handles, index: usize) {
    let changed = with_state(|s| {
        let n = s
            .form
            .as_ref()
            .and_then(|f| f.sections.get(index))
            .map(|(n, _)| *n)?;
        (n != s.section).then(|| {
            s.section = n;
            s.scroll = 0;
        })
    })
    .flatten();
    if changed.is_some() {
        // SAFETY: our own control.
        unsafe {
            SendMessageW(h.tabs, TCM_SETCURSEL, Some(WPARAM(index)), None);
        }
        rebuild();
        focus_first();
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
                let ctrl = key_down(VK_CONTROL.0);
                let shift = key_down(VK_SHIFT.0);
                if ctrl && shift && vk != VK_TAB.0 {
                    let mut m = *msg;
                    m.hwnd = h.main;
                    let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&m);
                    windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&m);
                    return true;
                }
                if ctrl && (vk == VK_TAB.0 || vk == VK_NEXT.0 || vk == VK_PRIOR.0) {
                    let n = with_state(|s| s.form.as_ref().map_or(0, |f| f.sections.len()))
                        .unwrap_or(0);
                    if n > 0 {
                        let at = SendMessageW(h.tabs, TCM_GETCURSEL, None, None).0.max(0) as usize;
                        let back = vk == VK_PRIOR.0 || (vk == VK_TAB.0 && shift);
                        let to = if back { (at + n - 1) % n } else { (at + 1) % n };
                        show_tab(h, to);
                    }
                    return true;
                }
                if vk == VK_RETURN.0 && shift && !is_dropped(GetFocus()) {
                    emit(Event::Save(true));
                    return true;
                }
            }
            WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR => return false,
            _ => {}
        }
        IsDialogMessageW(h.panel, msg).as_bool()
    }
}

/// A drop-down list that is open: Enter closes it rather than saving.
fn is_dropped(hwnd: HWND) -> bool {
    const CB_GETDROPPEDSTATE: u32 = 0x0157;
    // SAFETY: plain query; GetParent finds the combo of its edit part.
    unsafe {
        is_combo(hwnd) && SendMessageW(hwnd, CB_GETDROPPEDSTATE, None, None).0 != 0
            || GetParent(hwnd).ok().is_some_and(|p| {
                is_combo(p) && SendMessageW(p, CB_GETDROPPEDSTATE, None, None).0 != 0
            })
    }
}

/// A command from one of the page's row controls.
fn row_command(id: usize, code: u32, control: HWND) {
    let Some((row, updating)) = with_state(|s| {
        let row = s.built.get(id.checked_sub(ID_ROW)?)?.clone();
        Some((row, s.updating))
    })
    .flatten() else {
        return;
    };
    match (code, &row.kind, row.field) {
        (EN_CHANGE, NativeKind::Text { .. }, Some(f)) if !updating => {
            emit(Event::Text(f, search_text(control)));
        }
        (CBN_SELCHANGE, NativeKind::Choice { .. }, Some(f)) if !updating => {
            // SAFETY: plain query to our own control.
            let i = unsafe { SendMessageW(control, CB_GETCURSEL, None, None) }.0;
            if let Ok(i) = usize::try_from(i) {
                emit(Event::Choice(f, i));
            }
        }
        (BN_CLICKED, NativeKind::Browse, _) => emit(Event::Browse),
        (EN_SETFOCUS | CBN_SETFOCUS | BN_SETFOCUS, ..) => {
            scroll_into_view(control);
            emit(Event::Focused);
        }
        _ => {}
    }
}

fn command(id: usize, code: u32) {
    // Enter and Escape come as IDOK and IDCANCEL with code 0, as a click.
    if code == BN_SETFOCUS {
        emit(Event::Focused);
        return;
    }
    if code != BN_CLICKED {
        return;
    }
    match id {
        id if id == IDOK.0 as usize || id == ID_SAVE => emit(Event::Save(false)),
        ID_CONNECT => emit(Event::Save(true)),
        id if id == IDCANCEL.0 as usize || id == ID_CANCEL => emit(Event::Cancel),
        ID_DELETE => emit(Event::Delete),
        _ => {}
    }
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, title, error)) = with_state(|s| {
        (
            s.title_font,
            s.font,
            s.dpi,
            s.form.as_ref().map(|f| f.title.clone()).unwrap_or_default(),
            s.form.as_ref().and_then(|f| f.error.clone()),
        )
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
        SetTextColor(dc, COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT)));
        let mut text: Vec<u16> = title.encode_utf16().collect();
        DrawTextW(
            dc,
            &mut text,
            &mut tr,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if let Some(e) = error {
            SelectObject(dc, HGDIOBJ(font.0));
            // Red, readable on the window colour.
            SetTextColor(dc, crate::ui_theme::error_color());
            let button = scale(28, dpi);
            let gap = scale(6, dpi);
            let bottom = rc.bottom - scale(8, dpi) - button - gap;
            let mut er = RECT {
                left: pad,
                top: bottom - scale(40, dpi),
                right: rc.right - pad,
                bottom,
            };
            let mut text: Vec<u16> = e.encode_utf16().collect();
            DrawTextW(dc, &mut text, &mut er, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
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
        (WM_COMMAND, Some(_)) => {
            command(wparam.0 & 0xFFFF, ((wparam.0 >> 16) & 0xFFFF) as u32);
            LRESULT(0)
        }
        (WM_NOTIFY, Some(h)) => {
            // SAFETY: Windows passes a valid NMHDR for WM_NOTIFY.
            let hdr = unsafe { &*(lparam.0 as *const NMHDR) };
            if hdr.hwndFrom == h.tabs && hdr.code == TCN_SELCHANGE {
                // SAFETY: plain query to our own control.
                let at = unsafe { SendMessageW(h.tabs, TCM_GETCURSEL, None, None) }.0;
                if let Ok(at) = usize::try_from(at) {
                    let n = with_state(|s| {
                        s.form
                            .as_ref()
                            .and_then(|f| f.sections.get(at))
                            .map(|(n, _)| *n)
                    })
                    .flatten();
                    if let Some(n) = n {
                        with_state(|s| {
                            s.section = n;
                            s.scroll = 0;
                        });
                        rebuild();
                    }
                }
                emit(Event::Focused);
            }
            LRESULT(0)
        }
        (WM_REBUILD, Some(_)) => {
            rebuild();
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}

extern "system" fn page_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let h = handles().filter(|h| h.page == hwnd);
    match (msg, h) {
        (WM_PAINT, Some(_)) => {
            // SAFETY: a BeginPaint/EndPaint pair on our own window.
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                FillRect(dc, &ps.rcPaint, crate::ui_theme::sys_brush(COLOR_WINDOW));
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        (WM_ERASEBKGND, _) => LRESULT(1),
        (WM_COMMAND, Some(_)) => {
            let control = HWND(lparam.0 as *mut _);
            row_command(
                wparam.0 & 0xFFFF,
                ((wparam.0 >> 16) & 0xFFFF) as u32,
                control,
            );
            LRESULT(0)
        }
        (WM_CTLCOLORSTATIC, Some(_)) => {
            // Labels on the window colour; notes grey.
            let dc = HDC(wparam.0 as *mut _);
            let control = HWND(lparam.0 as *mut _);
            let note = with_state(|s| s.placed.iter().any(|p| p.note && p.control == control))
                .unwrap_or(false);
            // SAFETY: the DC Windows passes for this control's paint.
            unsafe {
                SetBkMode(dc, TRANSPARENT);
                let colour = if note {
                    COLOR_GRAYTEXT
                } else {
                    COLOR_WINDOWTEXT
                };
                SetTextColor(dc, COLORREF(crate::ui_theme::sys_color(colour)));
                LRESULT(crate::ui_theme::sys_brush(COLOR_WINDOW).0 as isize)
            }
        }
        (WM_VSCROLL, Some(h)) => {
            let (pos, page) = with_state(|s| (s.scroll, s.dpi)).unwrap_or((0, 96));
            let line = scale(ROW_DIP, page);
            // SAFETY: plain query.
            let height = unsafe {
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                rc.bottom
            };
            let code = (wparam.0 & 0xFFFF) as i32;
            let thumb = ((wparam.0 >> 16) & 0xFFFF) as i32;
            let to = match code {
                c if c == SB_LINEUP.0 => pos - line,
                c if c == SB_LINEDOWN.0 => pos + line,
                c if c == SB_PAGEUP.0 => pos - height,
                c if c == SB_PAGEDOWN.0 => pos + height,
                c if c == SB_THUMBTRACK.0 || c == SB_THUMBPOSITION.0 => thumb,
                c if c == SB_TOP.0 => 0,
                c if c == SB_BOTTOM.0 => i32::MAX / 2,
                _ => pos,
            };
            scroll_to(h, to);
            LRESULT(0)
        }
        (WM_MOUSEWHEEL, Some(h)) => {
            let delta = i32::from(((wparam.0 >> 16) & 0xFFFF) as u16 as i16);
            let (pos, dpi) = with_state(|s| (s.scroll, s.dpi)).unwrap_or((0, 96));
            scroll_to(h, pos - delta * scale(ROW_DIP, dpi) * 3 / 120);
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
