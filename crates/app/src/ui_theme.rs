//! Paints the window's own parts in the colours `ui_colours` picks
//! (Settings > Window colours): the menu bar, the title bar, popup menus
//! and the native tools pane's controls.
//!
//! - The menu bar is drawn here through the undocumented `WM_UAHDRAWMENU`
//!   and `WM_UAHDRAWMENUITEM` messages Windows sends a themed window for
//!   its menu bar, as Notepad++ and others do; the white line Windows
//!   leaves under it is painted over too.
//! - Popup menus, scroll bars and buttons use Windows' own dark variants:
//!   uxtheme's `SetPreferredAppMode`, `AllowDarkModeForWindow` and
//!   `FlushMenuThemes` (exported by ordinal only, Windows 10 1903 and
//!   later) and the `DarkMode_Explorer` / `DarkMode_CFD` visual styles.
//!   Without them (older Windows) only those parts stay light.
//! - The title bar turns dark through DWM, and takes the menu bar's colour
//!   on Windows 11.
//! - The panels ask here for their colours ([`sys_color`], [`sys_brush`])
//!   and for edit boxes', labels' and lists' ([`def_window_proc`]).
//!
//! With Windows' light look (`custom` off) nothing is changed: Windows
//! draws everything as it did before window colours existed.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::sync::OnceLock;

use ferossh_render::Rgb;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_BTNFACE, COLOR_GRAYTEXT, COLOR_MENU, COLOR_MENUBAR, COLOR_MENUTEXT,
    COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW, CreateSolidBrush, DT_CENTER,
    DT_HIDEPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawFocusRect, DrawTextW, EndPaint,
    FillRect, GetSysColor, GetSysColorBrush, GetWindowDC, HBRUSH, HDC, HFONT, HGDIOBJ,
    InvalidateRect, MapWindowPoints, OffsetRect, PAINTSTRUCT, RDW_ALLCHILDREN, RDW_ERASE,
    RDW_FRAME, RDW_INVALIDATE, RedrawWindow, ReleaseDC, SYS_COLOR_INDEX, SelectObject, SetBkColor,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, DRAWITEMSTRUCT,
    LVM_GETHEADER, LVM_SETBKCOLOR, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, NM_CUSTOMDRAW,
    NMCUSTOMDRAW, NMHDR, ODS_DISABLED, ODS_GRAYED, ODS_HOTLIGHT, ODS_INACTIVE, ODS_NOACCEL,
    ODS_SELECTED, SetWindowTheme, TCIF_TEXT, TCITEMW, TCM_GETCURSEL, TCM_GETITEMCOUNT,
    TCM_GETITEMRECT, TCM_GETITEMW, TVM_SETBKCOLOR, TVM_SETTEXTCOLOR,
};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DrawMenuBar, EnumChildWindows, GWL_STYLE, GetClassNameW, GetClientRect,
    GetMenuBarInfo, GetMenuItemInfoW, GetWindowLongPtrW, GetWindowRect, MENUBARINFO, MENUITEMINFOW,
    MIIM_STRING, NONCLIENTMETRICSW, OBJID_MENU, SPI_GETNONCLIENTMETRICS, SendMessageW,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_ERASEBKGND,
    WM_GETFONT, WM_NCACTIVATE, WM_NCPAINT, WM_NOTIFY, WM_PAINT,
};
use windows::core::{BOOL, PCSTR, PCWSTR, PWSTR, w};

use crate::ui_colours::{System, UiColours};

/// Windows sends these to a themed window to draw its menu bar.
const WM_UAHDRAWMENU: u32 = 0x0091;
const WM_UAHDRAWMENUITEM: u32 = 0x0092;

/// `UAHMENU`: the menu bar being drawn.
#[repr(C)]
struct UahMenu {
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    dc: HDC,
    flags: u32,
}

/// `UAHDRAWMENUITEM`: one item of it. Only the start of its `UAHMENUITEM`
/// (the item's position) is read.
#[repr(C)]
struct UahDrawMenuItem {
    dis: DRAWITEMSTRUCT,
    um: UahMenu,
    position: i32,
}

/// The colours in use and the brushes made from them.
struct Current {
    c: UiColours,
    window: HBRUSH,
    control: HBRUSH,
    bar: HBRUSH,
    hot: HBRUSH,
    /// Our colours were ever applied: going back to Windows' look then
    /// has to undo them.
    ever_custom: bool,
    /// The menu font, at the DPI it was made for.
    menu_font: Option<(u32, HFONT)>,
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

fn colorref(c: Rgb) -> COLORREF {
    COLORREF(u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16))
}

fn rgb(c: u32) -> Rgb {
    Rgb::new(c as u8, (c >> 8) as u8, (c >> 16) as u8)
}

/// The colours in use (Windows' light look until [`apply`] runs).
pub fn colours() -> UiColours {
    CURRENT
        .with(|c| c.try_borrow().ok()?.as_ref().map(|c| c.c))
        .unwrap_or_else(|| {
            crate::ui_colours::resolve(
                ferossh_config::WindowColours::Light,
                &ferossh_render::Theme::default(),
                &system(false),
            )
        })
}

fn custom() -> bool {
    colours().custom
}

/// Windows' system colours, and its app mode when `mode` (else light).
pub fn system(mode: bool) -> System {
    // SAFETY: plain queries.
    let sys = |i: SYS_COLOR_INDEX| rgb(unsafe { GetSysColor(i) });
    System {
        menu_bar: sys(COLOR_MENUBAR),
        window: sys(COLOR_WINDOW),
        text: sys(COLOR_MENUTEXT),
        gray: sys(COLOR_GRAYTEXT),
        highlight: sys(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHT),
        dark: mode && apps_use_dark(),
    }
}

/// Windows' app mode (Settings > Personalisation > Colours) is dark.
fn apps_use_dark() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: a DWORD-sized buffer for a DWORD value.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    r.is_ok() && value == 0
}

/// Windows' build number, 0 when unknown.
fn build() -> u32 {
    let mut buf = [0u16; 16];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the buffer's size in bytes is passed with it.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return 0;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(buf.get(..end).unwrap_or_default())
        .trim()
        .parse()
        .unwrap_or(0)
}

/// uxtheme's dark mode functions, where this Windows has them.
#[derive(Default)]
struct Ux {
    set_preferred_app_mode: Option<unsafe extern "system" fn(i32) -> i32>,
    allow_dark_mode_for_window: Option<unsafe extern "system" fn(HWND, BOOL) -> BOOL>,
    flush_menu_themes: Option<unsafe extern "system" fn()>,
}

fn ux() -> &'static Ux {
    static UX: OnceLock<Ux> = OnceLock::new();
    UX.get_or_init(|| {
        // The ordinals mean these functions from Windows 10 1903 on.
        if build() < 18362 {
            return Ux::default();
        }
        // SAFETY: uxtheme from System32; each ordinal's signature is the
        // one Windows has exported under it since 1903.
        unsafe {
            let Ok(lib) = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            else {
                return Ux::default();
            };
            let get = |ordinal: usize| GetProcAddress(lib, PCSTR(ordinal as *const u8));
            Ux {
                set_preferred_app_mode: get(135).map(|f| std::mem::transmute(f)),
                allow_dark_mode_for_window: get(133).map(|f| std::mem::transmute(f)),
                flush_menu_themes: get(136).map(|f| std::mem::transmute(f)),
            }
        }
    })
}

/// Lets Windows draw `hwnd` (a button, a scroll bar) dark.
fn allow_dark(hwnd: HWND, dark: bool) {
    if let Some(f) = ux().allow_dark_mode_for_window {
        // SAFETY: our own window.
        unsafe {
            let _ = f(hwnd, dark.into());
        }
    }
}

/// Uses `c` for the window `main` and everything in it; does nothing when
/// they are already in use.
pub fn apply(main: HWND, c: UiColours) {
    let changed = CURRENT.with(|cur| {
        let Ok(mut cur) = cur.try_borrow_mut() else {
            return false;
        };
        if cur.as_ref().is_some_and(|x| x.c == c) {
            return false;
        }
        // SAFETY: brushes we made; the old ones are no longer selected
        // anywhere once replaced.
        unsafe {
            let ever_custom = cur.as_ref().is_some_and(|x| x.ever_custom) || c.custom;
            let menu_font = cur.as_mut().and_then(|x| x.menu_font.take());
            if let Some(old) = cur.take() {
                for b in [old.window, old.control, old.bar, old.hot] {
                    let _ = DeleteObject(HGDIOBJ(b.0));
                }
            }
            *cur = Some(Current {
                c,
                window: CreateSolidBrush(colorref(c.window)),
                control: CreateSolidBrush(colorref(c.control)),
                bar: CreateSolidBrush(colorref(c.bar)),
                hot: CreateSolidBrush(colorref(c.hot)),
                ever_custom,
                menu_font,
            });
        }
        true
    });
    if !changed {
        return;
    }
    // Popup menus (the menus, context menus) dark or light.
    let ux = ux();
    if let Some(f) = ux.set_preferred_app_mode {
        // 0 = default (light), 2 = force dark.
        // SAFETY: plain call into uxtheme.
        unsafe {
            f(if c.custom && c.dark { 2 } else { 0 });
        }
    }
    if let Some(f) = ux.flush_menu_themes {
        // SAFETY: as above.
        unsafe { f() };
    }
    allow_dark(main, c.custom && c.dark);
    title_bar(main, &c);
    restyle(main);
    // SAFETY: our own window.
    unsafe {
        let _ = DrawMenuBar(main);
        let _ = RedrawWindow(
            Some(main),
            None,
            None,
            RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN,
        );
    }
}

/// A popup window's title bar in the colours in use.
pub fn popup_frame(hwnd: HWND) {
    title_bar(hwnd, &colours());
}

/// The title bar: dark or light, and on Windows 11 the menu bar's colour.
fn title_bar(main: HWND, c: &UiColours) {
    let set = |attr: DWMWINDOWATTRIBUTE, value: u32| {
        // SAFETY: a DWORD-sized value for each of these attributes.
        unsafe {
            DwmSetWindowAttribute(
                main,
                attr,
                (&raw const value).cast(),
                size_of::<u32>() as u32,
            )
        }
    };
    let dark = u32::from(c.custom && c.dark);
    // Before Windows 10 20H1 the attribute was 19.
    if set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark).is_err() {
        let _ = set(DWMWINDOWATTRIBUTE(19), dark);
    }
    // Windows 11 only; DWMWA_COLOR_DEFAULT puts Windows' own back.
    const DEFAULT: u32 = 0xFFFF_FFFF;
    let (caption, text) = if c.custom {
        (colorref(c.bar).0, colorref(c.text).0)
    } else {
        (DEFAULT, DEFAULT)
    };
    let _ = set(DWMWA_CAPTION_COLOR, caption);
    let _ = set(DWMWA_TEXT_COLOR, text);
}

/// A system colour, or ours in its place: the panels paint with these.
pub fn sys_color(index: SYS_COLOR_INDEX) -> u32 {
    let c = colours();
    if c.custom {
        let ours = match index {
            COLOR_WINDOW | COLOR_BTNFACE => Some(c.window),
            COLOR_WINDOWTEXT | COLOR_MENUTEXT => Some(c.text),
            COLOR_GRAYTEXT => Some(c.dim),
            COLOR_MENUBAR | COLOR_MENU => Some(c.bar),
            _ => None,
        };
        if let Some(rgb) = ours {
            return colorref(rgb).0;
        }
    }
    // SAFETY: plain query.
    unsafe { GetSysColor(index) }
}

/// A brush of [`sys_color`]'s colour. Owned here: never deleted by the
/// caller.
pub fn sys_brush(index: SYS_COLOR_INDEX) -> HBRUSH {
    let ours = CURRENT.with(|cur| {
        let cur = cur.try_borrow().ok()?;
        let cur = cur.as_ref().filter(|x| x.c.custom)?;
        match index {
            COLOR_WINDOW | COLOR_BTNFACE => Some(cur.window),
            COLOR_MENUBAR | COLOR_MENU => Some(cur.bar),
            _ => None,
        }
    });
    // SAFETY: plain query; system brushes are never deleted.
    ours.unwrap_or_else(|| unsafe { GetSysColorBrush(index) })
}

/// Red for a problem, readable on the window colour.
pub fn error_color() -> COLORREF {
    colorref(colours().error)
}

/// A progress bar's empty part.
pub fn track_color() -> COLORREF {
    colorref(colours().track)
}

/// `DefWindowProcW`, but giving edit boxes, labels, buttons and drop-down
/// lists our colours when they are in use. The panels end their window
/// procedures with it.
pub fn def_window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(r) = ctl_color(msg, wparam) {
        return r;
    }
    // SAFETY: default handling, with the arguments Windows gave.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn ctl_color(msg: u32, wparam: WPARAM) -> Option<LRESULT> {
    if !matches!(
        msg,
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN
    ) {
        return None;
    }
    CURRENT.with(|cur| {
        let cur = cur.try_borrow().ok()?;
        let cur = cur.as_ref().filter(|x| x.c.custom)?;
        let dc = HDC(wparam.0 as *mut _);
        let field = matches!(msg, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX);
        let (bg, brush) = if field {
            (cur.c.control, cur.control)
        } else {
            (cur.c.window, cur.window)
        };
        // SAFETY: the DC Windows passes for the control's paint.
        unsafe {
            SetTextColor(dc, colorref(cur.c.text));
            SetBkColor(dc, colorref(bg));
            if !field {
                SetBkMode(dc, TRANSPARENT);
            }
        }
        Some(LRESULT(brush.0 as isize))
    })
}

/// Styles `root` and every control in it for the colours in use: called
/// on a change, and by panels after they create controls. With Windows'
/// light look that was never changed, it does nothing.
pub fn restyle(root: HWND) {
    let (c, ever) = CURRENT.with(|cur| {
        cur.try_borrow()
            .ok()
            .and_then(|cur| cur.as_ref().map(|x| (x.c, x.ever_custom)))
            .unwrap_or((colours(), false))
    });
    if !ever {
        return;
    }
    unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> BOOL {
        style_control(hwnd, &colours());
        true.into()
    }
    style_control(root, &c);
    // SAFETY: a walk over our own windows; `each` lives for the program.
    unsafe {
        let _ = EnumChildWindows(Some(root), Some(each), LPARAM(0));
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: the buffer's length is passed with it.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(buf.get(..n).unwrap_or_default())
}

fn set_theme(hwnd: HWND, app: Option<PCWSTR>, idlist: Option<PCWSTR>) {
    // SAFETY: static strings or null; our own control.
    unsafe {
        let _ = SetWindowTheme(
            hwnd,
            app.unwrap_or(PCWSTR::null()),
            idlist.unwrap_or(PCWSTR::null()),
        );
    }
}

fn style_control(hwnd: HWND, c: &UiColours) {
    let dark = c.custom && c.dark;
    let class = class_name(hwnd).to_ascii_lowercase();
    // SAFETY: messages to our own controls with plain values.
    unsafe {
        let send = |msg: u32, l: u32| {
            SendMessageW(hwnd, msg, Some(WPARAM(0)), Some(LPARAM(l as isize)));
        };
        match class.as_str() {
            "edit" | "combobox" => {
                set_theme(hwnd, dark.then_some(w!("DarkMode_CFD")), None);
            }
            "systreeview32" => {
                set_theme(
                    hwnd,
                    Some(if dark {
                        w!("DarkMode_Explorer")
                    } else {
                        w!("Explorer")
                    }),
                    None,
                );
                // -1: the default colours.
                let (bg, text) = if c.custom {
                    (colorref(c.window).0, colorref(c.text).0)
                } else {
                    (u32::MAX, u32::MAX)
                };
                send(TVM_SETBKCOLOR, bg);
                send(TVM_SETTEXTCOLOR, text);
            }
            "syslistview32" => {
                set_theme(
                    hwnd,
                    Some(if dark {
                        w!("DarkMode_Explorer")
                    } else {
                        w!("Explorer")
                    }),
                    None,
                );
                let (bg, text) = if c.custom {
                    (c.window, c.text)
                } else {
                    (
                        rgb(GetSysColor(COLOR_WINDOW)),
                        rgb(GetSysColor(COLOR_WINDOWTEXT)),
                    )
                };
                send(LVM_SETBKCOLOR, colorref(bg).0);
                // CLR_NONE: the theme's selection shows behind the text.
                send(
                    LVM_SETTEXTBKCOLOR,
                    if c.custom { u32::MAX } else { colorref(bg).0 },
                );
                send(LVM_SETTEXTCOLOR, colorref(text).0);
                let header = HWND(SendMessageW(hwnd, LVM_GETHEADER, None, None).0 as *mut _);
                if !header.is_invalid() {
                    set_theme(header, dark.then_some(w!("DarkMode_ItemsView")), None);
                }
                // The header's text colour (its custom draw goes to the
                // list).
                let _ = SetWindowSubclass(hwnd, Some(list_proc), SUBCLASS_ID, 0);
            }
            "button" => {
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
                let kind = style & 0xF;
                let push_like = style & 0x1000 != 0; // BS_PUSHLIKE
                // BS_PUSHBUTTON, BS_DEFPUSHBUTTON, or a push-like check box.
                if kind <= 1 || push_like {
                    allow_dark(hwnd, dark);
                    set_theme(hwnd, dark.then_some(w!("DarkMode_Explorer")), None);
                } else if dark {
                    // A themed check box ignores the label colour; the
                    // classic one takes it.
                    set_theme(hwnd, Some(w!("")), Some(w!("")));
                } else {
                    set_theme(hwnd, None, None);
                }
            }
            "systabcontrol32" => {
                let _ = SetWindowSubclass(hwnd, Some(tab_proc), SUBCLASS_ID, 0);
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            // Our panels: their scroll bars.
            name if name.starts_with("ferossh") => {
                allow_dark(hwnd, dark);
                set_theme(hwnd, dark.then_some(w!("DarkMode_Explorer")), None);
            }
            _ => {}
        }
    }
}

/// Our subclass id on controls (`native::pass_edge` uses 1).
const SUBCLASS_ID: usize = 0x5549;

/// A list: its header's text in our colour.
unsafe extern "system" fn list_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_NOTIFY && custom() {
        // SAFETY: Windows passes a valid NMHDR (an NMCUSTOMDRAW for
        // NM_CUSTOMDRAW) for WM_NOTIFY.
        unsafe {
            let hdr = &*(lparam.0 as *const NMHDR);
            let header = HWND(SendMessageW(hwnd, LVM_GETHEADER, None, None).0 as *mut _);
            if hdr.code == NM_CUSTOMDRAW && hdr.hwndFrom == header {
                let cd = &*(lparam.0 as *const NMCUSTOMDRAW);
                if cd.dwDrawStage == CDDS_PREPAINT {
                    return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
                }
                if cd.dwDrawStage == CDDS_ITEMPREPAINT {
                    SetTextColor(cd.hdc, colorref(colours().text));
                    return LRESULT(CDRF_DODEFAULT as isize);
                }
            }
        }
    }
    // SAFETY: the default for everything else.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// A tab control, which Windows has no dark look for: drawn here while
/// our colours are in use.
unsafe extern "system" fn tab_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if custom() {
        match msg {
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                paint_tabs(hwnd);
                return LRESULT(0);
            }
            _ => {}
        }
    }
    // SAFETY: the default for everything else.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn paint_tabs(hwnd: HWND) {
    let c = colours();
    let Some((window, control)) = CURRENT.with(|cur| {
        cur.try_borrow()
            .ok()?
            .as_ref()
            .map(|x| (x.window, x.control))
    }) else {
        return;
    };
    // SAFETY: a BeginPaint/EndPaint pair on our own control; messages to
    // it with valid buffers; GDI objects made here are deleted and the
    // font is selected back out.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        FillRect(dc, &rc, window);
        let font = SendMessageW(hwnd, WM_GETFONT, None, None).0;
        let old = SelectObject(dc, HGDIOBJ(font as *mut _));
        SetBkMode(dc, TRANSPARENT);
        let count = SendMessageW(hwnd, TCM_GETITEMCOUNT, None, None).0.max(0) as usize;
        let selected = SendMessageW(hwnd, TCM_GETCURSEL, None, None).0;
        let line = CreateSolidBrush(colorref(c.dim));
        let accent = CreateSolidBrush(colorref(c.accent));
        let mut bottom = 0;
        for i in 0..count {
            let mut ir = RECT::default();
            if SendMessageW(
                hwnd,
                TCM_GETITEMRECT,
                Some(WPARAM(i)),
                Some(LPARAM(&raw mut ir as isize)),
            )
            .0 == 0
            {
                continue;
            }
            bottom = bottom.max(ir.bottom);
            let mut buf = [0u16; 128];
            let mut item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR(buf.as_mut_ptr()),
                cchTextMax: buf.len() as i32 - 1,
                ..Default::default()
            };
            SendMessageW(
                hwnd,
                TCM_GETITEMW,
                Some(WPARAM(i)),
                Some(LPARAM(&raw mut item as isize)),
            );
            let n = buf.iter().position(|&ch| ch == 0).unwrap_or(0);
            let mut text: Vec<u16> = buf.get(..n).unwrap_or_default().to_vec();
            let on = i as isize == selected;
            if on {
                FillRect(dc, &ir, control);
                let mut mark = ir;
                mark.top = mark.bottom - 2;
                FillRect(dc, &mark, accent);
            }
            SetTextColor(dc, colorref(if on { c.text } else { c.dim }));
            let mut tr = ir;
            DrawTextW(
                dc,
                &mut text,
                &mut tr,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_HIDEPREFIX,
            );
            if on && GetFocus() == hwnd {
                let mut fr = ir;
                fr.left += 2;
                fr.top += 2;
                fr.right -= 2;
                fr.bottom -= 3;
                let _ = DrawFocusRect(dc, &fr);
            }
        }
        // A thin line under the tabs.
        if bottom > 0 {
            let under = RECT {
                left: rc.left,
                top: bottom,
                right: rc.right,
                bottom: bottom + 1,
            };
            FillRect(dc, &under, line);
        }
        let _ = DeleteObject(HGDIOBJ(line.0));
        let _ = DeleteObject(HGDIOBJ(accent.0));
        SelectObject(dc, old);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// The menu bar and the line under it, in our colours; `None` leaves the
/// message to the window procedure. The main window calls it first.
pub fn frame_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if !matches!(
        msg,
        WM_UAHDRAWMENU | WM_UAHDRAWMENUITEM | WM_NCPAINT | WM_NCACTIVATE
    ) || !custom()
    {
        return None;
    }
    match msg {
        WM_UAHDRAWMENU => {
            // SAFETY: Windows passes a UAHMENU with this message.
            let um = unsafe { &*(lparam.0 as *const UahMenu) };
            draw_bar(hwnd, um.dc);
            Some(LRESULT(1))
        }
        WM_UAHDRAWMENUITEM => {
            // SAFETY: Windows passes a UAHDRAWMENUITEM with this message.
            let item = unsafe { &*(lparam.0 as *const UahDrawMenuItem) };
            draw_item(hwnd, item);
            Some(LRESULT(1))
        }
        _ => {
            // Windows' own frame first, then the line it leaves white.
            // SAFETY: default handling, with the arguments Windows gave.
            let r = unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            cover_bar_line(hwnd);
            Some(r)
        }
    }
}

/// The menu bar's rectangle in window coordinates.
fn bar_rect(hwnd: HWND) -> Option<RECT> {
    let mut mbi = MENUBARINFO {
        cbSize: size_of::<MENUBARINFO>() as u32,
        ..Default::default()
    };
    let mut wr = RECT::default();
    // SAFETY: plain queries with sized structs.
    unsafe {
        GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut mbi).ok()?;
        GetWindowRect(hwnd, &mut wr).ok()?;
        let mut r = mbi.rcBar;
        let _ = OffsetRect(&mut r, -wr.left, -wr.top);
        Some(r)
    }
}

fn draw_bar(hwnd: HWND, dc: HDC) {
    let Some(bar) = CURRENT.with(|c| c.try_borrow().ok()?.as_ref().map(|c| c.bar)) else {
        return;
    };
    if let Some(r) = bar_rect(hwnd) {
        // SAFETY: the DC Windows passed for the menu bar.
        unsafe {
            FillRect(dc, &r, bar);
        }
    }
}

/// The menu font at the window's DPI.
fn menu_font(hwnd: HWND) -> Option<HFONT> {
    // SAFETY: plain query.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    CURRENT.with(|cur| {
        let mut cur = cur.try_borrow_mut().ok()?;
        let cur = cur.as_mut()?;
        if let Some((at, f)) = cur.menu_font
            && at == dpi
        {
            return Some(f);
        }
        let mut m = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        // SAFETY: a correctly sized struct for the call to fill; the old
        // font is deleted once replaced (not selected anywhere then).
        unsafe {
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                m.cbSize,
                Some((&raw mut m).cast()),
                0,
                dpi,
            )
            .ok()?;
            let font = CreateFontIndirectW(&m.lfMenuFont);
            if let Some((_, old)) = cur.menu_font.replace((dpi, font)) {
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
            Some(font)
        }
    })
}

fn draw_item(hwnd: HWND, item: &UahDrawMenuItem) {
    let Some((c, bar, hot)) =
        CURRENT.with(|cur| cur.try_borrow().ok()?.as_ref().map(|x| (x.c, x.bar, x.hot)))
    else {
        return;
    };
    let mut buf = [0u16; 256];
    let mut mii = MENUITEMINFOW {
        cbSize: size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_STRING,
        dwTypeData: PWSTR(buf.as_mut_ptr()),
        cch: buf.len() as u32 - 1,
        ..Default::default()
    };
    let dis = &item.dis;
    let state = dis.itemState;
    // SAFETY: a sized struct and buffer for the item's text; the DC is the
    // one Windows passed, and the font is selected back out.
    unsafe {
        if GetMenuItemInfoW(item.um.menu, item.position as u32, true, &mut mii).is_err() {
            return;
        }
        let n = (mii.cch as usize).min(buf.len());
        let mut text: Vec<u16> = buf.get(..n).unwrap_or_default().to_vec();
        let lit = state.0 & (ODS_HOTLIGHT.0 | ODS_SELECTED.0) != 0;
        let off = state.0 & (ODS_INACTIVE.0 | ODS_GRAYED.0 | ODS_DISABLED.0) != 0;
        let dc = dis.hDC;
        FillRect(dc, &dis.rcItem, if lit { hot } else { bar });
        let old = menu_font(hwnd).map(|f| SelectObject(dc, HGDIOBJ(f.0)));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, colorref(if off && !lit { c.dim } else { c.text }));
        let mut flags = DT_CENTER | DT_VCENTER | DT_SINGLELINE;
        if state.0 & ODS_NOACCEL.0 != 0 {
            flags |= DT_HIDEPREFIX;
        }
        let mut r = dis.rcItem;
        DrawTextW(dc, &mut text, &mut r, flags);
        if let Some(old) = old {
            SelectObject(dc, old);
        }
    }
}

/// Paints over the light line Windows draws between the menu bar and the
/// client area.
fn cover_bar_line(hwnd: HWND) {
    let Some(bar) = CURRENT.with(|c| c.try_borrow().ok()?.as_ref().map(|c| c.bar)) else {
        return;
    };
    if bar_rect(hwnd).is_none() {
        return;
    }
    // SAFETY: plain queries; a window DC released after use.
    unsafe {
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let mut pts = [
            windows::Win32::Foundation::POINT {
                x: client.left,
                y: client.top,
            },
            windows::Win32::Foundation::POINT {
                x: client.right,
                y: client.bottom,
            },
        ];
        MapWindowPoints(Some(hwnd), None, &mut pts);
        let [top_left, bottom_right] = pts;
        let mut wr = RECT::default();
        let _ = GetWindowRect(hwnd, &mut wr);
        let line = RECT {
            left: top_left.x - wr.left,
            top: top_left.y - wr.top - 1,
            right: bottom_right.x - wr.left,
            bottom: top_left.y - wr.top,
        };
        let dc = GetWindowDC(Some(hwnd));
        if !dc.is_invalid() {
            FillRect(dc, &line, bar);
            ReleaseDC(Some(hwnd), dc);
        }
    }
}
