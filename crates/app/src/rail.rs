//! The tools rail: a native child window along the window's outer edge
//! with an icon button for each tool (see `toolpane.rs` for the layout).
//!
//! It paints itself with GDI in the system menu bar's colours, shows each
//! button's name as a standard tooltip, and turns a click into the same
//! `WM_COMMAND` the Tools menu sends, so the main window handles both the
//! same way. It never takes the keyboard: the terminal keeps it.

#![allow(unsafe_code)]

use std::cell::RefCell;

use ferossh_render::Rgb;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
    EndPaint, FF_DONTCARE, FW_NORMAL, FillRect, HGDIOBJ, InvalidateRect, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TOOLTIPS_CLASSW, TTF_SUBCLASS,
    TTM_ADDTOOLW, TTM_NEWTOOLRECTW, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, GetClientRect, HMENU, IDC_ARROW,
    LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER,
    SendMessageW, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WM_COMMAND, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, PWSTR, w};

/// `WM_MOUSELEAVE` (the windows crate keeps it with the controls).
const WM_MOUSELEAVE: u32 = 0x02A3;

use crate::layout::Rect;
use crate::toolpane::{RailItem, item_at, item_rect};

/// What the rail shows; the main window sets it.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// The tool that is open (its button is highlighted).
    pub active: Option<RailItem>,
    pub pinned: bool,
    pub background: Rgb,
    /// The open tool's button.
    pub selected: Rgb,
    pub text: Rgb,
    pub accent: Rgb,
    pub dpi: u32,
}

struct State {
    hwnd: HWND,
    tooltip: HWND,
    look: Look,
    /// The button under the mouse.
    hot: Option<RailItem>,
    /// The button pressed and not yet released.
    pressed: Option<RailItem>,
    tracking: bool,
    /// The command each button sends, in `RailItem::ALL` order.
    commands: [u32; 7],
    /// The shortcut of each button's command, as its tooltip says it
    /// (Settings > Key bindings); empty for none.
    keys: [String; 7],
}

thread_local! {
    // Separate from the main window's state, so painting the rail never
    // has to borrow it (the main window may be mid-update when Windows
    // asks the rail to paint).
    static RAIL: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Creates the rail, hidden, as a child of `parent`. `commands` are the
/// `WM_COMMAND` ids its buttons send, in `RailItem::ALL` order.
pub fn create(parent: HWND, commands: [u32; 7], look: Look) -> windows::core::Result<HWND> {
    // SAFETY: registering a class and creating windows with valid,
    // static strings; the handles stay owned by `parent`.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        };
        // Loads comctl32 (delay-loaded) and registers the tooltip class.
        let _ = InitCommonControlsEx(&icc);
        let class = w!("FeroSSHToolsRail");
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!(""),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU::default()),
            Some(instance.into()),
            None,
        )?;
        let tooltip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WS_POPUP
                | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    TTS_ALWAYSTIP | TTS_NOPREFIX,
                ),
            0,
            0,
            0,
            0,
            Some(hwnd),
            None,
            Some(instance.into()),
            None,
        )
        .unwrap_or_default();
        RAIL.with(|r| {
            *r.borrow_mut() = Some(State {
                hwnd,
                tooltip,
                look,
                hot: None,
                pressed: None,
                tracking: false,
                commands,
                keys: Default::default(),
            });
        });
        if !tooltip.is_invalid() {
            for (index, item) in RailItem::ALL.into_iter().enumerate() {
                let mut text = wide(item.name(false));
                let info = tool_info(hwnd, index, RECT::default(), &mut text);
                SendMessageW(
                    tooltip,
                    TTM_ADDTOOLW,
                    Some(WPARAM(0)),
                    Some(LPARAM(&raw const info as isize)),
                );
            }
        }
        Ok(hwnd)
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn tool_info(hwnd: HWND, index: usize, rect: RECT, text: &mut [u16]) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_SUBCLASS,
        hwnd,
        uId: index + 1,
        rect,
        lpszText: PWSTR(text.as_mut_ptr()),
        ..Default::default()
    }
}

/// Puts the rail at `at` (client coordinates of the parent), or hides it.
pub fn place(at: Option<Rect>) {
    let Some((hwnd, tooltip, pinned)) = RAIL.with(|r| {
        r.borrow()
            .as_ref()
            .map(|s| (s.hwnd, s.tooltip, s.look.pinned))
    }) else {
        return;
    };
    // SAFETY: our own child window.
    unsafe {
        match at {
            Some(r) => {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let _ = ShowWindow(hwnd, SW_SHOWNA);
                update_tips(hwnd, tooltip, r.w, r.h, pinned);
            }
            None => {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

/// Moves each tooltip to its button, with the pin's current wording.
fn update_tips(hwnd: HWND, tooltip: HWND, w: i32, h: i32, pinned: bool) {
    if tooltip.is_invalid() {
        return;
    }
    for (index, item) in RailItem::ALL.into_iter().enumerate() {
        let r = item_rect(item, w, h);
        let rect = RECT {
            left: r.x,
            top: r.y,
            right: r.right(),
            bottom: r.bottom(),
        };
        let key = RAIL.with(|r| {
            r.try_borrow()
                .ok()
                .and_then(|r| r.as_ref().and_then(|s| s.keys.get(index).cloned()))
        });
        let text = match key.filter(|k| !k.is_empty()) {
            Some(k) => format!("{} ({k})", item.name(pinned)),
            None => item.name(pinned).to_owned(),
        };
        let mut text = wide(&text);
        let info = tool_info(hwnd, index, rect, &mut text);
        // SAFETY: the tooltip copies the struct and text during the call.
        unsafe {
            SendMessageW(
                tooltip,
                TTM_NEWTOOLRECTW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const info as isize)),
            );
            SendMessageW(
                tooltip,
                TTM_UPDATETIPTEXTW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const info as isize)),
            );
        }
    }
}

/// The buttons' shortcuts, in `RailItem::ALL` order, for their tooltips.
pub fn set_keys(keys: [String; 7]) {
    let changed = RAIL.with(|r| {
        let mut r = r.try_borrow_mut().ok()?;
        let s = r.as_mut()?;
        if s.keys == keys {
            return None;
        }
        s.keys = keys;
        Some((s.hwnd, s.tooltip, s.look.pinned))
    });
    if let Some((hwnd, tooltip, pinned)) = changed {
        let mut rc = RECT::default();
        // SAFETY: our own child window.
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        update_tips(hwnd, tooltip, rc.right, rc.bottom, pinned);
    }
}

/// Changes what the rail shows; repaints only when something changed.
pub fn set_look(look: Look) {
    let changed = RAIL.with(|r| {
        let mut r = r.borrow_mut();
        let s = r.as_mut()?;
        if s.look == look {
            return None;
        }
        let pin = s.look.pinned != look.pinned;
        s.look = look;
        Some((s.hwnd, s.tooltip, pin))
    });
    if let Some((hwnd, tooltip, pin)) = changed {
        let mut rc = RECT::default();
        // SAFETY: our own child window.
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        if pin {
            let pinned = RAIL.with(|r| r.borrow().as_ref().is_some_and(|s| s.look.pinned));
            update_tips(hwnd, tooltip, rc.right, rc.bottom, pinned);
        }
    }
}

fn colorref(c: Rgb) -> COLORREF {
    COLORREF(u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16))
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Rgb::new(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

fn rect(r: Rect) -> RECT {
    RECT {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

fn paint(hwnd: HWND) {
    let Some((look, hot, pressed)) = RAIL.with(|r| {
        r.borrow()
            .as_ref()
            .map(|s| (s.look.clone(), s.hot, s.pressed))
    }) else {
        return;
    };
    // SAFETY: a normal BeginPaint/EndPaint pair on our own window; every
    // GDI object made here is selected out and deleted before returning.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let (w, h) = (rc.right, rc.bottom);
        let fill = |r: RECT, c: Rgb| {
            let brush = CreateSolidBrush(colorref(c));
            FillRect(dc, &r, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        };
        fill(rc, look.background);
        let em = 16 * look.dpi as i32 / 96;
        let font = CreateFontW(
            -em,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            FF_DONTCARE.0 as u32,
            w!("Segoe MDL2 Assets"),
        );
        let old = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        let top_end = item_rect(RailItem::Snippets, w, h).bottom();
        for item in RailItem::ALL {
            let r = item_rect(item, w, h);
            if item == RailItem::Palette || item == RailItem::Pin {
                // A window too short for both groups: the tools win.
                if r.y < top_end {
                    continue;
                }
            }
            let active = look.active == Some(item);
            let on = active || (item == RailItem::Pin && look.pinned);
            if active {
                fill(rect(r), look.selected);
                // The accent bar on the outer side of the open tool.
                let bar = (3 * look.dpi as i32 / 96).max(2);
                let inset = r.h / 4;
                fill(
                    RECT {
                        left: 0,
                        top: r.y + inset,
                        right: bar,
                        bottom: r.bottom() - inset,
                    },
                    look.accent,
                );
            } else if pressed == Some(item) {
                fill(rect(r), mix(look.background, look.text, 0.16));
            } else if hot == Some(item) {
                fill(rect(r), mix(look.background, look.text, 0.08));
            }
            let colour = if on {
                look.text
            } else {
                mix(look.text, look.background, 0.25)
            };
            SetTextColor(dc, colorref(colour));
            let mut glyph = [0u16; 2];
            let text = item.glyph(look.pinned).encode_utf16(&mut glyph);
            let mut tr = rect(r);
            DrawTextW(
                dc,
                text,
                &mut tr,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = EndPaint(hwnd, &ps);
    }
}

fn point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16;
    (i32::from(x), i32::from(y))
}

fn hit(hwnd: HWND, lparam: LPARAM) -> Option<RailItem> {
    let (x, y) = point(lparam);
    let mut rc = RECT::default();
    // SAFETY: our own window.
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    item_at(x, y, rc.right, rc.bottom)
}

/// Sets the hot or pressed button; repaints when it changed.
fn set(hwnd: HWND, f: impl FnOnce(&mut State) -> bool) {
    let changed = RAIL.with(|r| r.borrow_mut().as_mut().is_some_and(f));
    if changed {
        // SAFETY: our own window.
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let item = hit(hwnd, lparam);
            let track = RAIL.with(|r| {
                r.borrow_mut()
                    .as_mut()
                    .is_some_and(|s| !std::mem::replace(&mut s.tracking, true))
            });
            if track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: valid struct for our own window.
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
            }
            set(hwnd, |s| std::mem::replace(&mut s.hot, item) != item);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            set(hwnd, |s| {
                s.tracking = false;
                s.pressed = None;
                s.hot.take().is_some()
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let item = hit(hwnd, lparam);
            set(hwnd, |s| std::mem::replace(&mut s.pressed, item) != item);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let item = hit(hwnd, lparam);
            let mut command = None;
            set(hwnd, |s| {
                let pressed = s.pressed.take();
                if pressed.is_some() && pressed == item {
                    let index = RailItem::ALL.iter().position(|i| Some(*i) == item);
                    command = index.and_then(|i| s.commands.get(i).copied());
                }
                pressed.is_some()
            });
            if let Some(id) = command {
                // SAFETY: plain query and post to our parent, which handles
                // it after this message returns.
                unsafe {
                    let parent = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd)
                        .unwrap_or_default();
                    let _ = PostMessageW(Some(parent), WM_COMMAND, WPARAM(id as usize), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
