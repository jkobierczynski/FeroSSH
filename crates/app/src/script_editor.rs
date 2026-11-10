//! The script editor as native Windows controls: Name, Category, Hosts,
//! Folders, Hotkey and When fields, a box for the lines, and Save and
//! Cancel, on a panel over the uncovered part of the tools pane.
//!
//! The fields come from `SnippetsView::native_draft` and go back with
//! Save, which the view checks as the drawn editor did: a refusal shows
//! why under the lines and puts the keyboard in the field at fault.
//!
//! In the lines box Enter ends a line, which is typed with Enter after
//! it; Shift+Enter ends a line typed without Enter (it becomes an `@type`
//! line). Enter in a field or Ctrl+Enter saves, Escape cancels. [`pre_translate`] runs
//! in the message loop like the other native tools' and passes Ctrl+Shift
//! shortcuts on to the main window.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint,
    FIXED_PITCH, FillRect, GetObjectW, HFONT, HGDIOBJ, InvalidateRect, LOGFONTW, PAINTSTRUCT,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    EM_GETSEL, EM_LINEFROMCHAR, EM_LINEINDEX, EM_LINELENGTH, EM_REPLACESEL, EM_SCROLLCARET,
    EM_SETCUEBANNER, EM_SETSEL, WC_BUTTONW, WC_COMBOBOXW, WC_EDITW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_GETDROPPEDSTATE,
    CB_SETCURSEL, CBS_DROPDOWNLIST, CBS_HASSTRINGS, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
    ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN, GetClientRect, HMENU, IDC_ARROW,
    IDCANCEL, IDOK, IsChild, IsDialogMessageW, IsWindowVisible, LoadCursorW, MSG, MoveWindow,
    RegisterClassW, SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN, WM_PAINT, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_HSCROLL, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};
use crate::snippets_view::{HEAD, HEAD_HINTS, HEAD_LABELS, NativeDraft, without_enter};

/// What the user did in the editor.
#[derive(Debug)]
pub enum Event {
    /// Save, with what the fields and the lines box say.
    Save { head: [String; HEAD], lines: String },
    /// Cancel or Escape.
    Cancel,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

const ID_FIELD: usize = 500;
const ID_LINES: usize = 510;
const ID_SAVE: usize = 511;
const ID_CANCEL: usize = 512;

/// The head field that is the hotkey: a drop-down, not a box.
const HOTKEY: usize = 4;

/// What the lines box says how to use.
const LINES_HELP: &str = "Lines: Enter ends a line typed with Enter, Shift+Enter one typed \
                          without. $1 or ${1:default} asks; @expect, @key and others are steps.";

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    /// Name, Category, Hosts, Folders, Hotkey (a drop-down), When.
    fields: [HWND; HEAD],
    lines: HWND,
    save: HWND,
    cancel: HWND,
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    /// A fixed-width font for the lines.
    mono: HFONT,
    dpi: u32,
    /// The draft shown, to fill the fields only for another one.
    shown: Option<NativeDraft>,
    /// Where the labels, the note, the help and the error are painted.
    labels: [RECT; HEAD],
    note_at: RECT,
    help_at: RECT,
    error_at: RECT,
    /// Where the panel was last placed, and the width it was laid out
    /// for, to lay it out again when the note comes or goes.
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

/// A fixed-width font as tall as `like`.
fn mono_font(like: HFONT) -> HFONT {
    let mut lf = LOGFONTW::default();
    // SAFETY: a correctly sized struct for the call to fill; the new font
    // is deleted when replaced.
    unsafe {
        GetObjectW(
            HGDIOBJ(like.0),
            size_of::<LOGFONTW>() as i32,
            Some((&raw mut lf).cast()),
        );
        lf.lfPitchAndFamily = FIXED_PITCH.0;
        lf.lfFaceName = [0; 32];
        for (d, s) in lf.lfFaceName.iter_mut().zip("Consolas".encode_utf16()) {
            *d = s;
        }
        CreateFontIndirectW(&lf)
    }
}

/// Creates the panel, hidden, as a child of `main`.
pub fn create(main: HWND, handler: Handler, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering a class and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHScriptEditor");
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
        let mut fields = [HWND::default(); HEAD];
        for (i, f) in fields.iter_mut().enumerate() {
            if i == HOTKEY {
                let combo = WS_VSCROLL.0 | (CBS_DROPDOWNLIST | CBS_HASSTRINGS) as u32;
                *f = child(WC_COMBOBOXW, w!(""), combo, ID_FIELD + i)?;
                for n in 0..=12 {
                    let text = wide(&hotkey_text(n));
                    SendMessageW(*f, CB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
                }
                continue;
            }
            *f = child(
                WC_EDITW,
                w!(""),
                WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                ID_FIELD + i,
            )?;
            let cue = wide(HEAD_HINTS.get(i).copied().unwrap_or_default());
            SendMessageW(
                *f,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(cue.as_ptr() as isize)),
            );
        }
        let lines = child(
            WC_EDITW,
            w!(""),
            WS_BORDER.0
                | WS_VSCROLL.0
                | WS_HSCROLL.0
                | (ES_MULTILINE | ES_AUTOVSCROLL | ES_AUTOHSCROLL | ES_WANTRETURN) as u32,
            ID_LINES,
        )?;
        let save = child(WC_BUTTONW, w!("Save"), BS_DEFPUSHBUTTON as u32, ID_SAVE)?;
        let cancel = child(WC_BUTTONW, w!("Cancel"), BS_PUSHBUTTON as u32, ID_CANCEL)?;
        let h = Handles {
            main,
            panel,
            fields,
            lines,
            save,
            cancel,
        };
        let (font, title_font) = fonts(dpi);
        let mono = mono_font(font);
        for c in controls(&h) {
            set_font(c, font);
        }
        set_font(lines, mono);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                h,
                handler,
                font,
                title_font,
                mono,
                dpi,
                shown: None,
                labels: [RECT::default(); HEAD],
                note_at: RECT::default(),
                help_at: RECT::default(),
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

/// The hotkey drop-down's entry `n`: none, then F1 to F12.
fn hotkey_text(n: u8) -> String {
    if n == 0 {
        "None".to_owned()
    } else {
        format!("Ctrl+Shift+F{n}")
    }
}

/// The controls in the font of the panel (the lines box has its own).
fn controls(h: &Handles) -> Vec<HWND> {
    let mut v = h.fields.to_vec();
    v.extend([h.save, h.cancel]);
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
        let mono = mono_font(font);
        let old = [
            std::mem::replace(&mut s.font, font),
            std::mem::replace(&mut s.title_font, title),
            std::mem::replace(&mut s.mono, mono),
        ];
        Some((font, mono, old))
    })
    .flatten();
    if let Some((font, mono, old)) = refont {
        for c in controls(&h) {
            set_font(c, font);
        }
        set_font(h.lines, mono);
        // SAFETY: no control uses the old fonts any more.
        unsafe {
            for f in old {
                let _ = DeleteObject(HGDIOBJ(f.0));
            }
        }
    }
    let has_note = with(|s| s.shown.as_ref().is_some_and(|d| d.note.is_some())).unwrap_or(false);
    let pad = scale(8, dpi);
    let title = scale(36, dpi);
    let field = scale(26, dpi);
    let button = scale(28, dpi);
    let gap = scale(6, dpi);
    let line = scale(20, dpi);
    let inner = (width - 2 * pad).max(0);
    let label_w = scale(72, dpi).min(inner / 3);
    let field_w = (inner - label_w - gap).max(0);
    let note_h = if has_note { 2 * line } else { 0 };
    let mut y = title + note_h;
    let mut labels = [RECT::default(); HEAD];
    let mut rows = [0; HEAD];
    for (label, row) in labels.iter_mut().zip(rows.iter_mut()) {
        *label = RECT {
            left: pad,
            top: y,
            right: pad + label_w,
            bottom: y + field,
        };
        *row = y;
        y += field + gap;
    }
    let help_h = 2 * line;
    let help_y = y;
    let lines_y = help_y + help_h;
    let buttons_y = (at.h - pad - button).max(lines_y + 3 * field);
    let error_y = buttons_y - gap - 2 * line;
    let lines_h = (error_y - gap - lines_y).max(field);
    let button_w = scale(72, dpi);
    with(|s| {
        s.labels = labels;
        s.note_at = RECT {
            left: pad,
            top: title,
            right: pad + inner,
            bottom: title + note_h,
        };
        s.help_at = RECT {
            left: pad,
            top: help_y,
            right: pad + inner,
            bottom: help_y + help_h,
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
        for (i, (f, row)) in h.fields.iter().zip(rows).enumerate() {
            // A combo box's height includes its drop-down list.
            let height = if i == HOTKEY {
                field + scale(240, dpi)
            } else {
                field
            };
            let _ = MoveWindow(*f, pad + label_w + gap, row, field_w, height, true);
        }
        let _ = MoveWindow(h.lines, pad, lines_y, inner, lines_h, true);
        let _ = MoveWindow(h.save, pad, buttons_y, button_w, button, true);
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

/// Gives the keyboard to the field the draft says: the name for a new
/// script, the lines for one that has a name.
pub fn focus_first() {
    let Some(h) = handles() else {
        return;
    };
    let focus = with(|s| s.shown.as_ref().map(|d| d.focus))
        .flatten()
        .unwrap_or((0, 0));
    focus_field(&h, focus);
}

fn focus_field(h: &Handles, (field, line): (usize, usize)) {
    // SAFETY: our own controls.
    unsafe {
        match h.fields.get(field) {
            Some(f) => {
                let _ = SetFocus(Some(*f));
                if field != HOTKEY {
                    SendMessageW(*f, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                }
            }
            None => {
                let _ = SetFocus(Some(h.lines));
                // The start of the line at fault.
                let at = SendMessageW(h.lines, EM_LINEINDEX, Some(WPARAM(line)), None).0;
                let at = usize::try_from(at).unwrap_or(0);
                SendMessageW(
                    h.lines,
                    EM_SETSEL,
                    Some(WPARAM(at)),
                    Some(LPARAM(at as isize)),
                );
                SendMessageW(h.lines, EM_SCROLLCARET, None, None);
            }
        }
    }
}

/// Shows `draft`: another draft fills the fields, the same one only gets
/// its error or note (and the keyboard goes to the field at fault).
pub fn set_draft(draft: NativeDraft) {
    let Some(h) = handles() else {
        return;
    };
    let before = with(|s| s.shown.replace(draft.clone())).flatten();
    let refill = before.as_ref().is_none_or(|b| b.serial != draft.serial);
    let note_changed = before.as_ref().map(|b| b.note.is_some()) != Some(draft.note.is_some());
    if refill {
        // SAFETY: our own controls; each text lives through its call.
        unsafe {
            for (i, f) in h.fields.iter().enumerate() {
                let text = draft.head.get(i).cloned().unwrap_or_default();
                if i == HOTKEY {
                    let n = ferossh_config::login::parse_hotkey(&text)
                        .ok()
                        .flatten()
                        .unwrap_or(0);
                    SendMessageW(*f, CB_SETCURSEL, Some(WPARAM(usize::from(n))), None);
                    continue;
                }
                let text = wide(&text);
                let _ = SetWindowTextW(*f, PCWSTR(text.as_ptr()));
            }
            let text = wide(&draft.lines.join("\r\n"));
            let _ = SetWindowTextW(h.lines, PCWSTR(text.as_ptr()));
        }
    } else if draft.error.is_some() && before.as_ref().map(|b| &b.error) != Some(&draft.error) {
        focus_field(&h, draft.focus);
    }
    if note_changed && let Some((Some((at, width)), dpi)) = with(|s| (s.placed, s.dpi)) {
        place(Some(at), width, dpi);
    }
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(h.panel), None, false);
    }
}

/// Pastes `text` into the field that has the keyboard (the window's
/// paste shortcut); `false` when no field of the editor has it.
pub fn paste(text: &str) -> bool {
    let Some(h) = handles() else {
        return false;
    };
    // SAFETY: plain queries and a message to our own control; the text
    // lives through the call.
    unsafe {
        let f = GetFocus();
        let text = if f == h.lines {
            text.replace("\r\n", "\n").replace('\n', "\r\n")
        } else if h
            .fields
            .iter()
            .enumerate()
            .any(|(i, x)| *x == f && i != HOTKEY)
        {
            text.lines().collect::<Vec<_>>().join(" ")
        } else {
            return false;
        };
        let text = wide(&text);
        SendMessageW(
            f,
            EM_REPLACESEL,
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

fn save(h: &Handles) {
    let mut head: [String; HEAD] = Default::default();
    for (i, (t, f)) in head.iter_mut().zip(h.fields).enumerate() {
        if i == HOTKEY {
            // SAFETY: a plain message to our own control.
            let n = unsafe { SendMessageW(f, CB_GETCURSEL, None, None) }.0;
            *t = match n {
                1..=12 => format!("F{n}"),
                _ => String::new(),
            };
        } else {
            *t = search_text(f);
        }
    }
    emit(Event::Save {
        head,
        lines: search_text(h.lines),
    });
}

/// Shift+Enter in the lines box: the line at the caret becomes one typed
/// without Enter, and a new line starts after it.
fn line_without_enter(lines: HWND) {
    // SAFETY: plain messages to our own control; each text lives through
    // its call.
    unsafe {
        let mut start = 0u32;
        SendMessageW(
            lines,
            EM_GETSEL,
            Some(WPARAM(&raw mut start as usize)),
            None,
        );
        let row = SendMessageW(lines, EM_LINEFROMCHAR, Some(WPARAM(start as usize)), None).0;
        let first = SendMessageW(lines, EM_LINEINDEX, Some(WPARAM(row as usize)), None).0;
        let len = SendMessageW(
            lines,
            EM_LINELENGTH,
            Some(WPARAM(first.max(0) as usize)),
            None,
        )
        .0;
        let all = search_text(lines);
        let utf16: Vec<u16> = all.encode_utf16().collect();
        let (first, len) = (first.max(0) as usize, len.max(0) as usize);
        let line = String::from_utf16_lossy(utf16.get(first..first + len).unwrap_or_default());
        let mut new = if line.trim().is_empty() {
            line.clone()
        } else {
            without_enter(&line)
        };
        new.push_str("\r\n");
        SendMessageW(
            lines,
            EM_SETSEL,
            Some(WPARAM(first)),
            Some(LPARAM((first + len) as isize)),
        );
        let text = wide(&new);
        SendMessageW(
            lines,
            EM_REPLACESEL,
            Some(WPARAM(1)),
            Some(LPARAM(text.as_ptr() as isize)),
        );
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
        // Escape and Enter close the hotkey's open drop-down list.
        let dropped = h.fields.get(HOTKEY).is_some_and(|c| {
            (msg.hwnd == *c || IsChild(*c, msg.hwnd).as_bool())
                && SendMessageW(*c, CB_GETDROPPEDSTATE, None, None).0 != 0
        });
        if dropped && msg.message == WM_KEYDOWN {
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
            WM_KEYDOWN
                if msg.hwnd == h.lines
                    && msg.wParam.0 == usize::from(VK_RETURN.0)
                    && key_down(VK_SHIFT.0) =>
            {
                line_without_enter(h.lines);
                return true;
            }
            // Ctrl+Enter saves from the lines box too.
            WM_KEYDOWN
                if msg.hwnd == h.lines
                    && msg.wParam.0 == usize::from(VK_RETURN.0)
                    && key_down(VK_CONTROL.0) =>
            {
                save(&h);
                return true;
            }
            // Escape cancels, from the lines box too (a multi-line box
            // would close its parent).
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
        (id, _) if id == IDOK.0 as usize => save(&h),
        (id, _) if id == IDCANCEL.0 as usize => emit(Event::Cancel),
        (ID_SAVE, BN_CLICKED) => save(&h),
        (ID_CANCEL, BN_CLICKED) => emit(Event::Cancel),
        _ => {}
    }
}

fn paint(h: Handles) {
    let Some((title_font, font, dpi, draft, labels, note_at, help_at, error_at)) = with(|s| {
        (
            s.title_font,
            s.font,
            s.dpi,
            s.shown.clone(),
            s.labels,
            s.note_at,
            s.help_at,
            s.error_at,
        )
    }) else {
        return;
    };
    let title = if draft.as_ref().is_some_and(|d| d.existing) {
        "Edit script"
    } else {
        "New script"
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
        let line = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        let mut t: Vec<u16> = title.encode_utf16().collect();
        DrawTextW(dc, &mut t, &mut tr, DT_LEFT | line);
        SelectObject(dc, HGDIOBJ(font.0));
        for (label, at) in HEAD_LABELS.iter().zip(labels) {
            let mut l: Vec<u16> = label.encode_utf16().collect();
            let mut at = at;
            DrawTextW(dc, &mut l, &mut at, DT_LEFT | line);
        }
        SetTextColor(dc, grey);
        let block = DT_LEFT | DT_WORDBREAK | DT_NOPREFIX;
        if let Some(note) = draft.as_ref().and_then(|d| d.note.clone()) {
            let mut n: Vec<u16> = note.encode_utf16().collect();
            let mut at = note_at;
            DrawTextW(dc, &mut n, &mut at, block);
        }
        let mut help: Vec<u16> = LINES_HELP.encode_utf16().collect();
        let mut at = help_at;
        DrawTextW(dc, &mut help, &mut at, block);
        if let Some(e) = draft.and_then(|d| d.error) {
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
        // What a multi-line box sends on Escape: the panel stays.
        (WM_CLOSE, Some(_)) => {
            emit(Event::Cancel);
            LRESULT(0)
        }
        (WM_COMMAND, Some(h)) => {
            command(h, wparam.0 & 0xFFFF, ((wparam.0 >> 16) & 0xFFFF) as u32);
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
