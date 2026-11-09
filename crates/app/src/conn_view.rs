//! The connection manager as native Windows controls: a search box, a
//! tree view of the hosts in their folders, and Connect, New, Edit and
//! Import buttons, on a panel that fills the uncovered part of the tools
//! pane.
//!
//! The hosts, favourites and search come from the main window's
//! `SessionList` (`SessionList::tree`); what the user picks goes back as an
//! [`Event`] to the handler the main window gives, which carries it out
//! the same way as the drawn list's actions. The session form and the
//! import page are still drawn by the main window; the panel hides while
//! they show.
//!
//! The controls take the keyboard while the connection manager has it.
//! [`pre_translate`] runs in the message loop: it passes Ctrl+Shift
//! shortcuts on to the main window, turns typing in the tree into a
//! search, and gives Tab, Enter and Escape their dialog meaning.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashSet;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW, DT_LEFT, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, GetSysColor,
    GetSysColorBrush, HFONT, HGDIOBJ, LOGFONTW, PAINTSTRUCT, ScreenToClient, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    EM_SETCUEBANNER, EM_SETSEL, HTREEITEM, NM_DBLCLK, NM_RCLICK, NM_SETFOCUS, NMHDR, NMTREEVIEWW,
    NMTVGETINFOTIPW, NMTVKEYDOWN, SetWindowTheme, TVE_TOGGLE, TVGN_CARET, TVGN_ROOT, TVHITTESTINFO,
    TVI_LAST, TVI_ROOT, TVIF_PARAM, TVIF_STATE, TVIF_TEXT, TVINSERTSTRUCTW, TVIS_EXPANDED, TVITEMW,
    TVM_DELETEITEM, TVM_ENSUREVISIBLE, TVM_EXPAND, TVM_GETITEMW, TVM_GETNEXTITEM, TVM_HITTEST,
    TVM_INSERTITEMW, TVM_SELECTITEM, TVM_SETEXTENDEDSTYLE, TVN_GETINFOTIPW, TVN_ITEMEXPANDEDW,
    TVN_KEYDOWN, TVS_EX_DOUBLEBUFFER, TVS_EX_FADEINOUTEXPANDOS, TVS_FULLROWSELECT, TVS_HASBUTTONS,
    TVS_INFOTIP, TVS_LINESATROOT, TVS_SHOWSELALWAYS, WC_BUTTONW, WC_EDITW, WC_TREEVIEWW,
};
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, SetFocus, VK_CONTROL, VK_DELETE, VK_DOWN, VK_F2, VK_F5, VK_INSERT,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, EN_CHANGE,
    EN_SETFOCUS, ES_AUTOHSCROLL, GetClientRect, GetMessagePos, GetWindowTextLengthW,
    GetWindowTextW, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsChild, IsDialogMessageW, IsWindowVisible,
    LoadCursorW, MSG, MoveWindow, NONCLIENTMETRICSW, RegisterClassW, SPI_GETNONCLIENTMETRICS,
    SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CHAR, WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_PAINT, WM_SETFONT, WNDCLASSW,
    WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_CONTROLPARENT, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::layout::Rect;
use crate::sessions::{SideAction, TreeHost, TreeNode};

/// What the user did in the panel.
#[derive(Debug)]
pub enum Event {
    /// Carry this out as the drawn list's action.
    Action(SideAction),
    /// Enter in the search box: open the first match, or what was typed.
    OpenFirst { split: bool },
    /// A right click: the connection menu for this host (`None`: on no host).
    Menu(Option<String>),
    /// Run this menu command (Import).
    Command(u32),
    /// The search text changed.
    Query(String),
    /// One of the controls got the keyboard.
    Focused,
    /// F5: read the hosts again.
    Refresh,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_SEARCH: usize = 100;
const ID_TREE: usize = 101;
const ID_CONNECT: usize = 110;
const ID_NEW: usize = 111;
const ID_EDIT: usize = 112;
const ID_IMPORT: usize = 113;

/// What a tree item stands for (its `lParam` indexes these).
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Folder(String),
    Host(TreeHost),
}

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    search: HWND,
    tree: HWND,
    buttons: [HWND; 4],
}

struct State {
    h: Handles,
    handler: Handler,
    import: u32,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    /// The tree as shown, to rebuild it only when it changes.
    nodes: Option<Vec<TreeNode>>,
    items: Vec<Item>,
    /// Folders the user folded.
    collapsed: HashSet<String>,
    /// The width the controls are laid out for (the tool's whole width).
    width: i32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn handles() -> Option<Handles> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| s.h))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(dip: i32, dpi: u32) -> i32 {
    dip * dpi as i32 / 96
}

/// The system message font at `dpi`, and a larger, semi-bold one for the
/// title.
fn fonts(dpi: u32) -> (HFONT, HFONT) {
    let mut m = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: a correctly sized struct for the call to fill.
    let ok = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            m.cbSize,
            Some((&raw mut m).cast()),
            0,
            dpi,
        )
    }
    .is_ok();
    let mut lf = if ok {
        m.lfMessageFont
    } else {
        let mut lf = LOGFONTW {
            lfHeight: -scale(12, dpi),
            lfWeight: 400,
            ..Default::default()
        };
        for (d, s) in lf.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
            *d = s;
        }
        lf
    };
    let mut title = lf;
    title.lfHeight = lf.lfHeight * 4 / 3;
    title.lfWeight = 600;
    lf.lfQuality = windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY;
    title.lfQuality = lf.lfQuality;
    // SAFETY: valid LOGFONTs; the fonts are deleted when replaced.
    unsafe { (CreateFontIndirectW(&lf), CreateFontIndirectW(&title)) }
}

fn set_font(hwnd: HWND, font: HFONT) {
    // SAFETY: plain message to our own control.
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}

/// Creates the panel, hidden, as a child of `main`. `import` is the menu
/// command its Import button runs.
pub fn create(main: HWND, handler: Handler, import: u32, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering a class and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHConnections");
        let wc = WNDCLASSW {
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
        let cue = wide("Search connections");
        SendMessageW(
            search,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        let tree = child(
            WC_TREEVIEWW,
            w!(""),
            TVS_HASBUTTONS | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_FULLROWSELECT | TVS_INFOTIP,
            ID_TREE,
        )?;
        // The look of Explorer's navigation pane.
        let _ = SetWindowTheme(tree, w!("Explorer"), PCWSTR::null());
        let ex = TVS_EX_DOUBLEBUFFER | TVS_EX_FADEINOUTEXPANDOS;
        SendMessageW(
            tree,
            TVM_SETEXTENDEDSTYLE,
            Some(WPARAM(ex as usize)),
            Some(LPARAM(ex as isize)),
        );
        let buttons = [
            child(
                WC_BUTTONW,
                w!("Connect"),
                BS_DEFPUSHBUTTON as u32,
                ID_CONNECT,
            )?,
            child(WC_BUTTONW, w!("New"), BS_PUSHBUTTON as u32, ID_NEW)?,
            child(WC_BUTTONW, w!("Edit"), BS_PUSHBUTTON as u32, ID_EDIT)?,
            child(WC_BUTTONW, w!("Import..."), BS_PUSHBUTTON as u32, ID_IMPORT)?,
        ];
        let (font, title_font) = fonts(dpi);
        for c in [search, tree].iter().chain(&buttons) {
            set_font(*c, font);
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                h: Handles {
                    main,
                    panel,
                    search,
                    tree,
                    buttons,
                },
                handler,
                import,
                font,
                title_font,
                dpi,
                nodes: None,
                items: Vec::new(),
                collapsed: HashSet::new(),
                width: 0,
            });
        });
    }
    Ok(())
}

/// Puts the panel over `at` (the uncovered part of the tools pane), with
/// its controls laid out for the tool's whole `width`; `None` hides it.
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
        s.width = width;
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
        for c in [h.search, h.tree].iter().chain(&h.buttons) {
            set_font(*c, font);
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
    let tree_y = title + field + gap;
    let tree_h = (buttons_y - gap - tree_y).max(0);
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        let _ = MoveWindow(h.search, pad, title, inner, field, true);
        let _ = MoveWindow(h.tree, pad, tree_y, inner, tree_h, true);
        let mut x = pad;
        for (b, w) in h.buttons.iter().zip([76, 56, 56, 76]) {
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

/// Gives the tree the keyboard (selecting its first item if none is).
pub fn focus_tree() {
    let Some(h) = handles() else {
        return;
    };
    // SAFETY: our own controls.
    unsafe {
        if selected(h.tree).is_none() {
            let first = HTREEITEM(
                SendMessageW(
                    h.tree,
                    TVM_GETNEXTITEM,
                    Some(WPARAM(TVGN_ROOT as usize)),
                    None,
                )
                .0,
            );
            if first.0 != 0 {
                select(h.tree, first);
            }
        }
        let _ = SetFocus(Some(h.tree));
    }
}

fn select(tree: HWND, item: HTREEITEM) {
    // SAFETY: our own control and one of its items.
    unsafe {
        SendMessageW(
            tree,
            TVM_SELECTITEM,
            Some(WPARAM(TVGN_CARET as usize)),
            Some(LPARAM(item.0)),
        );
    }
}

/// The selected item's index into `State::items`.
fn selected(tree: HWND) -> Option<usize> {
    // SAFETY: our own control; the item struct is filled by the call.
    unsafe {
        let item = SendMessageW(
            tree,
            TVM_GETNEXTITEM,
            Some(WPARAM(TVGN_CARET as usize)),
            None,
        );
        item_index(tree, HTREEITEM(item.0))
    }
}

fn item_index(tree: HWND, item: HTREEITEM) -> Option<usize> {
    if item.0 == 0 {
        return None;
    }
    let mut tv = TVITEMW {
        mask: TVIF_PARAM,
        hItem: item,
        ..Default::default()
    };
    // SAFETY: a valid item struct for our own control.
    let ok = unsafe {
        SendMessageW(
            tree,
            TVM_GETITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&raw mut tv as isize)),
        )
    };
    (ok.0 != 0).then_some(tv.lParam.0 as usize)
}

fn item(index: usize) -> Option<Item> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref()?.items.get(index).cloned())
}

fn selected_item() -> Option<Item> {
    item(selected(handles()?.tree)?)
}

fn selected_host() -> Option<String> {
    match selected_item()? {
        Item::Host(h) => Some(h.name),
        Item::Folder(_) => None,
    }
}

/// Shows `nodes` in the tree, unless it shows them already. The selection
/// stays on the same host.
pub fn set_nodes(nodes: Vec<TreeNode>) {
    let Some(h) = handles() else {
        return;
    };
    let keep = selected_host();
    let built = STATE.with(|s| {
        let mut s = s.try_borrow_mut().ok()?;
        let s = s.as_mut()?;
        if s.nodes.as_ref() == Some(&nodes) {
            return None;
        }
        // Item indices follow the order the items are inserted in below.
        let mut items = Vec::new();
        for n in &nodes {
            if let TreeNode::Folder { name, .. } = n {
                items.push(Item::Folder(name.clone()));
            }
            items.extend(n.hosts().iter().cloned().map(Item::Host));
        }
        s.items = items;
        s.nodes = Some(nodes.clone());
        Some(s.collapsed.clone())
    });
    let Some(collapsed) = built else {
        return;
    };
    let insert = |parent: HTREEITEM, text: &str, open: bool, index: usize| {
        let mut text = wide(text);
        let mut ins = TVINSERTSTRUCTW {
            hParent: parent,
            hInsertAfter: TVI_LAST,
            ..Default::default()
        };
        ins.Anonymous.item = TVITEMW {
            mask: TVIF_TEXT | TVIF_PARAM | TVIF_STATE,
            pszText: PWSTR(text.as_mut_ptr()),
            state: if open {
                TVIS_EXPANDED
            } else {
                Default::default()
            },
            stateMask: TVIS_EXPANDED,
            lParam: LPARAM(index as isize),
            ..Default::default()
        };
        // SAFETY: our own control; the struct and text live through the
        // call, which copies them.
        HTREEITEM(
            unsafe {
                SendMessageW(
                    h.tree,
                    TVM_INSERTITEMW,
                    Some(WPARAM(0)),
                    Some(LPARAM(&raw const ins as isize)),
                )
            }
            .0,
        )
    };
    // SAFETY: our own control.
    unsafe {
        SendMessageW(
            h.tree,
            TVM_DELETEITEM,
            Some(WPARAM(0)),
            Some(LPARAM(TVI_ROOT.0)),
        );
    }
    let mut index = 0;
    let mut reselect = None;
    let mut host = |parent: HTREEITEM, t: &TreeHost, index: &mut usize| {
        let made = insert(parent, &t.label, false, *index);
        *index += 1;
        if keep.as_deref() == Some(t.name.as_str()) {
            reselect = Some(made);
        }
    };
    for n in &nodes {
        match n {
            TreeNode::Folder { name, hosts } => {
                let folder = insert(TVI_ROOT, name, !collapsed.contains(name), index);
                index += 1;
                for t in hosts {
                    host(folder, t, &mut index);
                }
            }
            TreeNode::Host(t) => host(TVI_ROOT, t, &mut index),
        }
    }
    if let Some(item) = reselect {
        select(h.tree, item);
        // SAFETY: our own control and one of its items.
        unsafe {
            SendMessageW(
                h.tree,
                TVM_ENSUREVISIBLE,
                Some(WPARAM(0)),
                Some(LPARAM(item.0)),
            );
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

fn key_down(vk: u16) -> bool {
    // SAFETY: plain query.
    unsafe { GetKeyState(i32::from(vk)) < 0 }
}

fn search_text(search: HWND) -> String {
    // SAFETY: a buffer sized from the control's own length.
    unsafe {
        let n = GetWindowTextLengthW(search).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(search, &mut buf).max(0) as usize;
        String::from_utf16_lossy(buf.get(..got).unwrap_or_default())
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
                // The window's shortcuts (Ctrl+Shift+S hides this pane).
                if key_down(VK_CONTROL.0) && key_down(VK_SHIFT.0) {
                    let mut m = *msg;
                    m.hwnd = h.main;
                    let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&m);
                    windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&m);
                    return true;
                }
                if msg.hwnd == h.search && vk == VK_DOWN.0 {
                    focus_tree();
                    return true;
                }
            }
            WM_CHAR if msg.hwnd == h.tree => {
                // Typing in the tree searches, as in the drawn list.
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
            _ => {}
        }
        // Alt keys go on to the menu bar as usual.
        if matches!(
            msg.message,
            windows::Win32::UI::WindowsAndMessaging::WM_SYSKEYDOWN
                | windows::Win32::UI::WindowsAndMessaging::WM_SYSKEYUP
                | windows::Win32::UI::WindowsAndMessaging::WM_SYSCHAR
        ) {
            return false;
        }
        // Tab between the controls, Enter (IDOK) and Escape (IDCANCEL).
        IsDialogMessageW(h.panel, msg).as_bool()
    }
}

/// Enter, the Connect button and a double click: open the selected host
/// (in a split with Shift), or fold the selected folder.
fn open_selected(h: Handles, from_search: bool) {
    let split = key_down(VK_SHIFT.0);
    if from_search {
        emit(Event::OpenFirst { split });
        return;
    }
    match selected_item() {
        Some(Item::Host(host)) => emit(Event::Action(SideAction::Open {
            host: host.name,
            split,
        })),
        Some(Item::Folder(_)) => {
            // SAFETY: our own control and its selected item.
            unsafe {
                let item = SendMessageW(
                    h.tree,
                    TVM_GETNEXTITEM,
                    Some(WPARAM(TVGN_CARET as usize)),
                    None,
                );
                SendMessageW(
                    h.tree,
                    TVM_EXPAND,
                    Some(WPARAM(TVE_TOGGLE.0 as usize)),
                    Some(LPARAM(item.0)),
                );
            }
        }
        None if !search_text(h.search).trim().is_empty() => emit(Event::OpenFirst { split }),
        None => {}
    }
}

fn command(h: Handles, id: usize, code: u32) {
    // SAFETY: plain query.
    let focus = unsafe { GetFocus() };
    match (id, code) {
        (ID_SEARCH, EN_CHANGE) => emit(Event::Query(search_text(h.search))),
        (ID_SEARCH, EN_SETFOCUS) => emit(Event::Focused),
        (id, _) if id == IDOK.0 as usize => open_selected(h, focus == h.search),
        (id, _) if id == IDCANCEL.0 as usize => {
            if search_text(h.search).is_empty() {
                emit(Event::Action(SideAction::Leave));
            } else {
                // SAFETY: our own control; EN_CHANGE clears the search.
                unsafe {
                    let _ = SetWindowTextW(h.search, w!(""));
                }
            }
        }
        (ID_CONNECT, BN_CLICKED) => open_selected(h, selected_host().is_none()),
        (ID_NEW, BN_CLICKED) => emit(Event::Action(SideAction::New)),
        (ID_EDIT, BN_CLICKED) => emit(Event::Action(match selected_host() {
            Some(host) => SideAction::Edit(host),
            None => SideAction::New,
        })),
        (ID_IMPORT, BN_CLICKED) => {
            let id = STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| s.import));
            if let Some(id) = id {
                emit(Event::Command(id));
            }
        }
        _ => {}
    }
}

/// The tree's notifications; the result for `WM_NOTIFY`.
fn notify(h: Handles, lparam: LPARAM) -> LRESULT {
    // SAFETY: Windows passes a valid NMHDR (and the larger struct its code
    // says) for the duration of WM_NOTIFY.
    unsafe {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.hwndFrom != h.tree {
            return LRESULT(0);
        }
        match hdr.code {
            NM_SETFOCUS => emit(Event::Focused),
            NM_DBLCLK => {
                if selected_host().is_some() {
                    open_selected(h, false);
                    return LRESULT(1);
                }
            }
            NM_RCLICK => {
                let pos = GetMessagePos();
                let mut pt = POINT {
                    x: i32::from((pos & 0xFFFF) as u16 as i16),
                    y: i32::from((pos >> 16) as u16 as i16),
                };
                let _ = ScreenToClient(h.tree, &mut pt);
                let mut hit = TVHITTESTINFO {
                    pt,
                    ..Default::default()
                };
                let hit_item = HTREEITEM(
                    SendMessageW(
                        h.tree,
                        TVM_HITTEST,
                        Some(WPARAM(0)),
                        Some(LPARAM(&raw mut hit as isize)),
                    )
                    .0,
                );
                if hit_item.0 != 0 {
                    select(h.tree, hit_item);
                }
                let host = item_index(h.tree, hit_item)
                    .and_then(item)
                    .and_then(|i| match i {
                        Item::Host(h) => Some(h.name),
                        Item::Folder(_) => None,
                    });
                emit(Event::Menu(host));
                return LRESULT(1);
            }
            TVN_KEYDOWN => {
                let key = &*(lparam.0 as *const NMTVKEYDOWN);
                let vk = key.wVKey;
                let ctrl = key_down(VK_CONTROL.0);
                let action = match vk {
                    v if v == VK_F2.0 => Some(match selected_host() {
                        Some(host) => SideAction::Edit(host),
                        None => SideAction::New,
                    }),
                    v if v == VK_DELETE.0 => selected_host().map(SideAction::Delete),
                    v if v == VK_INSERT.0 => Some(SideAction::New),
                    v if v == u16::from(b'D') && ctrl => {
                        selected_host().map(SideAction::ToggleFavourite)
                    }
                    v if v == VK_F5.0 => {
                        emit(Event::Refresh);
                        return LRESULT(1);
                    }
                    _ => return LRESULT(0),
                };
                if let Some(a) = action {
                    emit(Event::Action(a));
                }
                return LRESULT(1);
            }
            TVN_ITEMEXPANDEDW => {
                let tv = &*(lparam.0 as *const NMTREEVIEWW);
                let open = tv.itemNew.state.0 & TVIS_EXPANDED.0 != 0;
                let index = tv.itemNew.lParam.0 as usize;
                STATE.with(|s| {
                    if let Ok(mut s) = s.try_borrow_mut()
                        && let Some(s) = s.as_mut()
                        && let Some(Item::Folder(name)) = s.items.get(index).cloned()
                    {
                        if open {
                            s.collapsed.remove(&name);
                        } else {
                            s.collapsed.insert(name);
                        }
                    }
                });
            }
            TVN_GETINFOTIPW => {
                let tip = &mut *(lparam.0 as *mut NMTVGETINFOTIPW);
                if let Some(Item::Host(host)) = item(tip.lParam.0 as usize)
                    && tip.cchTextMax > 0
                    && !tip.pszText.is_null()
                {
                    let text: Vec<u16> = host.tip.encode_utf16().collect();
                    let n = text.len().min(tip.cchTextMax as usize - 1);
                    let out = std::slice::from_raw_parts_mut(tip.pszText.0, n + 1);
                    if let (Some(dst), Some(src)) = (out.get_mut(..n), text.get(..n)) {
                        dst.copy_from_slice(src);
                    }
                    if let Some(end) = out.get_mut(n) {
                        *end = 0;
                    }
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

fn paint(h: Handles) {
    let Some((title_font, dpi)) =
        STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| (s.title_font, s.dpi)))
    else {
        return;
    };
    // SAFETY: a BeginPaint/EndPaint pair on our own window; the font is
    // selected back out.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h.panel, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(h.panel, &mut rc);
        FillRect(dc, &rc, GetSysColorBrush(COLOR_WINDOW));
        let old = SelectObject(dc, HGDIOBJ(title_font.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(
            dc,
            windows::Win32::Foundation::COLORREF(GetSysColor(COLOR_WINDOWTEXT)),
        );
        let pad = scale(10, dpi);
        let mut tr = RECT {
            left: pad,
            top: 0,
            right: rc.right,
            bottom: scale(36, dpi),
        };
        let mut text: Vec<u16> = "Connections".encode_utf16().collect();
        DrawTextW(
            dc,
            &mut text,
            &mut tr,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
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
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
