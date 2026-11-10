//! Helpers the native tool panels share (`conn_view`, `live_panel`, ...):
//! the system font at a DPI, text in and out of controls, key state, and
//! letting the mouse through to the terminal's edge.

#![allow(unsafe_code)]

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, HFONT, LOGFONTW};
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowTextLengthW, GetWindowTextW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SendMessageW,
    WM_SETFONT,
};

thread_local! {
    /// The main window and, in its client coordinates, the strip around
    /// the terminal's edge that drags the tools pane.
    static EDGE: Cell<Option<(HWND, crate::layout::Rect)>> = const { Cell::new(None) };
}

/// Sets the strip around the terminal's edge (`None` when there is no
/// edge); the panels' controls let the mouse through to the main window
/// there, so the edge can be grabbed from the tool's side too.
pub fn set_edge_grab(main: HWND, grab: Option<crate::layout::Rect>) {
    EDGE.with(|e| e.set(grab.map(|g| (main, g))));
}

/// Lets the mouse through `panel` and every control in it at the time of
/// the call (a list's header too) where it is over the edge's strip, and
/// styles them for the window's colours (`ui_theme::restyle`).
/// Called again when controls are added later; a repeat is harmless.
///
/// A panel also ends its window procedure with
/// `ui_theme::def_window_proc` and paints with `ui_theme::sys_color` and
/// `ui_theme::sys_brush`, so it follows Settings > Window colours.
pub fn pass_edge(panel: HWND) {
    crate::ui_theme::restyle(panel);
    unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> windows::core::BOOL {
        // SAFETY: subclassing our own window with a procedure that lives
        // for the whole program.
        unsafe {
            let _ = SetWindowSubclass(hwnd, Some(edge_proc), 1, 0);
        }
        true.into()
    }
    // SAFETY: as above; the walk covers children's children.
    unsafe {
        let _ = SetWindowSubclass(panel, Some(edge_proc), 1, 0);
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumChildWindows(
            Some(panel),
            Some(each),
            LPARAM(0),
        );
    }
}

unsafe extern "system" fn edge_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    const WM_NCHITTEST: u32 = 0x0084;
    const HTTRANSPARENT: isize = -1;
    if msg == WM_NCHITTEST
        && let Some((main, grab)) = EDGE.with(Cell::get)
    {
        // Screen coordinates, signed 16-bit each.
        let mut pt = POINT {
            x: i32::from(lparam.0 as u16 as i16),
            y: i32::from((lparam.0 >> 16) as u16 as i16),
        };
        // SAFETY: a plain conversion for our own window.
        let _ = unsafe { ScreenToClient(main, &mut pt) };
        if grab.contains(pt.x, pt.y) {
            // The window underneath, in the end the main window, takes it.
            return LRESULT(HTTRANSPARENT);
        }
    }
    // SAFETY: the default for everything else.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn scale(dip: i32, dpi: u32) -> i32 {
    dip * dpi as i32 / 96
}

/// The system message font at `dpi`, and a larger, semi-bold one for the
/// title.
pub fn fonts(dpi: u32) -> (HFONT, HFONT) {
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

pub fn set_font(hwnd: HWND, font: HFONT) {
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

pub fn key_down(vk: u16) -> bool {
    // SAFETY: plain query.
    unsafe { GetKeyState(i32::from(vk)) < 0 }
}

pub fn search_text(search: HWND) -> String {
    // SAFETY: a buffer sized from the control's own length.
    unsafe {
        let n = GetWindowTextLengthW(search).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(search, &mut buf).max(0) as usize;
        String::from_utf16_lossy(buf.get(..got).unwrap_or_default())
    }
}
