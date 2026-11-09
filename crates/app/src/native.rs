//! Helpers the native tool panels share (`conn_view`, `live_panel`):
//! the system font at a DPI, text in and out of controls, key state.

#![allow(unsafe_code)]

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, HFONT, LOGFONTW};
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowTextLengthW, GetWindowTextW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SendMessageW,
    WM_SETFONT,
};

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
