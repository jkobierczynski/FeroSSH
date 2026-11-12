//! File > Import connections (and Import scripts) as native Windows
//! controls, on a panel over the uncovered part of the tools pane.
//!
//! Two pages, as on `ImportView`: the programs that have something saved
//! on this account, in a list view (Open, From a file..., Close), then one
//! program's sessions or scripts, each with a tick box (Select all, Clear
//! all, Import, Back). Names already in use show greyed with no box.
//!
//! What it shows comes from `ImportView::native`; what the user does goes
//! back as an [`Event`]. Keys work as on the other native tools:
//! [`pre_translate`] runs in the message loop, passes Ctrl+Shift shortcuts
//! on to the main window, Enter opens or imports, Escape (or Backspace in
//! the preview's list) goes back, then closes.

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
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDDS_SUBITEM, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW,
    CDRF_NOTIFYSUBITEMDRAW, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
    LVIF_PARAM, LVIF_STATE, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVIS_STATEIMAGEMASK, LVITEMW,
    LVM_DELETEALLITEMS, LVM_DELETECOLUMN, LVM_ENSUREVISIBLE, LVM_GETITEMSTATE, LVM_GETITEMW,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_ITEMCHANGED, LVN_ITEMCHANGING, LVNI_SELECTED,
    LVS_EX_CHECKBOXES, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP,
    LVS_NOSORTHEADER, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_CUSTOMDRAW, NM_DBLCLK,
    NM_SETFOCUS, NMHDR, NMLISTVIEW, NMLVCUSTOMDRAW, SetWindowTheme, WC_BUTTONW, WC_EDITW,
    WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, SetFocus, VK_BACK, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GetClientRect, HMENU, IDC_ARROW, IDCANCEL, IDOK,
    IsChild, IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow, RegisterClassW,
    SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_PAINT, WM_SYSCHAR, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VSCROLL,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::import_view::{NativeImport, NativeItem, NativePage, NativeSource};
use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, set_font, wide};

/// What the user did on the page.
#[derive(Debug)]
pub enum Event {
    /// Preview the source at this place in the list of sources.
    Open(usize),
    /// Pick a file of another program's to read.
    FromFile,
    /// An item's tick box: its place in the preview, and whether it is
    /// ticked.
    Check(usize, bool),
    /// Select all (`true`) or Clear all.
    CheckAll(bool),
    /// Import the ticked items.
    Import,
    /// From the preview back to the list of sources.
    Back,
    /// Close the page.
    Close,
    /// One of the controls got the keyboard.
    Focused,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_SOURCES: usize = 900;
const ID_ITEMS: usize = 901;
const ID_OPEN: usize = 910;
const ID_FILE: usize = 911;
const ID_CLOSE: usize = 912;
const ID_ALL: usize = 913;
const ID_NONE: usize = 914;
const ID_IMPORT: usize = 915;
const ID_BACK: usize = 916;
const ID_PROBLEMS: usize = 917;

/// The list of sources' columns and their widths at 96 DPI.
const SOURCE_COLUMNS: [(&str, i32); 3] = [("Program", 110), ("Found", 90), ("Where", 220)];
/// The preview's, for connections and for scripts.
const SESSION_COLUMNS: [(&str, i32); 3] =
    [("Connection", 140), ("Connects to", 170), ("Notes", 220)];
const SCRIPT_COLUMNS: [(&str, i32); 3] = [("Script", 140), ("Category", 110), ("Lines", 260)];

/// A tick box's state image: 0 none, 1 empty, 2 ticked.
const fn ticked(item: &NativeItem) -> u32 {
    (if item.taken {
        0
    } else if item.checked {
        2
    } else {
        1
    }) << 12
}

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    sources: HWND,
    items: HWND,
    open: HWND,
    file: HWND,
    close: HWND,
    all: HWND,
    none: HWND,
    import: HWND,
    back: HWND,
    /// What could not be read, in a read-only box that scrolls.
    problems: HWND,
}

impl Handles {
    fn controls(&self) -> [HWND; 10] {
        [
            self.problems,
            self.sources,
            self.items,
            self.open,
            self.file,
            self.close,
            self.all,
            self.none,
            self.import,
            self.back,
        ]
    }

    /// The controls of the list of sources, then of the preview.
    fn pages(&self) -> ([HWND; 4], [HWND; 5]) {
        (
            [self.sources, self.open, self.file, self.close],
            [self.items, self.all, self.none, self.import, self.back],
        )
    }
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    shown: Option<NativeImport>,
    /// Which column set the preview's list has: scripts or connections.
    columns: Option<bool>,
    /// While the lists are filled: their changes are not the user's.
    filling: bool,
    help_at: RECT,
    hint_at: RECT,
    footer_at: RECT,
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

/// Whether the preview's item `i` is taken (its name is in use).
fn taken(i: usize) -> bool {
    with(|s| match s.shown.as_ref().map(|v| &v.page) {
        Some(NativePage::Preview { items, .. }) => items.get(i).is_some_and(|it| it.taken),
        _ => false,
    })
    .unwrap_or(false)
}

/// Whether the list of sources has problems to show.
fn has_problems() -> bool {
    with(|s| match s.shown.as_ref().map(|v| &v.page) {
        Some(NativePage::Sources { problems, .. }) => !problems.is_empty(),
        _ => false,
    })
    .unwrap_or(false)
}

fn on_preview() -> bool {
    with(|s| {
        matches!(
            s.shown.as_ref().map(|v| &v.page),
            Some(NativePage::Preview { .. })
        )
    })
    .unwrap_or(false)
}

fn set_columns(list: HWND, columns: &[(&str, i32)], dpi: u32) {
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        while SendMessageW(list, LVM_DELETECOLUMN, Some(WPARAM(0)), None).0 != 0 {}
        for (i, (title, width)) in columns.iter().enumerate() {
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
    }
}

/// Creates the panel, hidden, as a child of `main`.
pub fn create(main: HWND, handler: Handler, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering a class and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHImport");
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
                WS_CHILD | WS_TABSTOP | WINDOW_STYLE(style),
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
        let list_style = LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER;
        let sources = child(WC_LISTVIEWW, w!(""), list_style, ID_SOURCES)?;
        let items = child(WC_LISTVIEWW, w!(""), list_style, ID_ITEMS)?;
        for (list, boxes) in [(sources, false), (items, true)] {
            let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
            let mut ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP;
            if boxes {
                ex |= LVS_EX_CHECKBOXES;
            }
            SendMessageW(
                list,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                Some(WPARAM(ex as usize)),
                Some(LPARAM(ex as isize)),
            );
        }
        set_columns(sources, &SOURCE_COLUMNS, dpi);
        let push = BS_PUSHBUTTON as u32;
        let open = child(WC_BUTTONW, w!("&Open"), BS_DEFPUSHBUTTON as u32, ID_OPEN)?;
        let file = child(WC_BUTTONW, w!("Fro&m a file..."), push, ID_FILE)?;
        let close = child(WC_BUTTONW, w!("Close"), push, ID_CLOSE)?;
        let all = child(WC_BUTTONW, w!("Select &all"), push, ID_ALL)?;
        let none = child(WC_BUTTONW, w!("C&lear all"), push, ID_NONE)?;
        let import = child(
            WC_BUTTONW,
            w!("&Import"),
            BS_DEFPUSHBUTTON as u32,
            ID_IMPORT,
        )?;
        let back = child(WC_BUTTONW, w!("&Back"), push, ID_BACK)?;
        let problems = child(
            WC_EDITW,
            w!(""),
            WS_BORDER.0 | WS_VSCROLL.0 | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
            ID_PROBLEMS,
        )?;
        let h = Handles {
            main,
            panel,
            sources,
            items,
            open,
            file,
            close,
            all,
            none,
            import,
            back,
            problems,
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
                columns: None,
                filling: false,
                help_at: RECT::default(),
                hint_at: RECT::default(),
                footer_at: RECT::default(),
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
        // Columns are set again at the new size by the next view.
        s.columns = None;
        s.shown = None;
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
        set_columns(h.sources, &SOURCE_COLUMNS, dpi);
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
    let line = scale(18, dpi);
    let inner = (width - 2 * pad).max(0);
    let help_h = 2 * line;
    let list_y = title + help_h + gap;
    // The buttons stay at the bottom, inside the panel however short it
    // is; the lists shrink first, then the hint and problems go.
    let buttons_y = (at.h - pad - button).max(0);
    let room = buttons_y - gap - list_y;
    // Sources: what could not be read under the list, then From a file
    // and its hint.
    let problems_h = if has_problems() { 3 * line } else { 0 };
    let hint_h = 3 * line;
    // From a file and at least one row of list; what could not be read
    // comes before the hint, which only says what From a file reads.
    let sources_need = |extra: i32| extra + button + gap + button;
    let problems_h = if room >= sources_need(problems_h + gap) {
        problems_h
    } else {
        0
    };
    let hint_h = if room >= sources_need(problems_h + gap + hint_h + gap) {
        hint_h
    } else {
        0
    };
    let hint_y = buttons_y - if hint_h > 0 { gap + hint_h } else { 0 };
    let file_y = hint_y - gap - button;
    let problems_y = file_y - gap - problems_h;
    let sources_end = if problems_h > 0 { problems_y } else { file_y };
    let sources_h = (sources_end - gap - list_y).max(0);
    // Preview: Select all and Clear all, then where it is saved.
    // Where the import goes (and that it replaces an earlier one) stays
    // as long as Select all and one row of list fit.
    let footer_h = if room >= 2 * line + gap + button + gap + button {
        2 * line
    } else {
        0
    };
    let footer_y = buttons_y - if footer_h > 0 { gap + footer_h } else { 0 };
    let picks_y = footer_y - gap - button;
    let items_h = (picks_y - gap - list_y).max(0);
    let button_w = scale(80, dpi);
    let wide_w = scale(112, dpi);
    let rect = |y: i32, h: i32| RECT {
        left: pad,
        top: y,
        right: pad + inner,
        bottom: y + h,
    };
    with(|s| {
        s.help_at = rect(title, help_h);
        s.hint_at = rect(hint_y, hint_h);
        s.footer_at = rect(footer_y, footer_h);
    });
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.sources, pad, list_y, inner, sources_h, true);
        let _ = MoveWindow(h.problems, pad, problems_y, inner, problems_h, true);
        let _ = ShowWindow(
            h.problems,
            if problems_h > 0 && !on_preview() {
                SW_SHOWNA
            } else {
                SW_HIDE
            },
        );
        let _ = MoveWindow(h.file, pad, file_y, wide_w, button, true);
        let _ = MoveWindow(h.open, pad, buttons_y, button_w, button, true);
        let x = pad + button_w + gap;
        let _ = MoveWindow(h.close, x, buttons_y, button_w, button, true);
        let _ = MoveWindow(h.items, pad, list_y, inner, items_h, true);
        let _ = MoveWindow(h.all, pad, picks_y, wide_w, button, true);
        let _ = MoveWindow(h.none, pad + wide_w + gap, picks_y, wide_w, button, true);
        let _ = MoveWindow(h.import, pad, buttons_y, wide_w, button, true);
        let x = pad + wide_w + gap;
        let _ = MoveWindow(h.back, x, buttons_y, button_w, button, true);
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

/// Gives the keyboard to the page's list.
pub fn focus_first() {
    let Some(h) = handles() else {
        return;
    };
    let list = if on_preview() { h.items } else { h.sources };
    // SAFETY: our own control.
    unsafe {
        let _ = SetFocus(Some(list));
    }
}

/// Shows `view`: a new page (or another import) fills the controls
/// afresh; the same preview only changes its ticks and counts.
pub fn set_view(view: NativeImport) {
    let Some(h) = handles() else {
        return;
    };
    let before = with(|s| s.shown.replace(view.clone())).flatten();
    if before.as_ref() == Some(&view) {
        return;
    }
    let fresh = before
        .as_ref()
        .is_none_or(|b| b.serial != view.serial || b.snippets != view.snippets);
    with(|s| s.filling = true);
    let had_focus = has_focus();
    match &view.page {
        NativePage::Sources {
            sources,
            last,
            problems,
            ..
        } => {
            if fresh || before.as_ref().map(|b| &b.page) != Some(&view.page) {
                fill_sources(&h, sources, *last);
                let text = wide(&problems.join("\r\n"));
                // SAFETY: our own control; the text lives through the call.
                unsafe {
                    let _ = SetWindowTextW(h.problems, PCWSTR(text.as_ptr()));
                }
            }
            show_page(&h, false);
        }
        NativePage::Preview {
            items,
            chosen,
            importable,
            ..
        } => {
            let same_rows = !fresh
                && matches!(
                    before.as_ref().map(|b| &b.page),
                    Some(NativePage::Preview { items: old, .. })
                        if old.len() == items.len()
                            && old.iter().zip(items).all(|(a, b)| a.name == b.name)
                );
            if same_rows {
                retick(&h, items);
            } else {
                let dpi = with(|s| s.dpi).unwrap_or(96);
                let scripts = view.snippets;
                if with(|s| s.columns.replace(scripts)).flatten() != Some(scripts) {
                    let columns = if scripts {
                        &SCRIPT_COLUMNS
                    } else {
                        &SESSION_COLUMNS
                    };
                    set_columns(h.items, columns, dpi);
                }
                fill_items(&h, items);
            }
            let label = wide(&format!("&Import {chosen}"));
            // SAFETY: our own controls; the text lives through the call.
            unsafe {
                let _ = SetWindowTextW(h.import, PCWSTR(label.as_ptr()));
                let enable = [
                    (h.import, *chosen > 0),
                    (h.all, *chosen < *importable),
                    (h.none, *chosen > 0),
                ];
                // A button about to be disabled hands the keyboard to the
                // list first (Clear all pressed with Space), or it would
                // be left with nothing.
                let focus = GetFocus();
                if enable.iter().any(|&(b, on)| !on && b == focus) {
                    let _ = SetFocus(Some(h.items));
                }
                for (b, on) in enable {
                    let _ = EnableWindow(b, on);
                }
            }
            show_page(&h, true);
        }
    }
    with(|s| s.filling = false);
    // A new page keeps the keyboard in the panel, on its list.
    if fresh && had_focus {
        focus_first();
    }
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
}

fn show_page(h: &Handles, preview: bool) {
    let (sources, items) = h.pages();
    // SAFETY: our own windows.
    unsafe {
        for c in sources {
            let _ = ShowWindow(c, if preview { SW_HIDE } else { SW_SHOWNA });
        }
        for c in items {
            let _ = ShowWindow(c, if preview { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

fn insert_row(list: HWND, at: usize, cells: &[&str], param: usize) -> i32 {
    let Some((first, rest)) = cells.split_first() else {
        return -1;
    };
    let mut label = wide(first);
    let item = LVITEMW {
        mask: LVIF_TEXT | LVIF_PARAM,
        iItem: at as i32,
        pszText: PWSTR(label.as_mut_ptr()),
        lParam: LPARAM(param as isize),
        ..Default::default()
    };
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        let row = SendMessageW(
            list,
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
                list,
                LVM_SETITEMTEXTW,
                Some(WPARAM(row as usize)),
                Some(LPARAM(&raw const item as isize)),
            );
        }
        row as i32
    }
}

fn fill_sources(h: &Handles, sources: &[NativeSource], last: Option<usize>) {
    // SAFETY: a plain message to our own control.
    unsafe {
        SendMessageW(h.sources, LVM_DELETEALLITEMS, None, None);
    }
    for (i, s) in sources.iter().enumerate() {
        insert_row(h.sources, i, &[&s.program, &s.what, &s.origin], i);
    }
    if !sources.is_empty() {
        let at = last.unwrap_or(0).min(sources.len() - 1);
        select(h.sources, at as i32);
    }
    // SAFETY: our own control.
    unsafe {
        let _ = EnableWindow(h.open, !sources.is_empty());
    }
}

fn fill_items(h: &Handles, items: &[NativeItem]) {
    // SAFETY: a plain message to our own control.
    unsafe {
        SendMessageW(h.items, LVM_DELETEALLITEMS, None, None);
    }
    let mut first = None;
    for (i, item) in items.iter().enumerate() {
        let row = insert_row(h.items, i, &[&item.name, &item.detail, &item.notes], i);
        set_item_state(h.items, row, ticked(item), LVIS_STATEIMAGEMASK.0);
        if first.is_none() && !item.taken {
            first = Some(row);
        }
    }
    if let Some(row) = first.or((!items.is_empty()).then_some(0)) {
        select(h.items, row);
    }
}

/// The ticks as `items` say, the rows being the same.
fn retick(h: &Handles, items: &[NativeItem]) {
    for (i, item) in items.iter().enumerate() {
        // SAFETY: a plain message to our own control.
        let state = unsafe {
            SendMessageW(
                h.items,
                LVM_GETITEMSTATE,
                Some(WPARAM(i)),
                Some(LPARAM(LVIS_STATEIMAGEMASK.0 as isize)),
            )
        }
        .0 as u32;
        if state != ticked(item) {
            set_item_state(h.items, i as i32, ticked(item), LVIS_STATEIMAGEMASK.0);
        }
    }
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

fn select(list: HWND, index: i32) {
    let on = LVIS_SELECTED.0 | LVIS_FOCUSED.0;
    set_item_state(list, index, on, on);
    // SAFETY: a plain message to our own control.
    unsafe {
        SendMessageW(
            list,
            LVM_ENSUREVISIBLE,
            Some(WPARAM(index as usize)),
            Some(LPARAM(0)),
        );
    }
}

/// The selected row's place in its page.
fn selected(list: HWND) -> Option<usize> {
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

fn emit(e: Event) {
    let Some((main, handler)) = with(|s| (s.h.main, s.handler)) else {
        return;
    };
    handler(main, e);
}

fn open(h: &Handles) {
    if let Some(i) = selected(h.sources) {
        emit(Event::Open(i));
    }
}

/// Enter: the default button of the page shown.
fn enter(h: &Handles) {
    if on_preview() {
        emit(Event::Import);
    } else {
        open(h);
    }
}

/// Escape: back from the preview, then close.
fn escape() {
    emit(if on_preview() {
        Event::Back
    } else {
        Event::Close
    });
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
            // Enter in a list does the page's main thing; a focused button
            // does its own.
            WM_KEYDOWN
                if msg.wParam.0 == usize::from(VK_RETURN.0)
                    && (msg.hwnd == h.sources || msg.hwnd == h.items) =>
            {
                enter(&h);
                return true;
            }
            WM_KEYDOWN if msg.wParam.0 == usize::from(VK_BACK.0) && msg.hwnd == h.items => {
                emit(Event::Back);
                return true;
            }
            WM_KEYDOWN if msg.wParam.0 == usize::from(VK_ESCAPE.0) => {
                escape();
                return true;
            }
            WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR => {
                // Alt with a button's underlined letter presses it; other
                // Alt keys reach the menu bar (the buttons' letters are
                // none of the menus': File, Edit, Settings, Tools, Window,
                // Session, Help).
                if msg.message == WM_SYSCHAR && press_mnemonic(&h, msg.wParam.0 as u32) {
                    return true;
                }
                return false;
            }
            _ => {}
        }
        IsDialogMessageW(h.panel, msg).as_bool()
    }
}

/// Alt+letter for the shown page's buttons, as a dialog has.
fn press_mnemonic(h: &Handles, c: u32) -> bool {
    let Some(c) = char::from_u32(c).map(|c| c.to_ascii_lowercase()) else {
        return false;
    };
    let event = if on_preview() {
        match c {
            'a' => Event::CheckAll(true),
            'l' => Event::CheckAll(false),
            'i' => Event::Import,
            'b' => Event::Back,
            _ => return false,
        }
    } else {
        match c {
            'o' => {
                open(h);
                return true;
            }
            'm' => Event::FromFile,
            _ => return false,
        }
    };
    emit(event);
    true
}

fn command(h: Handles, id: usize, code: u32) {
    match (id, code) {
        (id, _) if id == IDOK.0 as usize => enter(&h),
        (id, _) if id == IDCANCEL.0 as usize => escape(),
        (ID_OPEN, BN_CLICKED) => open(&h),
        (ID_FILE, BN_CLICKED) => emit(Event::FromFile),
        (ID_CLOSE, BN_CLICKED) => emit(Event::Close),
        (ID_ALL, BN_CLICKED) => emit(Event::CheckAll(true)),
        (ID_NONE, BN_CLICKED) => emit(Event::CheckAll(false)),
        (ID_IMPORT, BN_CLICKED) => emit(Event::Import),
        (ID_BACK, BN_CLICKED) => emit(Event::Back),
        _ => {}
    }
}

fn notify(h: Handles, lparam: LPARAM) -> LRESULT {
    // SAFETY: Windows passes a valid NMHDR (and the larger struct its code
    // says) for the duration of WM_NOTIFY.
    unsafe {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.hwndFrom != h.sources && hdr.hwndFrom != h.items {
            return LRESULT(0);
        }
        let items = hdr.hwndFrom == h.items;
        match hdr.code {
            NM_SETFOCUS => emit(Event::Focused),
            NM_DBLCLK if !items => open(&h),
            NM_CUSTOMDRAW if items => {
                // Taken items greyed, as a disabled row.
                let cd = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
                let stage = cd.nmcd.dwDrawStage;
                if stage == CDDS_PREPAINT {
                    return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
                }
                if stage == CDDS_ITEMPREPAINT || stage.0 == CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0 {
                    let i = usize::try_from(cd.nmcd.lItemlParam.0).unwrap_or(usize::MAX);
                    if taken(i) {
                        cd.clrText = COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT));
                    }
                    return LRESULT(if stage == CDDS_ITEMPREPAINT {
                        CDRF_NOTIFYSUBITEMDRAW as isize
                    } else {
                        CDRF_DODEFAULT as isize
                    });
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
            LVN_ITEMCHANGING if items && !filling() => {
                // A taken item never gets a tick box.
                let nm = &*(lparam.0 as *const NMLISTVIEW);
                let mask = LVIS_STATEIMAGEMASK.0;
                let changes_box = (nm.uChanged.0 & LVIF_STATE.0) != 0
                    && (nm.uOldState & mask) != (nm.uNewState & mask);
                let is_taken = usize::try_from(nm.lParam.0).is_ok_and(taken);
                return LRESULT(isize::from(changes_box && is_taken));
            }
            LVN_ITEMCHANGED if items => {
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
                    emit(Event::Check(index, new == 2 << 12));
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, view, help_at, hint_at, footer_at)) = with(|s| {
        (
            s.title_font,
            s.font,
            s.dpi,
            s.shown.clone(),
            s.help_at,
            s.hint_at,
            s.footer_at,
        )
    }) else {
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
        let block = DT_LEFT | DT_WORDBREAK | DT_NOPREFIX | DT_END_ELLIPSIS;
        let draw = |text: &str, at: RECT, flags| {
            let mut t: Vec<u16> = text.encode_utf16().collect();
            let mut at = at;
            DrawTextW(dc, &mut t, &mut at, flags);
        };
        // The count on the right of the title row, on a preview.
        if let NativePage::Preview {
            chosen, importable, ..
        } = &view.page
        {
            SetTextColor(dc, grey);
            let count = format!("{chosen} of {importable} ticked");
            let mut c: Vec<u16> = count.encode_utf16().collect();
            let mut size = tr;
            DrawTextW(dc, &mut c, &mut size, DT_LEFT | line | DT_CALCRECT);
            DrawTextW(dc, &mut c, &mut tr, DT_RIGHT | line);
            tr.right -= size.right - size.left + pad;
        }
        SelectObject(dc, HGDIOBJ(title_font.0));
        SetTextColor(dc, text_colour);
        draw(&view.title, tr, DT_LEFT | line | DT_END_ELLIPSIS);
        SelectObject(dc, HGDIOBJ(font.0));
        SetTextColor(dc, grey);
        let what = if view.snippets {
            "scripts"
        } else {
            "connections"
        };
        match &view.page {
            NativePage::Sources { hint, .. } => {
                draw(
                    &format!(
                        "Programs on this account with saved {what}. Open one to pick \
                         which to import; nothing is written until you choose Import."
                    ),
                    help_at,
                    block,
                );
                draw(hint, hint_at, block);
            }
            NativePage::Preview { footer, .. } => {
                let help = if view.snippets {
                    "Tick the scripts to import. Greyed ones are in their category already."
                } else {
                    "Tick the connections to import. Greyed ones have a name that is a \
                     host already."
                };
                draw(help, help_at, block);
                draw(footer, footer_at, block);
            }
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
