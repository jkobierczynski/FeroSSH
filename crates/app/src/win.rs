//! The Win32 window, menu and message loop.

// Win32 calls are FFI; each unsafe block below is a direct, documented API call.
#![allow(unsafe_code)]

use std::cell::RefCell;

use ferossh_render::{
    CellMetrics, Cursor, CursorShape, FontSpec, Frame, PaneView, PixelRect, Renderer, Theme,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_ADD, VK_CONTROL, VK_MENU, VK_NUMPAD0, VK_OEM_MINUS, VK_OEM_PLUS,
    VK_SHIFT, VK_SUBTRACT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CheckMenuRadioItem, CreateMenu,
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW, HMENU, IDC_ARROW, LoadCursorW, MB_ICONERROR, MB_OK, MF_BYCOMMAND,
    MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, MessageBoxW, PostQuitMessage, RegisterClassW,
    SIZE_MINIMIZED, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetMenu, SetWindowPos,
    ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_COMMAND, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_GETDPISCALEDSIZE, WM_KEYDOWN, WM_KILLFOCUS, WM_PAINT,
    WM_SETFOCUS, WM_SIZE, WM_SYSKEYUP, WNDCLASSW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, Result, w};

use crate::echo::EchoScreen;

const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;
/// Space between the window edge and the grid, in DIPs.
const PADDING_DIP: i32 = 4;
const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_APPWINDOW;

/// Font families offered in View > Font. A pane's font can be any
/// installed family; this list is only the menu.
const FONT_CHOICES: [&str; 3] = ["Cascadia Mono", "Consolas", "Courier New"];

/// Menu command ids.
mod cmd {
    pub const EXIT: u32 = 100;
    pub const ZOOM_IN: u32 = 200;
    pub const ZOOM_OUT: u32 = 201;
    pub const ZOOM_RESET: u32 = 202;
    pub const HIDE_MENU: u32 = 203;
    /// Plus the index into `FONT_CHOICES`.
    pub const FONT: u32 = 300;
    /// Plus the index into `Theme::builtin()`.
    pub const THEME: u32 = 400;
}

/// One terminal pane with its own font and colour theme. Milestone 1 has
/// exactly one; tabs and splits add more.
struct Pane {
    screen: EchoScreen,
    font: FontSpec,
    theme: Theme,
    metrics: CellMetrics,
}

struct Window {
    hwnd: HWND,
    renderer: Renderer,
    dpi: u32,
    client: (i32, i32),
    pane: Pane,
    menu: HMENU,
    menu_visible: bool,
    focused: bool,
    /// Repaint everything on the next WM_PAINT, not only dirty rows.
    full_redraw: bool,
    /// A shortcut was handled on key-down; drop the WM_CHAR it may produce.
    swallow_char: bool,
    high_surrogate: Option<u16>,
}

thread_local! {
    // The UI thread's one window. Handlers borrow it briefly and never hold
    // the borrow across a call that sends messages back to the window.
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Runs `f` on the window state, or returns `None` before the window is set
/// up or when called re-entrantly.
fn with_window<R>(f: impl FnOnce(&mut Window) -> R) -> Option<R> {
    WINDOW.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

pub fn run() -> Result<()> {
    let result = create_and_run();
    if let Err(e) = &result {
        let text = HSTRING::from(format!("FeroSSH could not start:\n\n{}", e.message()));
        // SAFETY: plain message box with valid strings.
        unsafe { MessageBoxW(None, &text, w!("FeroSSH"), MB_OK | MB_ICONERROR) };
    }
    result
}

fn create_and_run() -> Result<()> {
    // SAFETY: plain Win32 calls on this thread with valid arguments; the
    // window procedure below upholds the WNDPROC contract.
    unsafe {
        // The manifest already sets this; the call covers a missing manifest.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(None)?.into();
        let class = w!("FeroSSHWindow");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // No background brush: the renderer paints every pixel.
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }

        let menu = build_menu()?;
        let hwnd = CreateWindowExW(
            EX_STYLE,
            class,
            w!("FeroSSH"),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            Some(menu),
            Some(instance),
            None,
        )?;

        // Size the window for 80x24 cells at the DPI of the monitor it
        // opened on.
        let dpi = GetDpiForWindow(hwnd);
        let font = FontSpec::default();
        let mut renderer = Renderer::new(hwnd, 1, 1)?;
        let metrics = renderer.metrics(&font, dpi)?;
        let (width, height) = window_size(&metrics, DEFAULT_COLS, DEFAULT_ROWS, dpi, true);
        WINDOW.with(|cell| {
            *cell.borrow_mut() = Some(Window {
                hwnd,
                renderer,
                dpi,
                client: (0, 0),
                pane: Pane {
                    screen: EchoScreen::new(DEFAULT_COLS, DEFAULT_ROWS),
                    font,
                    theme: Theme::dark(),
                    metrics,
                },
                menu,
                menu_visible: true,
                focused: false,
                full_redraw: true,
                swallow_char: false,
                high_surrogate: None,
            });
        });
        with_window(Window::update_menu_checks);
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn build_menu() -> Result<HMENU> {
    // SAFETY: menu construction with valid handles and static strings.
    unsafe {
        let file = CreatePopupMenu()?;
        AppendMenuW(file, MF_STRING, cmd::EXIT as usize, w!("E&xit\tAlt+F4"))?;

        let fonts = CreatePopupMenu()?;
        for (id, name) in (cmd::FONT..).zip(FONT_CHOICES) {
            AppendMenuW(fonts, MF_STRING, id as usize, &HSTRING::from(name))?;
        }
        let themes = CreatePopupMenu()?;
        for (id, theme) in (cmd::THEME..).zip(Theme::builtin()) {
            AppendMenuW(themes, MF_STRING, id as usize, &HSTRING::from(theme.name))?;
        }

        let view = CreatePopupMenu()?;
        AppendMenuW(
            view,
            MF_STRING,
            cmd::ZOOM_IN as usize,
            w!("&Larger text\tCtrl+="),
        )?;
        AppendMenuW(
            view,
            MF_STRING,
            cmd::ZOOM_OUT as usize,
            w!("&Smaller text\tCtrl+-"),
        )?;
        AppendMenuW(
            view,
            MF_STRING,
            cmd::ZOOM_RESET as usize,
            w!("&Reset text size\tCtrl+0"),
        )?;
        AppendMenuW(view, MF_SEPARATOR, 0, None)?;
        AppendMenuW(view, MF_POPUP, fonts.0 as usize, w!("&Font"))?;
        AppendMenuW(view, MF_POPUP, themes.0 as usize, w!("&Theme"))?;
        AppendMenuW(view, MF_SEPARATOR, 0, None)?;
        AppendMenuW(
            view,
            MF_STRING,
            cmd::HIDE_MENU as usize,
            w!("&Hide menu bar (Alt shows it)\tCtrl+Shift+M"),
        )?;

        let bar = CreateMenu()?;
        AppendMenuW(bar, MF_POPUP, file.0 as usize, w!("&File"))?;
        AppendMenuW(bar, MF_POPUP, view.0 as usize, w!("&View"))?;
        Ok(bar)
    }
}

fn padding(dpi: u32) -> i32 {
    PADDING_DIP * dpi as i32 / 96
}

/// Outer window size for a grid of `cols` x `rows` cells.
fn window_size(m: &CellMetrics, cols: usize, rows: usize, dpi: u32, menu: bool) -> (i32, i32) {
    let pad = padding(dpi);
    let mut r = RECT {
        left: 0,
        top: 0,
        right: cols as i32 * m.width as i32 + 2 * pad,
        bottom: rows as i32 * m.height as i32 + 2 * pad,
    };
    // SAFETY: valid pointer to a local rect. On failure the client size is
    // used as is, which only makes the window slightly small.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut r, STYLE, menu, EX_STYLE, dpi) };
    (r.right - r.left, r.bottom - r.top)
}

impl Window {
    /// Fits the grid to the client area.
    fn layout(&mut self) {
        let mut r = RECT::default();
        // SAFETY: valid window and out-pointer.
        if unsafe { GetClientRect(self.hwnd, &mut r) }.is_err() {
            return;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        self.client = (w, h);
        self.renderer.resize(w.max(1) as u32, h.max(1) as u32);
        let pad = padding(self.dpi);
        let m = &self.pane.metrics;
        let cols = (w - 2 * pad).max(0) / m.width.max(1) as i32;
        let rows = (h - 2 * pad).max(0) / m.height.max(1) as i32;
        self.pane
            .screen
            .resize(cols.max(1) as usize, rows.max(1) as usize);
        self.request_full_redraw();
    }

    fn request_full_redraw(&mut self) {
        self.full_redraw = true;
        // SAFETY: valid window; no erase, the renderer paints everything.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Asks Windows to repaint the band of rows the grid marked dirty.
    fn invalidate_dirty(&mut self) {
        let Some((first, last)) = self.pane.screen.grid().dirty_span() else {
            return;
        };
        let pad = padding(self.dpi);
        let ch = self.pane.metrics.height as i32;
        let r = RECT {
            left: 0,
            top: pad + first as i32 * ch,
            right: self.client.0,
            bottom: pad + (last as i32 + 1) * ch,
        };
        // SAFETY: valid window and rect.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&r), false);
        }
    }

    fn paint(&mut self, update: RECT) {
        // Rows Windows wants repainted (e.g. after a restore) join the
        // grid's own dirty rows; anything outside the rows means everything.
        let pad = padding(self.dpi);
        let ch = self.pane.metrics.height.max(1) as i32;
        let rows = self.pane.screen.grid().rows() as i32;
        if update.top < pad || update.bottom > pad + rows * ch {
            self.full_redraw = true;
        } else {
            let grid = self.pane.screen.grid_mut();
            for y in (update.top - pad) / ch..=(update.bottom - 1 - pad) / ch {
                grid.mark_dirty(y.max(0) as usize);
            }
        }

        let (cx, cy) = self.pane.screen.cursor();
        let view = PaneView {
            grid: self.pane.screen.grid(),
            font: &self.pane.font,
            theme: &self.pane.theme,
            rect: PixelRect {
                left: 0,
                top: 0,
                right: self.client.0,
                bottom: self.client.1,
            },
            origin: (pad, pad),
            cursor: Some(Cursor {
                x: cx,
                y: cy,
                shape: if self.focused {
                    CursorShape::Block
                } else {
                    CursorShape::Hollow
                },
            }),
            full: self.full_redraw,
        };
        match self.renderer.draw(&[view], self.dpi) {
            Ok(Frame::Presented) => {
                self.pane.screen.grid_mut().clear_dirty();
                self.full_redraw = false;
            }
            Ok(Frame::Lost) => self.request_full_redraw(),
            // Leave the rows dirty; the next change or resize retries.
            Err(_) => {}
        }
    }

    fn on_char(&mut self, unit: u16) {
        if std::mem::take(&mut self.swallow_char) {
            return;
        }
        let ch = match unit {
            0xD800..=0xDBFF => {
                self.high_surrogate = Some(unit);
                return;
            }
            0xDC00..=0xDFFF => {
                let Some(high) = self.high_surrogate.take() else {
                    return;
                };
                char::decode_utf16([high, unit]).next().and_then(|r| r.ok())
            }
            _ => char::from_u32(u32::from(unit)),
        };
        let Some(ch) = ch else {
            return;
        };
        let (_, old_y) = self.pane.screen.cursor();
        self.pane.screen.input(ch);
        let (_, new_y) = self.pane.screen.cursor();
        let grid = self.pane.screen.grid_mut();
        grid.mark_dirty(old_y);
        grid.mark_dirty(new_y);
        self.invalidate_dirty();
    }

    /// Maps a key to a window shortcut, if it is one.
    fn on_key_down(&mut self, vk: VIRTUAL_KEY) -> Option<Command> {
        // SAFETY: plain key-state queries.
        let (ctrl, shift) = unsafe {
            (
                GetKeyState(i32::from(VK_CONTROL.0)) < 0,
                GetKeyState(i32::from(VK_SHIFT.0)) < 0,
            )
        };
        let command = match vk {
            _ if !ctrl => None,
            VK_OEM_PLUS | VK_ADD => Some(Command::Id(cmd::ZOOM_IN)),
            VK_OEM_MINUS | VK_SUBTRACT => Some(Command::Id(cmd::ZOOM_OUT)),
            VIRTUAL_KEY(0x30) | VK_NUMPAD0 => Some(Command::Id(cmd::ZOOM_RESET)),
            VIRTUAL_KEY(0x4D) if shift => Some(Command::Id(cmd::HIDE_MENU)),
            _ => None,
        };
        self.swallow_char = command.is_some();
        command
    }

    /// Runs a menu command that needs no call back into the window.
    fn command(&mut self, id: u32) -> Option<Command> {
        match id {
            cmd::EXIT => return Some(Command::Exit),
            cmd::HIDE_MENU => return Some(Command::SetMenuVisible(!self.menu_visible)),
            cmd::ZOOM_IN => self.set_font(self.pane.font.zoomed(1)),
            cmd::ZOOM_OUT => self.set_font(self.pane.font.zoomed(-1)),
            cmd::ZOOM_RESET => {
                self.set_font(FontSpec::new(&self.pane.font.family, FontSpec::DEFAULT_PT));
            }
            id if (cmd::FONT..cmd::FONT + FONT_CHOICES.len() as u32).contains(&id) => {
                if let Some(family) = FONT_CHOICES.get((id - cmd::FONT) as usize) {
                    self.set_font(FontSpec::new(family, self.pane.font.size_pt));
                }
            }
            id if id >= cmd::THEME => {
                if let Some(theme) = Theme::builtin().into_iter().nth((id - cmd::THEME) as usize) {
                    self.pane.theme = theme;
                    self.update_menu_checks();
                    self.request_full_redraw();
                }
            }
            _ => {}
        }
        None
    }

    /// Changes the pane's font; the window keeps its size and the grid
    /// gains or loses cells.
    fn set_font(&mut self, font: FontSpec) {
        if let Ok(metrics) = self.renderer.metrics(&font, self.dpi) {
            self.pane.font = font;
            self.pane.metrics = metrics;
            self.update_menu_checks();
            self.layout();
        }
    }

    fn update_menu_checks(&mut self) {
        let font = FONT_CHOICES
            .iter()
            .position(|f| *f == self.pane.font.family)
            .map_or(u32::MAX, |i| cmd::FONT + i as u32);
        let theme = Theme::builtin()
            .iter()
            .position(|t| t.name == self.pane.theme.name)
            .map_or(u32::MAX, |i| cmd::THEME + i as u32);
        let last_font = cmd::FONT + FONT_CHOICES.len() as u32 - 1;
        let last_theme = cmd::THEME + Theme::builtin().len() as u32 - 1;
        // SAFETY: valid menu handle; ids are ours. A missing id just leaves
        // the group unchecked.
        unsafe {
            let _ = CheckMenuRadioItem(self.menu, cmd::FONT, last_font, font, MF_BYCOMMAND.0);
            let _ = CheckMenuRadioItem(self.menu, cmd::THEME, last_theme, theme, MF_BYCOMMAND.0);
        }
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        let (_, y) = self.pane.screen.cursor();
        self.pane.screen.grid_mut().mark_dirty(y);
        self.invalidate_dirty();
    }

    /// The DPI changed: reload fonts at the new size.
    fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.renderer.clear_font_cache();
        if let Ok(metrics) = self.renderer.metrics(&self.pane.font, dpi) {
            self.pane.metrics = metrics;
        }
        self.full_redraw = true;
    }

    /// The window size that keeps the current grid at `dpi`.
    fn size_for_dpi(&mut self, dpi: u32) -> Option<(i32, i32)> {
        let metrics = self.renderer.metrics(&self.pane.font, dpi).ok()?;
        let grid = self.pane.screen.grid();
        Some(window_size(
            &metrics,
            grid.cols(),
            grid.rows(),
            dpi,
            self.menu_visible,
        ))
    }
}

/// Follow-up actions that call into Win32 functions which send messages to
/// the window, so they run after the state borrow is released.
enum Command {
    Id(u32),
    Exit,
    SetMenuVisible(bool),
}

fn run_command(hwnd: HWND, command: Command) {
    match command {
        Command::Id(id) => {
            if let Some(next) = with_window(|w| w.command(id)).flatten() {
                run_command(hwnd, next);
            }
        }
        Command::Exit => {
            // SAFETY: our own window.
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Command::SetMenuVisible(visible) => {
            let Some(menu) = with_window(|w| {
                w.menu_visible = visible;
                w.menu
            }) else {
                return;
            };
            // SAFETY: our window and menu. SetMenu sends WM_SIZE, which
            // re-lays out the grid.
            let _ = unsafe { SetMenu(hwnd, visible.then_some(menu)) };
        }
    }
}

fn loword(v: usize) -> u32 {
    (v & 0xFFFF) as u32
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: called by Windows with valid arguments for this window; the
    // pointer casts below follow each message's documented lparam type.
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with_window(|w| w.paint(ps.rcPaint));
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                if wparam.0 as u32 != SIZE_MINIMIZED {
                    with_window(Window::layout);
                }
                LRESULT(0)
            }
            WM_GETDPISCALEDSIZE => {
                let size = lparam.0 as *mut SIZE;
                match with_window(|w| w.size_for_dpi(loword(wparam.0))).flatten() {
                    Some((cx, cy)) if !size.is_null() => {
                        *size = SIZE { cx, cy };
                        LRESULT(1)
                    }
                    _ => LRESULT(0),
                }
            }
            WM_DPICHANGED => {
                with_window(|w| w.set_dpi(loword(wparam.0)));
                let r = lparam.0 as *const RECT;
                if let Some(r) = r.as_ref() {
                    // Sends WM_SIZE, which lays out the grid at the new DPI.
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                LRESULT(0)
            }
            WM_CHAR => {
                with_window(|w| w.on_char(wparam.0 as u16));
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = VIRTUAL_KEY(wparam.0 as u16);
                match with_window(|w| w.on_key_down(vk)).flatten() {
                    Some(command) => {
                        run_command(hwnd, command);
                        LRESULT(0)
                    }
                    None => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
            }
            WM_SYSKEYUP if wparam.0 == usize::from(VK_MENU.0) => {
                // Alt alone brings a hidden menu bar back.
                if with_window(|w| !w.menu_visible) == Some(true) {
                    run_command(hwnd, Command::SetMenuVisible(true));
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_COMMAND => {
                run_command(hwnd, Command::Id(loword(wparam.0)));
                LRESULT(0)
            }
            WM_SETFOCUS => {
                with_window(|w| w.set_focus(true));
                LRESULT(0)
            }
            WM_KILLFOCUS => {
                with_window(|w| w.set_focus(false));
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
