//! Tools > Theme manager and Tools > Theme rules as native Windows
//! controls, on one panel over the uncovered part of the tools pane like
//! Known hosts; [`set_page`] picks which (see `theme_view.rs`).
//!
//! The theme manager lists the themes (by kind) and the terminal fonts,
//! with the rules that use each; fonts that are not installed are grey.
//! A strip under the list previews the picked theme or font. Apply to this
//! pane (Enter, a double click) puts it on the pane behind the tools; Set
//! as default makes new tabs open with it; Use in a rule... makes a theme
//! rule with it.
//!
//! Theme rules lists the user's rules in the order they are tried, then
//! the built-in tag rules (which cannot change), each with the saved
//! connections it matches. Edit (Enter, a double click) opens one; Move
//! up and Move down change the order; Delete asks first and says which
//! connections change.
//!
//! A click on a column heading sorts within the groups (again: the other
//! way). Keys work as in `known_hosts_panel`: [`pre_translate`] runs in
//! the message loop, passes Ctrl+Shift shortcuts on to the main window,
//! turns typing in the list into a search, and gives Tab, Enter and
//! Escape their dialog meaning.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW,
    CreateSolidBrush, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FillRect, HFONT, HGDIOBJ, InvalidateRect, LOGFONTW,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT,
    EM_LIMITTEXT, EM_SETCUEBANNER, EM_SETSEL, LIST_VIEW_ITEM_STATE_FLAGS, LVCDI_GROUP, LVCF_TEXT,
    LVCF_WIDTH, LVCOLUMNW, LVGF_GROUPID, LVGF_HEADER, LVGF_STATE, LVGGR_HEADER, LVGROUP,
    LVGS_COLLAPSED, LVGS_COLLAPSIBLE, LVIF_GROUPID, LVIF_PARAM, LVIF_TEXT, LVIS_FOCUSED,
    LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_DELETECOLUMN, LVM_ENABLEGROUPVIEW,
    LVM_ENSUREVISIBLE, LVM_GETGROUPRECT, LVM_GETGROUPSTATE, LVM_GETITEMW, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTGROUP, LVM_INSERTITEMW, LVM_REMOVEALLGROUPS, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETGROUPINFO, LVM_SETITEMSTATE, LVM_SETITEMTEXTW,
    LVN_COLUMNCLICK, LVN_ITEMCHANGED, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_EX_LABELTIP, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_CLICK, NM_CUSTOMDRAW,
    NM_DBLCLK, NM_SETFOCUS, NMHDR, NMITEMACTIVATE, NMLISTVIEW, NMLVCUSTOMDRAW, SetWindowTheme,
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

use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};
use crate::theme_view::{self as view, Column, Group, Move, Page, Row};

/// What the user did in the panel.
#[derive(Debug)]
pub enum Event {
    /// Put this theme or font on the pane behind the tools.
    Apply(Row),
    /// New tabs open with this theme or font.
    SetDefault(Row),
    /// Open the themes folder in Explorer.
    OpenFolder,
    /// Change this theme's colours (a built-in one into a new theme).
    EditColours(Row),
    /// Move this rule up or down the list.
    Move(Row, Move),
    /// Make a new rule, starting with this theme and font (from the
    /// theme or font row picked).
    New {
        theme: Option<String>,
        font: Option<String>,
    },
    /// Open this rule to change (Edit, Enter, a double click): the row
    /// as shown, so the rule opened is the one picked even if the list
    /// changed under it.
    Edit(Row),
    /// Delete this rule (the user said yes).
    Delete(Row),
    /// Read the list again (F5).
    Reload,
    /// One of the controls got the keyboard.
    Focused,
    /// Escape with no search: back to the terminal.
    Leave,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_SEARCH: usize = 200;
const ID_LIST: usize = 201;
const ID_NEW: usize = 210;
const ID_EDIT: usize = 211;
const ID_DELETE: usize = 212;
const ID_UP: usize = 213;
const ID_DOWN: usize = 214;
const ID_USE: usize = 220;
const ID_APPLY: usize = 221;
const ID_DEFAULT: usize = 222;
const ID_FOLDER: usize = 223;
const ID_COLOURS: usize = 224;

/// Every button: its id, label, width in DIPs and page.
const BUTTONS: [(usize, &str, i32, Page); 10] = [
    (ID_APPLY, "Apply to this pane", 120, Page::Looks),
    (ID_DEFAULT, "Set as default", 100, Page::Looks),
    (ID_USE, "Use in a rule...", 104, Page::Looks),
    (ID_COLOURS, "Edit colours...", 104, Page::Looks),
    (ID_FOLDER, "Open themes folder", 128, Page::Looks),
    (ID_NEW, "New rule...", 88, Page::Rules),
    (ID_EDIT, "Edit...", 72, Page::Rules),
    (ID_DELETE, "Delete...", 80, Page::Rules),
    (ID_UP, "Move up", 76, Page::Rules),
    (ID_DOWN, "Move down", 84, Page::Rules),
];

/// The height of the summary line above the buttons, in DIPs.
const STATUS_DIP: i32 = 22;

/// The height of the theme manager's preview strip, in DIPs.
const PREVIEW_DIP: i32 = 64;

/// What new tabs open with: the preview of a font shows it in this
/// theme, and of a theme in this font.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Defaults {
    pub theme: String,
    pub font: String,
    pub size: f32,
}

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    search: HWND,
    list: HWND,
    buttons: [HWND; 10],
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    rows: Vec<Row>,
    /// Indices into `rows`, in the order shown.
    shown: Vec<usize>,
    /// The column sorted by; none: the rows' own order.
    sort: Option<Column>,
    descending: bool,
    /// What the line above the buttons says: the summary.
    status: String,
    /// Where the summary line ends: just above the buttons (see `place`).
    status_bottom: i32,
    /// The page shown.
    page: Page,
    /// The look new tabs get, for the preview.
    defaults: Defaults,
    /// Where the preview strip is (empty on the rules page).
    preview: RECT,
    /// Where the panel was put last, to lay it out again for a new page.
    placed: Option<(Rect, i32)>,
    /// The groups folded away, kept when the list is filled again.
    folded: Vec<Group>,
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
        let class = w!("FeroSSHThemeManager");
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
        let made = (|| -> windows::core::Result<(HWND, HWND, [HWND; 10])> {
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
            let cue = wide(
                "Search: a theme, font, tag or connection; -word leaves it out; unused: used by none",
            );
            SendMessageW(
                search,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(cue.as_ptr() as isize)),
            );
            let list = child(
                WC_LISTVIEWW,
                w!(""),
                LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
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
            set_columns(list, Page::default(), dpi);
            let mut buttons = [HWND::default(); 10];
            for (b, (id, label, _, _)) in buttons.iter_mut().zip(BUTTONS) {
                let text = wide(label);
                *b = child(WC_BUTTONW, PCWSTR(text.as_ptr()), BS_PUSHBUTTON as u32, id)?;
            }
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
                shown: Vec::new(),
                sort: None,
                descending: false,
                status: String::new(),
                status_bottom: 0,
                page: Page::default(),
                defaults: Defaults::default(),
                preview: RECT::default(),
                placed: None,
                folded: Vec::new(),
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
    let page = with(|s| {
        s.placed = Some((at, width));
        s.page
    })
    .unwrap_or_default();
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
        for (i, c) in Column::on(page).iter().enumerate() {
            // SAFETY: our own control.
            unsafe {
                SendMessageW(
                    h.list,
                    LVM_SETCOLUMNWIDTH,
                    Some(WPARAM(i)),
                    Some(LPARAM(scale(c.width(page), dpi) as isize)),
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
    let mut spots = Vec::with_capacity(BUTTONS.len());
    let (mut x, mut row) = (pad, 0);
    for (_, _, w, _) in BUTTONS.iter().filter(|b| b.3 == page) {
        let w = *w;
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
    let preview_h = if page == Page::Looks {
        scale(PREVIEW_DIP, dpi) + gap
    } else {
        0
    };
    let list_h = (buttons_y - gap - status - preview_h - list_y).max(0);
    with(|s| {
        s.status_bottom = buttons_y - gap;
        s.preview = if preview_h > 0 {
            RECT {
                left: pad,
                top: list_y + list_h + gap,
                right: pad + inner,
                bottom: list_y + list_h + preview_h,
            }
        } else {
            RECT::default()
        };
    });
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.search, pad, title, inner, field, true);
        let _ = MoveWindow(h.list, pad, list_y, inner, list_h, true);
        let mut spots = spots.into_iter();
        for (b, spec) in h.buttons.iter().zip(BUTTONS) {
            if spec.3 != page {
                let _ = ShowWindow(*b, SW_HIDE);
                continue;
            }
            if let Some((x, row, w)) = spots.next() {
                let y = buttons_y + row * (button + gap);
                let _ = MoveWindow(*b, x, y, w, button, true);
                let _ = ShowWindow(*b, SW_SHOWNA);
            }
        }
        let _ = InvalidateRect(Some(h.panel), None, false);
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

/// Shows `rows` (read afresh), with `defaults` for the preview. The
/// picked row stays picked when it is still listed.
pub fn set_rows(rows: Vec<Row>, defaults: Defaults) {
    let keep = selected().map(|r| key(&r));
    with(|s| {
        s.rows = rows;
        s.defaults = defaults;
    });
    fill(keep);
}

/// The page shown.
pub fn page() -> Page {
    with(|s| s.page).unwrap_or_default()
}

/// Shows `page`: its columns, buttons and rows. The search and the sort
/// start afresh.
pub fn set_page(page: Page) {
    let Some((h, dpi, placed, was)) = with(|s| {
        let was = s.page;
        s.page = page;
        if was != page {
            s.sort = None;
            s.descending = false;
        }
        (s.h, s.dpi, s.placed, was)
    }) else {
        return;
    };
    if was == page {
        return;
    }
    set_columns(h.list, page, dpi);
    // SAFETY: our own control; EN_CHANGE refills the list.
    unsafe {
        let _ = SetWindowTextW(h.search, w!(""));
    }
    if let Some((at, width)) = placed {
        place(Some(at), width, dpi);
    }
    fill(None);
}

/// Gives the list `page`'s columns.
fn set_columns(list: HWND, page: Page, dpi: u32) {
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        while SendMessageW(list, LVM_DELETECOLUMN, Some(WPARAM(0)), None).0 != 0 {}
        for (i, c) in Column::on(page).iter().enumerate() {
            let mut text = wide(c.title(page));
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: scale(c.width(page), dpi),
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

fn set_state(list: HWND, index: usize) {
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
            Some(WPARAM(index)),
            Some(LPARAM(&raw const item as isize)),
        );
        SendMessageW(
            list,
            LVM_ENSUREVISIBLE,
            Some(WPARAM(index)),
            Some(LPARAM(0)),
        );
    }
}

/// Which row it is, across a reload: a rule by what it is (its place
/// may change when another rule goes), a theme or font by its name.
fn key(row: &Row) -> (Group, String) {
    match &row.rule {
        Some(rule) => (row.group, format!("{rule:?}")),
        None => (row.group, row.name.clone()),
    }
}

/// The rows the search keeps, in the chosen order and their groups, and
/// the summary; the row `keep` stays picked, else the first row is.
fn fill(keep: Option<(Group, String)>) {
    let Some((h, cells, names, folded)) = with(|s| {
        let query = search_text(s.h.search);
        let page = s.page;
        let mut shown = view::filter(view::on_page(&s.rows, page), &query);
        view::sort(&mut shown, s.sort, s.descending);
        let rules = s.rows.iter().filter(|r| r.group == Group::Rules).count();
        s.status = view::summary(page, &shown, rules);
        if page == Page::Looks && !s.defaults.theme.is_empty() {
            s.status = format!(
                "New tabs: {}, {} {} pt. {}",
                s.defaults.theme, s.defaults.font, s.defaults.size, s.status
            );
        }
        // Every row shown is one of `s.rows`: its index from its address.
        let base = s.rows.as_ptr() as usize;
        let size = std::mem::size_of::<Row>().max(1);
        s.shown = shown
            .iter()
            .map(|r| (std::ptr::from_ref::<Row>(*r) as usize - base) / size)
            .collect();
        let cells: Vec<(Group, Vec<String>)> = shown
            .iter()
            .map(|r| {
                (
                    r.group,
                    Column::on(page).iter().map(|&c| view::text(r, c)).collect(),
                )
            })
            .collect();
        let names: Vec<(Group, String)> = shown.iter().map(|r| key(r)).collect();
        (s.h, cells, names, s.folded.clone())
    }) else {
        return;
    };
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        SendMessageW(h.list, LVM_DELETEALLITEMS, None, None);
        SendMessageW(h.list, LVM_REMOVEALLGROUPS, None, None);
        SendMessageW(h.list, LVM_ENABLEGROUPVIEW, Some(WPARAM(1)), None);
        for (id, g) in Group::ALL.iter().enumerate() {
            if !cells.iter().any(|(rg, _)| rg == g) {
                continue;
            }
            let mut header = wide(g.title());
            let group = LVGROUP {
                cbSize: size_of::<LVGROUP>() as u32,
                mask: LVGF_HEADER | LVGF_GROUPID | LVGF_STATE,
                pszHeader: PWSTR(header.as_mut_ptr()),
                iGroupId: id as i32,
                stateMask: LVGS_COLLAPSIBLE | LVGS_COLLAPSED,
                state: if folded.contains(g) {
                    LVGS_COLLAPSIBLE | LVGS_COLLAPSED
                } else {
                    LVGS_COLLAPSIBLE
                },
                ..Default::default()
            };
            SendMessageW(
                h.list,
                LVM_INSERTGROUP,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(&raw const group as isize)),
            );
        }
        for (i, (group, row)) in cells.iter().enumerate() {
            let mut label = wide(row.first().map_or("", String::as_str));
            let gid = Group::ALL.iter().position(|g| g == group).unwrap_or(0);
            let item = LVITEMW {
                mask: LVIF_TEXT | LVIF_PARAM | LVIF_GROUPID,
                iItem: i as i32,
                pszText: PWSTR(label.as_mut_ptr()),
                lParam: LPARAM(i as isize),
                iGroupId: gid as i32,
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
    let at = keep
        .and_then(|k| names.iter().position(|n| *n == k))
        .or((!names.is_empty()).then_some(0));
    if let Some(i) = at {
        set_state(h.list, i);
    }
    update_buttons(h);
}

/// The buttons that fit the picked row: on the theme manager, a theme
/// or font; on theme rules, one of the user's own (and Move up or down
/// only where it can go).
fn update_buttons(h: Handles) {
    let row = selected();
    let rules = with(|s| s.rows.iter().filter(|r| r.group == Group::Rules).count()).unwrap_or(0);
    let own = row.as_ref().filter(|r| r.is_own_rule());
    let look = row.as_ref().is_some_and(|r| r.group.page() == Page::Looks);
    let on = |id| match id {
        ID_EDIT | ID_DELETE => own.is_some(),
        ID_UP => own.is_some_and(|r| view::moved_to(rules, r.index, Move::Up).is_some()),
        ID_DOWN => own.is_some_and(|r| view::moved_to(rules, r.index, Move::Down).is_some()),
        ID_APPLY | ID_DEFAULT | ID_USE => look,
        ID_COLOURS => row.as_ref().is_some_and(|r| r.preset_theme().is_some()),
        _ => true,
    };
    for (b, (id, ..)) in h.buttons.iter().zip(BUTTONS) {
        // SAFETY: our own controls.
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(*b, on(id));
        }
    }
    with(|s| {
        // SAFETY: our own window; the strip draws the new pick.
        unsafe {
            let _ = InvalidateRect(Some(s.h.panel), Some(&raw const s.preview), false);
        }
    });
}

/// The picked row. Its list position maps to `shown`, kept in the order
/// the items went in (groups only change how they are drawn).
fn selected() -> Option<Row> {
    let h = handles()?;
    // SAFETY: plain messages to our own control; the struct lives
    // through the call.
    let at = unsafe {
        let i = SendMessageW(
            h.list,
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
            h.list,
            LVM_GETITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&raw mut item as isize)),
        );
        usize::try_from(item.lParam.0).ok()?
    };
    with(|s| s.shown.get(at).and_then(|&i| s.rows.get(i)).cloned()).flatten()
}

fn emit(e: Event) {
    let Some((main, handler)) =
        STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| (s.h.main, s.handler)))
    else {
        return;
    };
    handler(main, e);
}

/// Enter or a double click: a rule opens, a theme or font goes on the
/// pane.
fn edit() {
    match selected() {
        Some(r) if r.is_own_rule() => emit(Event::Edit(r)),
        Some(r) if r.group.page() == Page::Looks => emit(Event::Apply(r)),
        _ => {}
    }
}

fn use_in_rule() {
    if let Some(r) = selected().filter(|r| r.group.page() == Page::Looks) {
        emit(Event::New {
            theme: r.preset_theme().map(str::to_owned),
            font: r.preset_font().map(str::to_owned),
        });
    }
}

fn delete(h: Handles) {
    let Some(row) = selected().filter(Row::is_own_rule) else {
        return;
    };
    let text = wide(&view::delete_question(&row));
    let caption = wide("Theme rules");
    // SAFETY: a message box over the main window; the strings live
    // through the call.
    let yes = unsafe {
        windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
            Some(h.main),
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::MB_YESNO
                | windows::Win32::UI::WindowsAndMessaging::MB_ICONWARNING
                | windows::Win32::UI::WindowsAndMessaging::MB_DEFBUTTON2,
        )
    } == windows::Win32::UI::WindowsAndMessaging::IDYES;
    if yes {
        emit(Event::Delete(row));
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
                    edit();
                    return true;
                }
                if msg.hwnd == h.list && vk == VK_DELETE.0 && page() == Page::Rules {
                    delete(h);
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
            let keep = selected().map(|r| key(&r));
            fill(keep);
        }
        (ID_SEARCH, EN_SETFOCUS) => emit(Event::Focused),
        // Enter in the search box goes to the list, where it edits.
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
        (ID_NEW, BN_CLICKED) => emit(Event::New {
            theme: None,
            font: None,
        }),
        (ID_EDIT, BN_CLICKED) => edit(),
        (ID_DELETE, BN_CLICKED) => delete(h),
        (ID_UP | ID_DOWN, BN_CLICKED) => {
            if let Some(r) = selected().filter(Row::is_own_rule) {
                let way = if id == ID_UP { Move::Up } else { Move::Down };
                emit(Event::Move(r, way));
            }
        }
        (ID_APPLY, BN_CLICKED) => {
            if let Some(r) = selected().filter(|r| r.group.page() == Page::Looks) {
                emit(Event::Apply(r));
            }
        }
        (ID_DEFAULT, BN_CLICKED) => {
            if let Some(r) = selected().filter(|r| r.group.page() == Page::Looks) {
                emit(Event::SetDefault(r));
            }
        }
        (ID_USE, BN_CLICKED) => use_in_rule(),
        (ID_FOLDER, BN_CLICKED) => emit(Event::OpenFolder),
        (ID_COLOURS, BN_CLICKED) => {
            if let Some(r) = selected().filter(|r| r.preset_theme().is_some()) {
                emit(Event::EditColours(r));
            }
        }
        _ => {}
    }
}

/// Draws group heading `id` like the other tool pages: a fold mark, the
/// name in the window's text colour and the rows shown in grey.
fn draw_heading(list: HWND, cd: &NMLVCUSTOMDRAW) {
    let Some(&group) = Group::ALL.get(cd.nmcd.dwItemSpec) else {
        return;
    };
    let Some((folded, count)) = with(|s| {
        let n = s
            .shown
            .iter()
            .filter(|&&i| s.rows.get(i).is_some_and(|r| r.group == group))
            .count();
        (s.folded.contains(&group), n)
    }) else {
        return;
    };
    let mark = if folded { '\u{25B8}' } else { '\u{25BE}' };
    let name = format!("{mark} {}", group.title());
    crate::scripts_panel::draw_group_heading(list, cd, &name, &count.to_string());
}

/// The group whose heading is at `pt`, in the list's client coordinates.
fn heading_at(list: HWND, pt: windows::Win32::Foundation::POINT) -> Option<usize> {
    (0..Group::ALL.len()).find(|&id| {
        let mut rc = RECT {
            top: LVGGR_HEADER as i32,
            ..Default::default()
        };
        // SAFETY: our own control; it fills `rc` (and fails for a group
        // not in the list).
        unsafe {
            SendMessageW(
                list,
                LVM_GETGROUPRECT,
                Some(WPARAM(id)),
                Some(LPARAM(&raw mut rc as isize)),
            )
            .0 != 0
                && pt.x >= rc.left
                && pt.x < rc.right
                && pt.y >= rc.top
                && pt.y < rc.bottom
        }
    })
}

/// Folds group `id` away, or opens it, and remembers which.
fn toggle(h: Handles, id: usize) {
    let Some(&group) = Group::ALL.get(id) else {
        return;
    };
    // SAFETY: a plain query of our own control.
    let now = unsafe {
        SendMessageW(
            h.list,
            LVM_GETGROUPSTATE,
            Some(WPARAM(id)),
            Some(LPARAM(LVGS_COLLAPSED.0 as isize)),
        )
        .0 != 0
    };
    let mine = with(|s| s.folded.contains(&group)).unwrap_or(false);
    // The list folds a heading itself when its own (hidden) button is
    // clicked: then what it shows already is the new state.
    let fold = if now == mine { !now } else { now };
    with(|s| {
        s.folded.retain(|g| *g != group);
        if fold {
            s.folded.push(group);
        }
    });
    let info = LVGROUP {
        cbSize: size_of::<LVGROUP>() as u32,
        mask: LVGF_STATE,
        stateMask: LVGS_COLLAPSED,
        state: if fold {
            LVGS_COLLAPSED
        } else {
            Default::default()
        },
        ..Default::default()
    };
    // SAFETY: our own control; the struct lives through the call.
    unsafe {
        SendMessageW(
            h.list,
            LVM_SETGROUPINFO,
            Some(WPARAM(id)),
            Some(LPARAM(&raw const info as isize)),
        );
        let _ = InvalidateRect(Some(h.list), None, true);
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
                    // The theme draws headings in its own blue: draw them
                    // as the other tool pages do.
                    draw_heading(h.list, cd);
                    return LRESULT(CDRF_SKIPDEFAULT as isize);
                }
                if stage == CDDS_PREPAINT {
                    return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
                }
                if stage == CDDS_ITEMPREPAINT {
                    // The item's lParam is its place in `shown`.
                    let at = cd.nmcd.lItemlParam.0;
                    let dim = with(|s| {
                        let i = *s.shown.get(usize::try_from(at).ok()?)?;
                        s.rows.get(i).map(|r| r.dim)
                    })
                    .flatten()
                    .unwrap_or(false);
                    if dim {
                        cd.clrText = windows::Win32::Foundation::COLORREF(
                            crate::ui_theme::sys_color(COLOR_GRAYTEXT),
                        );
                    }
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
            NM_SETFOCUS => emit(Event::Focused),
            NM_DBLCLK => {
                // A double click on a heading folds it, nothing more.
                let a = &*(lparam.0 as *const NMITEMACTIVATE);
                if heading_at(h.list, a.ptAction).is_none() {
                    edit();
                }
            }
            NM_CLICK => {
                let a = &*(lparam.0 as *const NMITEMACTIVATE);
                if let Some(id) = heading_at(h.list, a.ptAction) {
                    toggle(h, id);
                }
            }
            LVN_ITEMCHANGED => update_buttons(h),
            LVN_COLUMNCLICK => {
                let nm = &*(lparam.0 as *const NMLISTVIEW);
                if let Some(&c) = usize::try_from(nm.iSubItem)
                    .ok()
                    .and_then(|i| Column::on(page()).get(i))
                {
                    with(|s| {
                        if s.sort == Some(c) {
                            s.descending = !s.descending;
                        } else {
                            s.sort = Some(c);
                            // The most used first.
                            s.descending = matches!(c, Column::Connections | Column::Rules);
                        }
                    });
                    let keep = selected().map(|r| key(&r));
                    fill(keep);
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, page, total, status, bottom, strip, defaults)) =
        STATE.with(|s| {
            s.try_borrow().ok()?.as_ref().map(|s| {
                (
                    s.title_font,
                    s.font,
                    s.dpi,
                    s.page,
                    s.rows.iter().filter(|r| r.group == Group::Rules).count(),
                    s.status.clone(),
                    s.status_bottom,
                    s.preview,
                    s.defaults.clone(),
                )
            })
        })
    else {
        return;
    };
    // Read before painting: `selected` takes the state again.
    let picked = (page == Page::Looks).then(selected).flatten();
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
        let mut text: Vec<u16> = page.title().encode_utf16().collect();
        DrawTextW(
            dc,
            &mut text,
            &mut tr,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if total > 0 && page == Page::Rules {
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
        if page == Page::Looks && strip.bottom > strip.top {
            preview(dc, strip, picked.as_ref(), &defaults, dpi);
        }
        SelectObject(dc, old);
        let _ = EndPaint(h.panel, &ps);
    }
}

/// The theme manager's preview: a prompt and an `ls` in the picked theme
/// (else the default one) and font (else the default one), and the
/// theme's eight colours, on `dc` (being painted).
fn preview(
    dc: windows::Win32::Graphics::Gdi::HDC,
    at: RECT,
    row: Option<&Row>,
    d: &Defaults,
    dpi: u32,
) {
    use ferossh_render::Theme;
    let theme = row
        .and_then(|r| r.preset_theme())
        .and_then(Theme::by_name)
        .or_else(|| Theme::by_name(&d.theme))
        .unwrap_or_default();
    let family = row
        .and_then(|r| r.preset_font())
        .unwrap_or(d.font.as_str())
        .to_owned();
    let pt = if d.size > 0.0 { d.size } else { 10.5 };
    let colour = |c: ferossh_render::Rgb| {
        windows::Win32::Foundation::COLORREF(
            u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16),
        )
    };
    // SAFETY: GDI objects made here are selected back out and deleted;
    // the texts live through each call.
    unsafe {
        let fill = |r: RECT, c| {
            let brush = CreateSolidBrush(colour(c));
            FillRect(dc, &r, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        };
        fill(at, theme.background);
        let mut lf = LOGFONTW {
            lfHeight: -((pt * dpi as f32 / 72.0).round() as i32),
            ..Default::default()
        };
        for (d, s) in lf.lfFaceName.iter_mut().take(31).zip(family.encode_utf16()) {
            *d = s;
        }
        let font = CreateFontIndirectW(&lf);
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let pad = scale(8, dpi);
        let line = (at.bottom - at.top - 2 * pad) / 2;
        let ansi = |i: usize| theme.ansi.get(i).copied().unwrap_or(theme.foreground);
        let lines: [&[(&str, ferossh_render::Rgb)]; 2] = [
            &[
                ("user@host", ansi(2)),
                (":", theme.foreground),
                ("~", ansi(4)),
                ("$ ", theme.foreground),
                ("ls", theme.foreground),
            ],
            &[
                ("docs  ", ansi(4)),
                ("notes.txt  ", theme.foreground),
                ("run.sh  ", ansi(2)),
                ("error.log", ansi(1)),
            ],
        ];
        for (n, parts) in lines.iter().enumerate() {
            let mut x = at.left + pad;
            let top = at.top + pad + n as i32 * line;
            for (text, c) in *parts {
                SetTextColor(dc, colour(*c));
                let mut t: Vec<u16> = text.encode_utf16().collect();
                let mut r = RECT {
                    left: x,
                    top,
                    right: at.right - pad,
                    bottom: top + line,
                };
                DrawTextW(
                    dc,
                    &mut t,
                    &mut r,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
                // Where the next part starts: measured the same way.
                let mut m = RECT {
                    left: x,
                    top,
                    right: at.right,
                    bottom: top + line,
                };
                DrawTextW(
                    dc,
                    &mut t,
                    &mut m,
                    DT_LEFT
                        | DT_SINGLELINE
                        | DT_NOPREFIX
                        | windows::Win32::Graphics::Gdi::DT_CALCRECT,
                );
                x = m.right;
            }
        }
        // The eight normal colours, on the right.
        let size = scale(12, dpi);
        for i in 0..8usize {
            let right = at.right - pad - (7 - i as i32) * (size + scale(3, dpi));
            let top = at.bottom - pad - size;
            fill(
                RECT {
                    left: right - size,
                    top,
                    right,
                    bottom: top + size,
                },
                ansi(i),
            );
        }
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(font.0));
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
