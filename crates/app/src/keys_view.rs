//! Settings > Key bindings: a window over the main one that lists every
//! action of the menus with its shortcuts and its default ones, as native
//! Windows controls. [`ask`] runs it modally, like a message box, and
//! returns the new [`Keymap`] on OK.
//!
//! Change (Enter, a double click) and Add wait for a key to be pressed:
//! the next key with its modifiers becomes the action's (Esc cancels).
//! A key another action has moves to this one, after asking; a key that
//! would stop typing working is refused, and one programs in the terminal
//! use (Ctrl+C, a tmux prefix) is taken only after a warning
//! ([`keymap::check`]). Reset to default puts one action's keys back;
//! Reset all, every action's. Nothing changes until OK.

#![allow(unsafe_code)]

use std::cell::RefCell;

use ferossh_config::hotkey::{self, Hotkey};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, DeleteObject, FillRect, HDC, HFONT, HGDIOBJ};
use windows::Win32::UI::Controls::{
    EM_SETCUEBANNER, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_PARAM,
    LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE,
    LVM_GETITEMW, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_ITEMCHANGED,
    LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, LVS_NOSORTHEADER,
    LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_DBLCLK, NMHDR, NMLISTVIEW, SetWindowTheme,
    WC_BUTTONW, WC_EDITW, WC_LISTVIEWW, WC_STATICW,
};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_RETURN,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    DestroyWindow, DispatchMessageW, EN_CHANGE, ES_AUTOHSCROLL, GetClientRect, GetMessageW,
    GetWindowRect, HMENU, IDC_ARROW, IDCANCEL, IDOK, IDYES, IsDialogMessageW, LoadCursorW,
    MB_ICONWARNING, MB_YESNO, MSG, MessageBoxW, PostQuitMessage, RegisterClassW, SW_SHOW,
    SendMessageW, SetForegroundWindow, SetWindowTextW, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN,
    WM_KEYUP, WM_NOTIFY, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use crate::keymap::{self, Check, Keymap, When};
use crate::native::{fonts, scale, search_text, set_font, wide};

/// An action the window lists.
#[derive(Debug, Clone)]
pub struct Action {
    /// The menu command id.
    pub id: u32,
    /// "Window: Split right", as the command palette names it.
    pub name: String,
}

/// What else the checks need to know.
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// The tmux prefix of the open tmux sessions, if FeroSSH knows one.
    pub tmux_prefix: Option<Hotkey>,
    /// The standby window's hotkey, when standby is on.
    pub standby: Option<Hotkey>,
    /// Shows a key as people read it (the layout's character on
    /// punctuation keys).
    pub label: fn(Hotkey) -> String,
}

const ID_SEARCH: usize = 100;
const ID_LIST: usize = 101;
const ID_STATUS: usize = 102;
const ID_CHANGE: usize = 110;
const ID_ADD: usize = 111;
const ID_CLEAR: usize = 112;
const ID_RESET: usize = 113;
const ID_RESET_ALL: usize = 114;
const SS_NOPREFIX: u32 = 0x80;

/// Width and height of the window's inside, in DIPs.
const WIDTH: i32 = 640;
const HEIGHT: i32 = 480;

/// Columns: title and width in DIPs.
const COLUMNS: [(&str, i32); 3] = [("Action", 300), ("Shortcut", 150), ("Default", 150)];

#[derive(Clone, Copy)]
struct Handles {
    dlg: HWND,
    search: HWND,
    list: HWND,
    status: HWND,
    buttons: [HWND; 4],
}

struct State {
    h: Handles,
    actions: Vec<Action>,
    keys: Keymap,
    ctx: Context,
    /// Waiting for a key for this action; `true` to add it to the keys
    /// the action has, `false` to replace them.
    capture: Option<(u32, bool)>,
    /// `None` while the window is open; then the answer.
    done: Option<Option<Keymap>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Shows the window over `owner` and waits for OK or Cancel.
pub fn ask(
    owner: HWND,
    dpi: u32,
    actions: Vec<Action>,
    keys: Keymap,
    ctx: Context,
) -> Option<Keymap> {
    // SAFETY: a class with valid static strings and a window procedure
    // of ours; the window and its controls are destroyed before return.
    let dlg = unsafe { create(owner, dpi) }.ok()?;
    STATE.with(|st| {
        if let Some(s) = st.borrow_mut().as_mut() {
            s.actions = actions;
            s.keys = keys;
            s.ctx = ctx;
        }
    });
    fill(None);
    let (font, title) = fonts(dpi);
    // SAFETY: the title font is not used here.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(title.0));
    }
    set_fonts(dlg, font);
    crate::ui_theme::restyle(dlg);
    crate::ui_theme::popup_frame(dlg);
    // SAFETY: the modal loop of a message box: the owner is disabled
    // while the window is up; every message is dispatched as usual, so
    // the main window keeps painting and reading its connections. No
    // state borrow is held while a message runs.
    let answer = unsafe {
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        if let Some(h) = with(|s| s.h) {
            let _ = SetFocus(Some(h.list));
        }
        let mut msg = MSG::default();
        loop {
            if let Some(done) = with(|s| s.done.take()).flatten() {
                break done;
            }
            let got = GetMessageW(&mut msg, None, 0, 0);
            if got.0 == 0 {
                // WM_QUIT: hand it on to the main loop.
                PostQuitMessage(msg.wParam.0 as i32);
                break None;
            }
            if got.0 == -1 {
                break None;
            }
            if capture_key(&msg) || enter(&msg) {
                continue;
            }
            if !IsDialogMessageW(dlg, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    };
    // SAFETY: the owner first, so it gets the activation back, then our
    // own window and font.
    unsafe {
        let _ = EnableWindow(owner, true);
        let _ = SetForegroundWindow(owner);
        let _ = DestroyWindow(dlg);
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
    STATE.with(|s| s.borrow_mut().take());
    answer
}

fn set_fonts(dlg: HWND, font: HFONT) {
    set_font(dlg, font);
    // SAFETY: walking our own window's children.
    unsafe {
        let mut c = windows::Win32::UI::WindowsAndMessaging::GetWindow(
            dlg,
            windows::Win32::UI::WindowsAndMessaging::GW_CHILD,
        )
        .ok();
        while let Some(h) = c {
            set_font(h, font);
            c = windows::Win32::UI::WindowsAndMessaging::GetWindow(
                h,
                windows::Win32::UI::WindowsAndMessaging::GW_HWNDNEXT,
            )
            .ok();
        }
    }
}

unsafe fn create(owner: HWND, dpi: u32) -> windows::core::Result<HWND> {
    // SAFETY: the caller's promise; see `ask`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHKeyBindings");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let s = |d: i32| scale(d, dpi);
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: s(WIDTH),
            bottom: s(HEIGHT),
        };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut o = RECT::default();
        let _ = GetWindowRect(owner, &mut o);
        let x = o.left + ((o.right - o.left) - w) / 2;
        let y = o.top + ((o.bottom - o.top) - h) / 3;
        let dlg = CreateWindowExW(
            ex,
            class,
            w!("Key bindings"),
            style,
            x,
            y,
            w,
            h,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )?;
        let child = |class: PCWSTR, text: &str, style: u32, id: usize, at: (i32, i32, i32, i32)| {
            let text = wide(text);
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                s(at.0),
                s(at.1),
                s(at.2),
                s(at.3),
                Some(dlg),
                Some(HMENU(id as *mut _)),
                Some(instance.into()),
                None,
            )
        };
        let search = child(
            WC_EDITW,
            "",
            WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
            ID_SEARCH,
            (12, 12, WIDTH - 24, 24),
        )?;
        let cue = wide("Search actions or keys");
        SendMessageW(
            search,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        let list = child(
            WC_LISTVIEWW,
            "",
            WS_TABSTOP.0
                | WS_BORDER.0
                | LVS_REPORT
                | LVS_SINGLESEL
                | LVS_SHOWSELALWAYS
                | LVS_NOSORTHEADER,
            ID_LIST,
            (12, 44, WIDTH - 24, 270),
        )?;
        let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
        let lex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP;
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(lex as usize)),
            Some(LPARAM(lex as isize)),
        );
        for (i, (title, width)) in COLUMNS.iter().enumerate() {
            let mut text = wide(title);
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: s(*width),
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
        let push = WS_TABSTOP.0 | BS_PUSHBUTTON as u32;
        let row = 322;
        let buttons = [
            child(
                WC_BUTTONW,
                "&Change...",
                push,
                ID_CHANGE,
                (12, row, 100, 28),
            )?,
            child(WC_BUTTONW, "Add &key...", push, ID_ADD, (120, row, 100, 28))?,
            child(
                WC_BUTTONW,
                "&No shortcut",
                push,
                ID_CLEAR,
                (228, row, 100, 28),
            )?,
            child(
                WC_BUTTONW,
                "Reset to &default",
                push,
                ID_RESET,
                (336, row, 120, 28),
            )?,
        ];
        let status = child(
            WC_STATICW,
            "",
            SS_NOPREFIX,
            ID_STATUS,
            (12, 360, WIDTH - 24, 70),
        )?;
        let bottom = HEIGHT - 12 - 28;
        child(
            WC_BUTTONW,
            "Reset &all",
            push,
            ID_RESET_ALL,
            (12, bottom, 100, 28),
        )?;
        child(
            WC_BUTTONW,
            "OK",
            WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
            IDOK.0 as usize,
            (WIDTH - 12 - 88 - 8 - 88, bottom, 88, 28),
        )?;
        child(
            WC_BUTTONW,
            "Cancel",
            push,
            IDCANCEL.0 as usize,
            (WIDTH - 12 - 88, bottom, 88, 28),
        )?;
        STATE.with(|st| {
            *st.borrow_mut() = Some(State {
                h: Handles {
                    dlg,
                    search,
                    list,
                    status,
                    buttons,
                },
                actions: Vec::new(),
                keys: Keymap::new(&[], &Default::default()),
                ctx: Context {
                    tmux_prefix: None,
                    standby: None,
                    label: |k| k.to_string(),
                },
                capture: None,
                done: None,
            });
        });
        Ok(dlg)
    }
}

fn set_text(hwnd: HWND, text: &str) {
    let t = wide(text);
    // SAFETY: our own control; the text lives through the call.
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(t.as_ptr()));
    }
}

fn labels(keys: &[Hotkey], label: fn(Hotkey) -> String) -> String {
    keys.iter()
        .map(|k| label(*k))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The list again, as the search keeps it, with `keep` (or the first
/// row) selected.
fn fill(keep: Option<u32>) {
    let Some((h, rows)) = with(|s| {
        let query = search_text(s.h.search).to_lowercase();
        let label = s.ctx.label;
        let rows: Vec<(u32, [String; 3])> = s
            .actions
            .iter()
            .map(|a| {
                let now = labels(s.keys.keys(a.id), label);
                let default = labels(s.keys.default_keys(a.id), label);
                let mark = if s.keys.is_default(a.id) {
                    ""
                } else {
                    "\u{2022} "
                };
                (a.id, [a.name.clone(), format!("{mark}{now}"), default])
            })
            .filter(|(_, cells)| {
                query.is_empty() || cells.iter().any(|c| c.to_lowercase().contains(&query))
            })
            .collect();
        (s.h, rows)
    }) else {
        return;
    };
    let mut select = None;
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        SendMessageW(h.list, LVM_DELETEALLITEMS, None, None);
        for (i, (id, cells)) in rows.iter().enumerate() {
            let mut label = wide(&cells[0]);
            let item = LVITEMW {
                mask: LVIF_TEXT | LVIF_PARAM,
                iItem: i as i32,
                pszText: PWSTR(label.as_mut_ptr()),
                lParam: LPARAM(*id as isize),
                ..Default::default()
            };
            let at = SendMessageW(
                h.list,
                LVM_INSERTITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const item as isize)),
            )
            .0;
            for (sub, text) in cells.iter().enumerate().skip(1) {
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
            if Some(*id) == keep {
                select = Some(i);
            }
        }
    }
    if !rows.is_empty() {
        select_row(h.list, select.unwrap_or(0) as i32);
    }
    show_selected();
}

fn select_row(list: HWND, index: i32) {
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
            Some(WPARAM(index as usize)),
            Some(LPARAM(&raw const item as isize)),
        );
        SendMessageW(
            list,
            LVM_ENSUREVISIBLE,
            Some(WPARAM(index as usize)),
            Some(LPARAM(0)),
        );
    }
}

/// The selected action's id.
fn selected() -> Option<u32> {
    let h = with(|s| s.h)?;
    // SAFETY: plain messages to our own control; the struct lives
    // through the call.
    unsafe {
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
        u32::try_from(item.lParam.0).ok()
    }
}

fn name_of(s: &State, id: u32) -> String {
    s.actions
        .iter()
        .find(|a| a.id == id)
        .map_or_else(|| format!("action {id}"), |a| a.name.clone())
}

/// The status line for the selected action, and its buttons enabled as
/// they apply.
fn show_selected() {
    let id = selected();
    let Some((h, text, has_keys, changed)) = with(|s| {
        let Some(id) = id else {
            return (s.h, String::new(), false, false);
        };
        let label = s.ctx.label;
        let now = s.keys.keys(id);
        let mut text = if now.is_empty() {
            format!("{} has no shortcut.", name_of(s, id))
        } else {
            format!("{}: {}.", name_of(s, id), labels(now, label))
        };
        match s.keys.when(id) {
            When::Always => {}
            When::Tabs => text.push_str(
                " Its keys work while the window has more than one tab; with one, they go \
                 to the terminal.",
            ),
            When::Split => text.push_str(
                " Its keys work while the tab has more than one pane; with one, they go to \
                 the terminal.",
            ),
        }
        if !s.keys.is_default(id) {
            let default = s.keys.default_keys(id);
            text.push_str(&if default.is_empty() {
                " By default it has none.".to_owned()
            } else {
                format!(" By default: {}.", labels(default, label))
            });
        }
        text.push_str(
            "\n\nChange (Enter or a double click) and Add key wait for the keys to press; \
             keys with Ctrl+Shift are the safest.",
        );
        (s.h, text, !now.is_empty(), !s.keys.is_default(id))
    }) else {
        return;
    };
    set_text(h.status, &text);
    // SAFETY: our own controls.
    unsafe {
        let [change, add, clear, reset] = h.buttons;
        let _ = EnableWindow(change, id.is_some());
        let _ = EnableWindow(add, id.is_some());
        let _ = EnableWindow(clear, has_keys);
        let _ = EnableWindow(reset, changed);
    }
}

/// Change or Add key: waits for the key.
fn start_capture(add: bool) {
    let Some(id) = selected() else {
        return;
    };
    let Some((h, name)) = with(|s| {
        s.capture = Some((id, add));
        (s.h, name_of(s, id))
    }) else {
        return;
    };
    let what = if add {
        "another shortcut for"
    } else {
        "the new shortcut for"
    };
    set_text(
        h.status,
        &format!("Press {what} {name}, with Ctrl, Alt or Shift as you want them. Esc cancels."),
    );
    // SAFETY: our own control; the keys come to the modal loop.
    unsafe {
        let _ = SetFocus(Some(h.list));
    }
}

/// While waiting for a key: takes every key message, so none reaches a
/// control (Alt does not open the window menu, Enter presses nothing).
/// `true` when the message was ours.
fn capture_key(msg: &MSG) -> bool {
    let Some(Some((id, add))) = with(|s| s.capture) else {
        return false;
    };
    match msg.message {
        WM_KEYDOWN | WM_SYSKEYDOWN => {}
        WM_KEYUP | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR => return true,
        _ => return false,
    }
    let vk = msg.wParam.0 as u16;
    if hotkey::is_modifier(vk) {
        return true;
    }
    // SAFETY: plain queries.
    let down = |k: u16| unsafe { GetKeyState(i32::from(k)) } < 0;
    let key = keymap::chord(vk, down(VK_CONTROL.0), down(VK_MENU.0), down(VK_SHIFT.0));
    with(|s| s.capture = None);
    if vk == VK_ESCAPE.0 && !key.ctrl && !key.alt && !key.shift {
        show_selected();
        return true;
    }
    take(id, add, key);
    true
}

/// Enter in the search box or the list: Change, as a double click.
fn enter(msg: &MSG) -> bool {
    if msg.message != WM_KEYDOWN || msg.wParam.0 != usize::from(VK_RETURN.0) {
        return false;
    }
    let Some(h) = with(|s| s.h) else {
        return false;
    };
    // SAFETY: a plain query.
    let focus = unsafe { GetFocus() };
    if focus != h.list && focus != h.search {
        return false;
    }
    start_capture(false);
    true
}

/// Gives `key` to action `id` (added to its keys with `add`), after the
/// checks and, for a warning or a key another action has, the user's yes.
fn take(id: u32, add: bool, key: Hotkey) {
    let Some((h, label, name, owner, check, standby)) = with(|s| {
        let label = (s.ctx.label)(key);
        let owner = s.keys.owner(key).filter(|o| *o != id);
        (
            s.h,
            label.clone(),
            name_of(s, id),
            owner.map(|o| name_of(s, o)),
            keymap::check(key, &label, s.ctx.tmux_prefix),
            s.ctx.standby == Some(key),
        )
    }) else {
        return;
    };
    if let Check::Refuse(why) = check {
        set_text(h.status, &why);
        return;
    }
    let mut notes = Vec::new();
    if let Some(o) = &owner {
        notes.push(format!(
            "{label} is the shortcut of {o} now; it moves to {name}."
        ));
    }
    if standby {
        notes.push(format!(
            "{label} is the standby window's hotkey (Window > Standby window), which Windows \
             gives to the standby window first: this binding would not run while standby is \
             on."
        ));
    }
    if let Check::Warn(why) = check {
        notes.push(why);
    }
    if !notes.is_empty() {
        let text = format!("Give {label} to {name}?\n\n{}", notes.join("\n\n"));
        if !confirm(h.dlg, &text) {
            show_selected();
            return;
        }
    }
    with(|s| {
        if add {
            s.keys.add(id, key);
        } else {
            s.keys.set(id, vec![key]);
        }
    });
    fill(Some(id));
}

fn confirm(dlg: HWND, text: &str) -> bool {
    // SAFETY: a message box over our own window; no state borrow is held.
    unsafe {
        MessageBoxW(
            Some(dlg),
            &HSTRING::from(text),
            w!("Key bindings"),
            MB_YESNO | MB_ICONWARNING,
        ) == IDYES
    }
}

fn reset(id: u32) {
    let Some((h, takes)) = with(|s| {
        let label = s.ctx.label;
        let takes: Vec<String> = s
            .keys
            .reset_takes(id)
            .into_iter()
            .map(|(k, o)| format!("{} from {}", label(k), name_of(s, o)))
            .collect();
        (s.h, takes)
    }) else {
        return;
    };
    if !takes.is_empty() {
        let text = format!(
            "Its default keys are other actions' now. Resetting takes back {}.",
            takes.join(", ")
        );
        if !confirm(h.dlg, &text) {
            return;
        }
    }
    with(|s| s.keys.reset(id));
    fill(Some(id));
}

fn reset_all() {
    let Some((h, any)) = with(|s| (s.h, s.keys.any_changed())) else {
        return;
    };
    if !any {
        return;
    }
    if !confirm(
        h.dlg,
        "Put every action's shortcuts back as FeroSSH has them? The changes are lost once \
         you choose OK.",
    ) {
        return;
    }
    let keep = selected();
    with(|s| s.keys.reset_all());
    fill(keep);
}

fn command(id: usize, code: u32) {
    // A button pressed while waiting for a key ends the wait.
    if code == BN_CLICKED {
        with(|s| s.capture = None);
    }
    match (id, code) {
        (x, _) if x == IDOK.0 as usize => {
            with(|s| s.done = Some(Some(s.keys.clone())));
        }
        (x, _) if x == IDCANCEL.0 as usize => {
            with(|s| s.done = Some(None));
        }
        (ID_SEARCH, EN_CHANGE) => fill(selected()),
        (ID_CHANGE, BN_CLICKED) => start_capture(false),
        (ID_ADD, BN_CLICKED) => start_capture(true),
        (ID_CLEAR, BN_CLICKED) => {
            if let Some(id) = selected() {
                with(|s| s.keys.set(id, Vec::new()));
                fill(Some(id));
            }
        }
        (ID_RESET, BN_CLICKED) => {
            if let Some(id) = selected() {
                reset(id);
            }
        }
        (ID_RESET_ALL, BN_CLICKED) => reset_all(),
        _ => {}
    }
}

fn notify(lparam: LPARAM) -> LRESULT {
    let Some(h) = with(|s| s.h) else {
        return LRESULT(0);
    };
    // SAFETY: Windows passes a valid NMHDR (and the larger struct its code
    // says) for the duration of WM_NOTIFY.
    unsafe {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.hwndFrom != h.list {
            return LRESULT(0);
        }
        match hdr.code {
            NM_DBLCLK => start_capture(false),
            LVN_ITEMCHANGED => {
                let nm = &*(lparam.0 as *const NMLISTVIEW);
                let waiting = with(|s| s.capture.is_some()).unwrap_or(false);
                if nm.uNewState & LVIS_SELECTED.0 != 0 && !waiting {
                    show_selected();
                }
            }
            _ => {}
        }
    }
    LRESULT(0)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            command(wparam.0 & 0xFFFF, ((wparam.0 >> 16) & 0xFFFF) as u32);
            LRESULT(0)
        }
        WM_NOTIFY => notify(lparam),
        WM_CLOSE => {
            with(|s| s.done = Some(None));
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let mut r = RECT::default();
            // SAFETY: the DC Windows gives for our own window.
            unsafe {
                let _ = GetClientRect(hwnd, &mut r);
                FillRect(
                    HDC(wparam.0 as *mut _),
                    &r,
                    crate::ui_theme::sys_brush(COLOR_WINDOW),
                );
            }
            LRESULT(1)
        }
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
