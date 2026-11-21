//! Theme manager > Edit colours: a small window that lists a theme's
//! colours (foreground, background, cursor, cursor text and the 16 ANSI
//! colours) with a swatch and its `#RRGGBB` each, and a preview. A click
//! on a swatch opens the Windows colour picker; a colour can be typed
//! too. Save writes one of the user's own themes back to its file; Save
//! as asks for a new one (a built-in theme only has Save as, so it is
//! never changed). [`edit`] runs it modally over the main window.

#![allow(unsafe_code)]

use std::cell::RefCell;

use ferossh_render::{Rgb, Theme};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW, CreateSolidBrush, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawFocusRect, DrawTextW, EndPaint,
    FW_NORMAL, FillRect, FrameRect, HDC, HFONT, HGDIOBJ, InvalidateRect, LOGFONTW, PAINTSTRUCT,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::Dialogs::{CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_LIMITTEXT, EM_SETSEL, ODS_FOCUS, WC_BUTTONW, WC_EDITW, WC_STATICW,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_OWNERDRAW, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW,
    CreateWindowExW, DC_HASDEFID, DM_GETDEFID, DestroyWindow, DispatchMessageW, EN_CHANGE,
    EN_KILLFOCUS, ES_AUTOHSCROLL, GW_CHILD, GW_HWNDNEXT, GetClientRect, GetMessageW, GetWindow,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW, IDCANCEL, IDOK,
    IsDialogMessageW, LoadCursorW, MSG, PostQuitMessage, RegisterClassW, SW_SHOW, SendMessageW,
    SetForegroundWindow, SetWindowTextW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DRAWITEM, WM_ERASEBKGND, WM_PAINT, WNDCLASSW, WS_BORDER,
    WS_CAPTION, WS_CHILD, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::colour_edit::{self as edit_view, NAMES, SLOTS};
use crate::native::{fonts, scale, set_font, wide};

/// Save as's button id (Save is `IDOK`).
const ID_SAVE_AS: usize = 10;
/// Each colour's swatch button: this plus its slot.
const ID_SWATCH: usize = 300;
/// Each colour's text field: this plus its slot.
const ID_HEX: usize = 400;
const SS_NOPREFIX: u32 = 0x80;
const WIDTH: i32 = 560;
const INTRO_TOP: i32 = 12;
const INTRO_H: i32 = 36;
const PREVIEW_TOP: i32 = INTRO_TOP + INTRO_H + 8;
const PREVIEW_H: i32 = 76;
const GRID_TOP: i32 = PREVIEW_TOP + PREVIEW_H + 12;
const ROW_H: i32 = 30;
/// The grid's rows: two of the four plain colours, then each normal
/// ANSI colour beside its bright one.
const ROWS: i32 = 2 + 8;
const CELL_W: i32 = (WIDTH - 24) / 2;
const HEIGHT: i32 = GRID_TOP + ROWS * ROW_H + 8 + 20 + 8 + 28 + 12;

/// How the edited colours are to be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Written back to the theme's own file.
    Save,
    /// Written to a new theme file.
    SaveAs,
}

struct State {
    theme: Theme,
    original: Theme,
    swatches: Vec<HWND>,
    hexes: Vec<HWND>,
    status: HWND,
    mono: HFONT,
    dpi: u32,
    /// The text fields are being set from the colours, not typed in.
    setting: bool,
    /// Save is offered (else only Save as, the default button then).
    can_save: bool,
    /// The colour picker's sixteen custom colours, kept while it is open.
    custom: [COLORREF; 16],
    done: Option<Option<(Theme, Choice)>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

fn colorref(c: Rgb) -> COLORREF {
    COLORREF(u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16))
}

fn rgb(c: COLORREF) -> Rgb {
    Rgb::new(
        (c.0 & 0xFF) as u8,
        ((c.0 >> 8) & 0xFF) as u8,
        ((c.0 >> 16) & 0xFF) as u8,
    )
}

/// The grid cell of colour `slot`: its column and row.
fn cell(slot: usize) -> (i32, i32) {
    let s = i32::try_from(slot).unwrap_or(0);
    match slot {
        0..=3 => (s % 2, s / 2),
        4..=11 => (0, 2 + s - 4),
        _ => (1, 2 + s - 12),
    }
}

/// Shows the editor for theme `original` over `owner`, starting with the
/// colours of `theme` (edits kept from an earlier try), and waits: the
/// edited theme and how to keep it, or `None` for Cancel. `can_save`
/// offers Save (one of the user's own themes); else only Save as.
pub fn edit(
    owner: HWND,
    original: &Theme,
    theme: &Theme,
    can_save: bool,
) -> Option<(Theme, Choice)> {
    // SAFETY: plain query of our own window: the editor takes the DPI of
    // the monitor it opens on.
    let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
    // SAFETY: a class with valid static strings and a window procedure
    // of ours; the window and its controls are destroyed before return.
    let dlg = unsafe { create(owner, dpi, original, theme, can_save) }.ok()?;
    let (font, title) = fonts(dpi);
    // SAFETY: the title font is not used here.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(title.0));
    }
    set_fonts(dlg, font);
    crate::ui_theme::restyle(dlg);
    crate::ui_theme::popup_frame(dlg);
    // SAFETY: a message box's modal loop; see fields_view::ask.
    let answer = unsafe {
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        if let Some(Some(first)) = with(|s| s.swatches.first().copied()) {
            let _ = SetFocus(Some(first));
        }
        let mut msg = MSG::default();
        loop {
            if let Some(done) = with(|s| s.done.take()).flatten() {
                break done;
            }
            let got = GetMessageW(&mut msg, None, 0, 0);
            if got.0 == 0 {
                PostQuitMessage(msg.wParam.0 as i32);
                break None;
            }
            if got.0 == -1 {
                break None;
            }
            if !IsDialogMessageW(dlg, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    };
    let mono = with(|s| s.mono);
    // SAFETY: the owner first, so it gets the activation back; the fonts
    // are ours and no longer selected anywhere.
    unsafe {
        let _ = EnableWindow(owner, true);
        let _ = SetForegroundWindow(owner);
        let _ = DestroyWindow(dlg);
        let _ = DeleteObject(HGDIOBJ(font.0));
        if let Some(mono) = mono {
            let _ = DeleteObject(HGDIOBJ(mono.0));
        }
    }
    STATE.with(|s| s.borrow_mut().take());
    answer
}

fn set_fonts(dlg: HWND, font: HFONT) {
    set_font(dlg, font);
    // SAFETY: walking our own window's children.
    unsafe {
        let mut c = GetWindow(dlg, GW_CHILD).ok();
        while let Some(h) = c {
            set_font(h, font);
            c = GetWindow(h, GW_HWNDNEXT).ok();
        }
    }
}

/// A fixed-width font for the preview.
fn mono_font(dpi: u32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -scale(13, dpi),
        lfWeight: FW_NORMAL.0 as i32,
        ..Default::default()
    };
    for (slot, c) in lf.lfFaceName.iter_mut().zip("Consolas".encode_utf16()) {
        *slot = c;
    }
    // SAFETY: a filled-in LOGFONTW; the caller deletes the font.
    unsafe { CreateFontIndirectW(&lf) }
}

unsafe fn create(
    owner: HWND,
    dpi: u32,
    original: &Theme,
    theme: &Theme,
    can_save: bool,
) -> windows::core::Result<HWND> {
    // SAFETY: the caller's promise; see `edit`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHColours");
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
        let top = o.top + ((o.bottom - o.top) - h) / 3;
        // Kept on the owner's monitor's work area, the buttons with it
        // (the top first when it is taller than the area).
        let (x, top) = {
            use windows::Win32::Graphics::Gdi::{
                GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
            };
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let mon = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let a = mi.rcWork;
                (
                    x.min(a.right - w).max(a.left),
                    top.min(a.bottom - h).max(a.top),
                )
            } else {
                (x, top)
            }
        };
        let title = wide(&format!("Edit colours: {}", original.name));
        let dlg = CreateWindowExW(
            ex,
            class,
            PCWSTR(title.as_ptr()),
            style,
            x,
            top,
            w,
            h,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )?;
        let built = build(dlg, instance.into(), dpi, original, theme, can_save);
        if built.is_err() {
            // Its controls go with it.
            let _ = DestroyWindow(dlg);
        }
        built.map(|()| dlg)
    }
}

/// The editor's controls, in window `dlg`.
unsafe fn build(
    dlg: HWND,
    instance: windows::Win32::Foundation::HINSTANCE,
    dpi: u32,
    original: &Theme,
    theme: &Theme,
    can_save: bool,
) -> windows::core::Result<()> {
    let s = |d: i32| scale(d, dpi);
    // SAFETY: the caller's promise; see `edit`.
    unsafe {
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
                Some(instance),
                None,
            )
        };
        let intro = if can_save {
            "Click a colour to pick another, or type it as #RRGGBB. Save changes this theme's \
             file; Save as keeps it and makes a new theme."
        } else {
            "Click a colour to pick another, or type it as #RRGGBB. A built-in theme stays as \
             it is: Save as makes a new theme with your colours."
        };
        child(
            WC_STATICW,
            intro,
            SS_NOPREFIX,
            0,
            (12, INTRO_TOP, WIDTH - 24, INTRO_H),
        )?;
        let mut swatches = Vec::with_capacity(SLOTS);
        let mut hexes = Vec::with_capacity(SLOTS);
        for (slot, name) in NAMES.iter().enumerate() {
            let (col, row) = cell(slot);
            let x = 12 + col * CELL_W;
            let y = GRID_TOP + row * ROW_H;
            child(WC_STATICW, name, SS_NOPREFIX, 0, (x, y + 4, 112, 20))?;
            let b = child(
                WC_BUTTONW,
                name,
                WS_TABSTOP.0 | BS_OWNERDRAW as u32,
                ID_SWATCH + slot,
                (x + 116, y, 44, 24),
            )?;
            let c = edit_view::get(theme, slot).unwrap_or_default();
            let e = child(
                WC_EDITW,
                &edit_view::hex(c),
                WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                ID_HEX + slot,
                (x + 168, y, 84, 24),
            )?;
            // Room for spaces around a pasted colour, which `parse` ignores.
            SendMessageW(e, EM_LIMITTEXT, Some(WPARAM(16)), None);
            swatches.push(b);
            hexes.push(e);
        }
        let status_top = GRID_TOP + ROWS * ROW_H + 8;
        let status = child(
            WC_STATICW,
            &summary(original, theme),
            SS_NOPREFIX,
            0,
            (12, status_top, WIDTH - 24, 20),
        )?;
        let bottom = HEIGHT - 12 - 28;
        let save = child(
            WC_BUTTONW,
            "&Save",
            WS_TABSTOP.0
                | if can_save {
                    BS_DEFPUSHBUTTON as u32
                } else {
                    BS_PUSHBUTTON as u32
                },
            IDOK.0 as usize,
            (WIDTH - 12 - 96 - 8 - 104 - 8 - 96, bottom, 96, 28),
        )?;
        if !can_save {
            let _ = EnableWindow(save, false);
        }
        child(
            WC_BUTTONW,
            "Save &as...",
            WS_TABSTOP.0
                | if can_save {
                    BS_PUSHBUTTON as u32
                } else {
                    BS_DEFPUSHBUTTON as u32
                },
            ID_SAVE_AS,
            (WIDTH - 12 - 96 - 8 - 104, bottom, 104, 28),
        )?;
        child(
            WC_BUTTONW,
            "Cancel",
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            IDCANCEL.0 as usize,
            (WIDTH - 12 - 96, bottom, 96, 28),
        )?;
        // The picker starts with the theme's ANSI colours as its own.
        let mut custom = [COLORREF(0); 16];
        for (slot, c) in custom.iter_mut().zip(theme.ansi.iter()) {
            *slot = colorref(*c);
        }
        STATE.with(|st| {
            *st.borrow_mut() = Some(State {
                theme: theme.clone(),
                original: original.clone(),
                swatches,
                hexes,
                status,
                mono: mono_font(dpi),
                dpi,
                setting: false,
                can_save,
                custom,
                done: None,
            });
        });
        Ok(())
    }
}

/// The line under the colours: which changed.
fn summary(was: &Theme, now: &Theme) -> String {
    let names = edit_view::changed(was, now);
    match names.len() {
        0 => "No colour changed yet.".to_owned(),
        1..=4 => format!("Changed: {}.", names.join(", ")),
        n => format!("{n} colours changed."),
    }
}

fn text_of(hwnd: HWND) -> String {
    // SAFETY: a buffer sized from the control's own length.
    unsafe {
        let n = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(buf.get(..got).unwrap_or_default())
    }
}

/// The preview strip's place, in the window's client pixels.
fn preview_rect(dpi: u32) -> RECT {
    RECT {
        left: scale(12, dpi),
        top: scale(PREVIEW_TOP, dpi),
        right: scale(WIDTH - 12, dpi),
        bottom: scale(PREVIEW_TOP + PREVIEW_H, dpi),
    }
}

/// Colour `slot` is now `c`: its swatch, its text (unless typed there),
/// the preview and the summary follow.
fn changed(dlg: HWND, slot: usize, c: Rgb, from_text: bool) {
    let Some((swatch, hex, status, summary, dpi)) = with(|s| {
        edit_view::set(&mut s.theme, slot, c);
        let summary = summary(&s.original, &s.theme);
        Some((
            *s.swatches.get(slot)?,
            *s.hexes.get(slot)?,
            s.status,
            summary,
            s.dpi,
        ))
    })
    .flatten() else {
        return;
    };
    if !from_text {
        with(|s| s.setting = true);
        let text = wide(&edit_view::hex(c));
        // SAFETY: our own control and a terminated string.
        unsafe {
            let _ = SetWindowTextW(hex, PCWSTR(text.as_ptr()));
        }
        with(|s| s.setting = false);
    }
    let text = wide(&summary);
    let preview = preview_rect(dpi);
    // SAFETY: our own controls and window.
    unsafe {
        let _ = SetWindowTextW(status, PCWSTR(text.as_ptr()));
        let _ = InvalidateRect(Some(swatch), None, true);
        let _ = InvalidateRect(Some(dlg), Some(&preview), false);
    }
}

/// The Windows colour picker for colour `slot`.
fn pick(dlg: HWND, slot: usize) {
    let Some((now, mut custom)) =
        with(|s| Some((edit_view::get(&s.theme, slot)?, s.custom))).flatten()
    else {
        return;
    };
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: dlg,
        rgbResult: colorref(now),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    // SAFETY: the struct is filled in as documented and `custom` outlives
    // the call; its modal loop holds no state borrow.
    let ok = unsafe { ChooseColorW(&mut cc) }.as_bool();
    with(|s| s.custom = custom);
    if ok {
        changed(dlg, slot, rgb(cc.rgbResult), false);
    }
}

/// Draws swatch button `di`: its colour, a frame, and the focus.
fn draw_swatch(di: &DRAWITEMSTRUCT) {
    let slot = (di.CtlID as usize).wrapping_sub(ID_SWATCH);
    let Some(c) = with(|s| edit_view::get(&s.theme, slot)).flatten() else {
        return;
    };
    // SAFETY: the DC and rectangle Windows gives for this draw; the
    // brushes are ours and deleted.
    unsafe {
        let dc = di.hDC;
        let rc = di.rcItem;
        FillRect(dc, &rc, crate::ui_theme::sys_brush(COLOR_WINDOW));
        let inner = RECT {
            left: rc.left + 3,
            top: rc.top + 3,
            right: rc.right - 3,
            bottom: rc.bottom - 3,
        };
        let brush = CreateSolidBrush(colorref(c));
        FillRect(dc, &inner, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        FrameRect(dc, &inner, crate::ui_theme::sys_brush(COLOR_WINDOWTEXT));
        if (di.itemState.0 & ODS_FOCUS.0) != 0 {
            let _ = DrawFocusRect(dc, &rc);
        }
    }
}

/// Draws the preview: a prompt, a listing in the ANSI colours and the
/// cursor, in the theme being edited.
fn paint(dlg: HWND) {
    let Some((theme, font, dpi)) = with(|s| (s.theme.clone(), s.mono, s.dpi)) else {
        return;
    };
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: a paint of our own window; the font is selected back out and
    // the brushes are deleted.
    unsafe {
        let dc = BeginPaint(dlg, &mut ps);
        let rc = preview_rect(dpi);
        let bg = CreateSolidBrush(colorref(theme.background));
        FillRect(dc, &rc, bg);
        let _ = DeleteObject(HGDIOBJ(bg.0));
        FrameRect(dc, &rc, crate::ui_theme::sys_brush(COLOR_WINDOWTEXT));
        let old = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        let line = scale(18, dpi);
        let pad = scale(8, dpi);
        let ansi = |i: usize| theme.ansi.get(i).copied().unwrap_or(theme.foreground);
        let lines: [Vec<(String, Rgb)>; 3] = [
            vec![
                ("user@host".into(), ansi(2)),
                (":".into(), theme.foreground),
                ("~/src".into(), ansi(4)),
                ("$ ls --color".into(), theme.foreground),
            ],
            [
                "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
            ]
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("{n} "), ansi(i)))
            .collect(),
            [
                "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
            ]
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("{n} "), ansi(i + 8)))
            .collect(),
        ];
        let mut prompt_end = rc.left + pad;
        for (n, parts) in lines.iter().enumerate() {
            let top = rc.top + pad / 2 + i32::try_from(n).unwrap_or(0) * line;
            let mut x = rc.left + pad;
            for (text, colour) in parts {
                let mut t: Vec<u16> = text.encode_utf16().collect();
                let mut at = RECT {
                    left: x,
                    top,
                    right: rc.right - pad,
                    bottom: top + line,
                };
                let flags = DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
                SetTextColor(dc, colorref(*colour));
                let mut size = at;
                DrawTextW(
                    dc,
                    &mut t,
                    &mut size,
                    flags | windows::Win32::Graphics::Gdi::DT_CALCRECT,
                );
                DrawTextW(dc, &mut t, &mut at, flags);
                x = size.right;
            }
            if n == 0 {
                prompt_end = x;
            }
        }
        // A block cursor after the prompt, with its text colour on it.
        let top = rc.top + pad / 2;
        let mut block = RECT {
            left: prompt_end + scale(6, dpi),
            top: top + scale(1, dpi),
            right: prompt_end + scale(6 + 8, dpi),
            bottom: top + line - scale(1, dpi),
        };
        let cursor = CreateSolidBrush(colorref(theme.cursor));
        FillRect(dc, &block, cursor);
        let _ = DeleteObject(HGDIOBJ(cursor.0));
        SetTextColor(dc, colorref(theme.cursor_text));
        let mut t: Vec<u16> = "x".encode_utf16().collect();
        DrawTextW(
            dc,
            &mut t,
            &mut block,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        let _ = EndPaint(dlg, &ps);
    }
}

fn finish(choice: Choice) {
    // A field still holding text that is no colour stops both saves:
    // the colour it shows would not be the one written.
    let bad = with(|s| {
        s.hexes
            .iter()
            .enumerate()
            .find(|(_, h)| edit_view::parse(&text_of(**h)).is_none())
            .map(|(i, h)| (i, *h, s.status))
    })
    .flatten();
    if let Some((slot, field, status)) = bad {
        let name = NAMES.get(slot).copied().unwrap_or("A colour");
        let text = wide(&format!(
            "{name} is not a colour: type #RRGGBB, or press Esc to cancel."
        ));
        // SAFETY: our own controls and a terminated string.
        unsafe {
            let _ = SetWindowTextW(status, PCWSTR(text.as_ptr()));
            let _ = SetFocus(Some(field));
            SendMessageW(field, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
        return;
    }
    with(|s| s.done = Some(Some((s.theme.clone(), choice))));
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            match id {
                // Enter sends IDOK too: Save as when there is no Save.
                x if x == IDOK.0 as usize => {
                    if with(|s| s.can_save).unwrap_or(false) {
                        finish(Choice::Save);
                    } else {
                        finish(Choice::SaveAs);
                    }
                }
                ID_SAVE_AS => finish(Choice::SaveAs),
                x if x == IDCANCEL.0 as usize => {
                    with(|s| s.done = Some(None));
                }
                x if (ID_SWATCH..ID_SWATCH + SLOTS).contains(&x) && code == BN_CLICKED => {
                    pick(hwnd, x - ID_SWATCH);
                }
                x if (ID_HEX..ID_HEX + SLOTS).contains(&x) => {
                    let slot = x - ID_HEX;
                    let field = HWND(lparam.0 as *mut _);
                    if code == EN_CHANGE && with(|s| !s.setting).unwrap_or(false) {
                        if let Some(c) = edit_view::parse(&text_of(field)) {
                            changed(hwnd, slot, c, true);
                        }
                    } else if code == EN_KILLFOCUS {
                        // A colour typed reads back in the usual form; text
                        // that is no colour stays, for Save to point at.
                        if let Some(c) = edit_view::parse(&text_of(field)) {
                            changed(hwnd, slot, c, false);
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        DM_GETDEFID => {
            // The default button, for Enter in IsDialogMessageW.
            let id = if with(|s| s.can_save).unwrap_or(false) {
                IDOK.0 as usize
            } else {
                ID_SAVE_AS
            };
            LRESULT(((DC_HASDEFID as usize) << 16 | id) as isize)
        }
        WM_DRAWITEM => {
            // SAFETY: Windows passes a valid DRAWITEMSTRUCT for the message.
            let di = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            draw_swatch(di);
            LRESULT(1)
        }
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
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
