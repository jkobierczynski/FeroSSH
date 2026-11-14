//! The page that asks for a script's arguments, as native Windows
//! controls: a box per argument (`$1` to `$9`), what will be typed below
//! them, and Run and Cancel, on a panel over the uncovered part of the
//! tools pane.
//!
//! The boxes come from `SnippetsView::native_ask`; each change goes back
//! so the preview follows. Enter goes to the next box and runs the script
//! from the last; Shift+Enter or Ctrl+Enter runs it from any box. Escape
//! cancels. [`pre_translate`] runs in the message loop like the other
//! native tools' and passes Ctrl+Shift shortcuts on to the main window.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint,
    FillRect, HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::{EM_SETSEL, WC_BUTTONW, WC_EDITW};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    EN_CHANGE, ES_AUTOHSCROLL, GetClientRect, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsChild,
    IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow, RegisterClassW, SW_HIDE,
    SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND,
    WM_ERASEBKGND, WM_KEYDOWN, WM_PAINT, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW,
    WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_CONTROLPARENT, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};
use crate::snippets_view::NativeAsk;

/// What the user did on the page.
#[derive(Debug)]
pub enum Event {
    /// A box changed: what each says, for the preview.
    Values(Vec<String>),
    /// Run, with what each box says.
    Run(Vec<String>),
    /// Cancel or Escape.
    Cancel,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

/// Arguments are `$1` to `$9`.
const BOXES: usize = 9;
const ID_BOX: usize = 600;
const ID_RUN: usize = 610;
const ID_CANCEL: usize = 611;

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    boxes: [HWND; BOXES],
    run: HWND,
    cancel: HWND,
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    /// The run shown, to fill the boxes only for another one.
    shown: Option<NativeAsk>,
    /// A newly shown run wants the keyboard on its first box, taken once
    /// the panel is placed (see [`take_focus_pending`]).
    focus_pending: bool,
    /// While the boxes are filled: their changes are not the user's.
    filling: bool,
    /// Where the `$n` labels, the help and the preview are painted.
    labels: Vec<RECT>,
    help_at: RECT,
    preview_at: RECT,
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
        let class = w!("FeroSSHScriptArgs");
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
        let mut boxes = [HWND::default(); BOXES];
        for (i, b) in boxes.iter_mut().enumerate() {
            *b = child(
                WC_EDITW,
                w!(""),
                WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                ID_BOX + i,
            )?;
            // As much as the view keeps.
            SendMessageW(
                *b,
                windows::Win32::UI::Controls::EM_LIMITTEXT,
                Some(WPARAM(crate::snippets_view::MAX_TEXT)),
                None,
            );
        }
        let run = child(
            WC_BUTTONW,
            w!("Run"),
            WS_VISIBLE.0 | BS_DEFPUSHBUTTON as u32,
            ID_RUN,
        )?;
        let cancel = child(
            WC_BUTTONW,
            w!("Cancel"),
            WS_VISIBLE.0 | BS_PUSHBUTTON as u32,
            ID_CANCEL,
        )?;
        let h = Handles {
            main,
            panel,
            boxes,
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
                focus_pending: false,
                filling: false,
                labels: Vec::new(),
                help_at: RECT::default(),
                preview_at: RECT::default(),
                placed: None,
            });
        });
    }
    if let Some(h) = handles() {
        pass_edge(h.panel);
    }
    Ok(())
}

fn controls(h: &Handles) -> Vec<HWND> {
    let mut v = h.boxes.to_vec();
    v.extend([h.run, h.cancel]);
    v
}

/// The arguments shown.
fn count() -> usize {
    with(|s| s.shown.as_ref().map_or(0, |a| a.args.len()))
        .unwrap_or(0)
        .min(BOXES)
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
    let n = count();
    let pad = scale(8, dpi);
    let title = scale(36, dpi);
    let field = scale(26, dpi);
    let button = scale(28, dpi);
    let gap = scale(6, dpi);
    let line = scale(20, dpi);
    let inner = (width - 2 * pad).max(0);
    let label_w = scale(36, dpi).min(inner / 4);
    let help_h = 2 * line;
    let mut y = title + help_h;
    let mut labels = Vec::new();
    let mut rows = [0; BOXES];
    for row in rows.iter_mut().take(n) {
        labels.push(RECT {
            left: pad,
            top: y,
            right: pad + label_w,
            bottom: y + field,
        });
        *row = y;
        y += field + gap;
    }
    let preview_y = y + gap;
    let buttons_y = (at.h - pad - button).max(preview_y + 2 * line);
    let button_w = scale(72, dpi);
    with(|s| {
        s.labels = labels;
        s.help_at = RECT {
            left: pad,
            top: title,
            right: pad + inner,
            bottom: title + help_h,
        };
        s.preview_at = RECT {
            left: pad,
            top: preview_y,
            right: pad + inner,
            bottom: buttons_y - gap,
        };
    });
    // SAFETY: moving our own windows.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
        for (i, (b, row)) in h.boxes.iter().zip(rows).enumerate() {
            let _ = MoveWindow(
                *b,
                pad + label_w + gap,
                row,
                (inner - label_w - gap).max(0),
                field,
                true,
            );
            let _ = ShowWindow(*b, if i < n { SW_SHOWNA } else { SW_HIDE });
        }
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

/// Gives the keyboard to the first box.
pub fn focus_first() {
    if let Some(h) = handles() {
        focus_box(&h, 0);
    }
}

/// Whether a run just opened the page and its first box should take the
/// keyboard; clears the flag, so it fires only once per run.
pub fn take_focus_pending() -> bool {
    with(|s| std::mem::take(&mut s.focus_pending)).unwrap_or(false)
}

fn focus_box(h: &Handles, i: usize) {
    let Some(b) = h.boxes.get(i) else {
        return;
    };
    // SAFETY: our own control.
    unsafe {
        let _ = SetFocus(Some(*b));
        SendMessageW(*b, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
}

/// Shows `ask`: another run fills the boxes, the same one only gets its
/// preview.
pub fn set_ask(ask: NativeAsk) {
    let Some(h) = handles() else {
        return;
    };
    let before = with(|s| s.shown.replace(ask.clone())).flatten();
    let refill = before.as_ref().is_none_or(|b| b.serial != ask.serial);
    if refill {
        // A run just opened this page: put the keyboard on the first
        // argument once it is placed.
        with(|s| {
            s.focus_pending = true;
            s.filling = true;
        });
        // SAFETY: our own controls; each text lives through its call.
        unsafe {
            for (i, b) in h.boxes.iter().enumerate() {
                let text = wide(ask.args.get(i).map_or("", |(_, v)| v.as_str()));
                let _ = SetWindowTextW(*b, PCWSTR(text.as_ptr()));
            }
        }
        with(|s| s.filling = false);
        if let Some((Some((at, width)), dpi)) = with(|s| (s.placed, s.dpi)) {
            place(Some(at), width, dpi);
        }
    }
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
}

/// Pastes `text` into the box that has the keyboard, as one line;
/// `false` when no box has it.
pub fn paste(text: &str) -> bool {
    let Some(h) = handles() else {
        return false;
    };
    // SAFETY: plain queries and a message to our own control; the text
    // lives through the call.
    unsafe {
        let f = GetFocus();
        if !h.boxes.contains(&f) {
            return false;
        }
        let text = wide(&text.lines().collect::<Vec<_>>().join(" "));
        SendMessageW(
            f,
            windows::Win32::UI::Controls::EM_REPLACESEL,
            Some(WPARAM(1)),
            Some(LPARAM(text.as_ptr() as isize)),
        );
    }
    true
}

fn emit(e: Event) {
    let Some((main, handler)) = with(|s| (s.h.main, s.handler)) else {
        return;
    };
    handler(main, e);
}

fn values(h: &Handles) -> Vec<String> {
    h.boxes
        .iter()
        .take(count())
        .map(|b| search_text(*b))
        .collect()
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
        let at = h.boxes.iter().position(|b| *b == msg.hwnd);
        let n = count();
        match msg.message {
            WM_KEYDOWN if key_down(VK_CONTROL.0) && key_down(VK_SHIFT.0) => {
                let mut m = *msg;
                m.hwnd = h.main;
                let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&m);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&m);
                return true;
            }
            // Enter goes to the next box; on the last, or with Shift or
            // Ctrl, the script runs.
            WM_KEYDOWN if msg.wParam.0 == usize::from(VK_RETURN.0) && at.is_some() => {
                let i = at.unwrap_or(0);
                if key_down(VK_SHIFT.0) || key_down(VK_CONTROL.0) || i + 1 >= n {
                    emit(Event::Run(values(&h)));
                } else {
                    focus_box(&h, i + 1);
                }
                return true;
            }
            WM_KEYDOWN if msg.wParam.0 == usize::from(VK_DOWN.0) && at.is_some() => {
                focus_box(&h, (at.unwrap_or(0) + 1) % n.max(1));
                return true;
            }
            WM_KEYDOWN if msg.wParam.0 == usize::from(VK_UP.0) && at.is_some() => {
                focus_box(
                    &h,
                    at.unwrap_or(0)
                        .checked_sub(1)
                        .unwrap_or(n.saturating_sub(1)),
                );
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
        (id, _) if id == IDOK.0 as usize => emit(Event::Run(values(&h))),
        (id, _) if id == IDCANCEL.0 as usize => emit(Event::Cancel),
        (ID_RUN, BN_CLICKED) => emit(Event::Run(values(&h))),
        (ID_CANCEL, BN_CLICKED) => emit(Event::Cancel),
        (id, EN_CHANGE)
            if (ID_BOX..ID_BOX + BOXES).contains(&id) && with(|s| s.filling) == Some(false) =>
        {
            emit(Event::Values(values(&h)));
        }
        _ => {}
    }
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, ask, labels, help_at, preview_at)) = with(|s| {
        (
            s.title_font,
            s.font,
            s.dpi,
            s.shown.clone(),
            s.labels.clone(),
            s.help_at,
            s.preview_at,
        )
    }) else {
        return;
    };
    let Some(ask) = ask else {
        return;
    };
    let title = match ask.hosts {
        0 => format!("Run {}", ask.name),
        1 => format!("Run {} on 1 host", ask.name),
        n => format!("Run {} on {n} hosts", ask.name),
    };
    let help = if ask.enter || ask.hosts > 0 {
        "Enter goes to the next box and runs the script from the last; \
         Shift+Enter runs it at once. Escape cancels."
    } else {
        "Enter goes to the next box and inserts the script from the last, \
         without its last Enter; Shift+Enter inserts it at once. Escape cancels."
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
        let line = DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        let mut t: Vec<u16> = title.encode_utf16().collect();
        DrawTextW(dc, &mut t, &mut tr, line | DT_END_ELLIPSIS);
        SelectObject(dc, HGDIOBJ(font.0));
        for ((number, _), at) in ask.args.iter().zip(labels) {
            let mut l: Vec<u16> = format!("${number}").encode_utf16().collect();
            let mut at = at;
            DrawTextW(dc, &mut l, &mut at, line);
        }
        SetTextColor(dc, grey);
        let mut help: Vec<u16> = help.encode_utf16().collect();
        let mut at = help_at;
        DrawTextW(dc, &mut help, &mut at, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
        // What will be typed, a line per step, as far as there is room.
        let row = scale(20, dpi);
        let mut y = preview_at.top;
        let heading = if ask.hosts > 0 {
            "Typed on each host:"
        } else {
            "Typed:"
        };
        for (i, text) in std::iter::once(heading.to_owned())
            .chain(ask.preview.iter().map(|p| format!("› {p}")))
            .enumerate()
        {
            if y + row > preview_at.bottom {
                break;
            }
            SetTextColor(dc, if i == 0 { grey } else { text_colour });
            let mut t: Vec<u16> = text.encode_utf16().collect();
            let mut at = RECT {
                top: y,
                bottom: y + row,
                ..preview_at
            };
            DrawTextW(dc, &mut t, &mut at, line | DT_END_ELLIPSIS);
            y += row;
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
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
