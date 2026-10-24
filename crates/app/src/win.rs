//! The Win32 window, menu, input and message loop.

// Win32 calls are FFI; each unsafe block below is a direct, documented API call.
#![allow(unsafe_code)]

use std::cell::{Cell as StdCell, RefCell};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ferossh_render::{
    CellMetrics, Cursor, FontSpec, Frame, Grid, PaneView, PixelRect, Renderer, Theme,
};
use ferossh_term::{
    Action, Key, Mods, MouseButton, MouseEvent, SelectionKind, Settings, Terminal, check_link,
    clipboard_preview,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, LOGFONTW, PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_POINT, COMPOSITIONFORM, ImmGetContext, ImmReleaseContext,
    ImmSetCandidateWindow, ImmSetCompositionFontW, ImmSetCompositionWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_ADD, VK_BACK,
    VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F3, VK_F4, VK_F12, VK_HOME,
    VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT, VK_NUMPAD0, VK_OEM_2, VK_OEM_MINUS, VK_OEM_PLUS,
    VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_SUBTRACT, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CheckMenuItem, CheckMenuRadioItem,
    CreateMenu, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetClientRect, GetMessageW, HMENU,
    IDC_IBEAM, IDYES, KillTimer, LoadCursorW, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONQUESTION,
    MB_ICONWARNING, MB_OK, MB_YESNO, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MF_BYCOMMAND, MF_CHECKED,
    MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, MessageBoxW, PM_REMOVE, PeekMessageW,
    PostMessageW, PostQuitMessage, RegisterClassW, SIZE_MINIMIZED, SW_SHOW, SW_SHOWNORMAL,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetMenu, SetTimer, SetWindowPos, SetWindowTextW,
    ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_COMMAND,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_GETDPISCALEDSIZE, WM_IME_COMPOSITION,
    WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETFOCUS, WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
    WNDCLASSW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, Result, w};

use crate::clipboard;
use crate::conpty::{self, Output, Shell};
use crate::pattern;

const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;
/// Space between the window edge and the grid, in DIPs.
const PADDING_DIP: i32 = 4;
const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_APPWINDOW;
const APP_NAME: &str = "FeroSSH";

/// Posted by the shell's threads when output is waiting.
const WM_SHELL_OUTPUT: u32 = WM_APP + 1;
/// Fires when a synchronized update must be shown even though it has not
/// ended.
const TIMER_SYNC: usize = 1;
/// Output processed per message before the window gets a chance to paint.
const OUTPUT_BUDGET: usize = 1 << 20;
/// Wheel units (120 per notch) per scrolled line: three lines a notch.
const WHEEL_PER_LINE: i32 = 40;
/// Characters of host-supplied text shown in a confirmation prompt.
const PREVIEW_CHARS: usize = 400;

/// Font families offered in View > Font. A pane's font can be any
/// installed family; this list is only the menu.
const FONT_CHOICES: [&str; 3] = ["Cascadia Mono", "Consolas", "Courier New"];

/// Menu command ids. Shortcuts map to the same ids.
mod cmd {
    pub const RESTART_SHELL: u32 = 100;
    pub const EXIT: u32 = 101;
    pub const ZOOM_IN: u32 = 200;
    pub const ZOOM_OUT: u32 = 201;
    pub const ZOOM_RESET: u32 = 202;
    pub const HIDE_MENU: u32 = 203;
    pub const TEST_PATTERN: u32 = 204;
    /// Plus the index into `FONT_CHOICES`.
    pub const FONT: u32 = 300;
    /// Plus the index into `Theme::builtin()`.
    pub const THEME: u32 = 400;
    pub const COPY: u32 = 500;
    pub const PASTE: u32 = 501;
    pub const FIND: u32 = 502;
    pub const CLEAR_SCROLLBACK: u32 = 503;
    pub const COPY_ON_SELECT: u32 = 504;
    pub const RIGHT_CLICK_PASTE: u32 = 505;
    pub const SCROLL_PAGE_UP: u32 = 510;
    pub const SCROLL_PAGE_DOWN: u32 = 511;
    pub const SCROLL_TOP: u32 = 512;
    pub const SCROLL_BOTTOM: u32 = 513;
}

/// One terminal pane with its own font and colour theme. Milestone 2 has
/// exactly one; tabs and splits add more.
struct Pane {
    term: Terminal,
    /// What the renderer draws: the visible part of `term`.
    grid: Grid,
    cursor: Option<Cursor>,
    shell: Option<Shell>,
    /// The shell exited; Enter starts a new one.
    exited: bool,
    font: FontSpec,
    theme: Theme,
    metrics: CellMetrics,
}

/// Behaviour PuTTY users may want; both off by default.
#[derive(Default)]
struct Options {
    copy_on_select: bool,
    right_click_paste: bool,
}

#[derive(Default)]
struct Mouse {
    /// The button held while the host receives mouse reports.
    reporting: Option<MouseButton>,
    /// A left-button selection is being dragged.
    selecting: bool,
    /// The cell of the last motion report.
    last_cell: Option<(usize, usize)>,
    /// Time, cell and count of the last left click, for double and triple
    /// clicks.
    last_click: Option<(Instant, (usize, usize), u8)>,
    /// Wheel movement not yet worth a line.
    wheel_rest: i32,
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
    high_surrogate: Option<u16>,
    mouse: Mouse,
    options: Options,
    /// The command line the local shell runs.
    command: String,
    /// Set while a WM_SHELL_OUTPUT is queued, so the shell's threads post
    /// one message per batch rather than one per read.
    output_pending: Arc<AtomicBool>,
}

thread_local! {
    // The UI thread's one window. Handlers borrow it briefly and never hold
    // the borrow across a call that sends messages back to the window.
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
    // A confirmation dialog is open. Requests that would open another one
    // meanwhile are dropped.
    static DIALOG_OPEN: StdCell<bool> = const { StdCell::new(false) };
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

/// The command line after the program name, or the default shell.
fn shell_command() -> String {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.is_empty() {
        return conpty::default_command();
    }
    args.iter()
        .map(|a| {
            if a.is_empty() || a.contains([' ', '\t']) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
            hCursor: LoadCursorW(None, IDC_IBEAM)?,
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
        let theme = Theme::dark();
        let mut renderer = Renderer::new(hwnd, 1, 1)?;
        let metrics = renderer.metrics(&font, dpi)?;
        let (width, height) = window_size(&metrics, DEFAULT_COLS, DEFAULT_ROWS, dpi, true);
        let mut term = Terminal::new(DEFAULT_COLS, DEFAULT_ROWS, &Settings::default());
        term.set_theme(&theme);
        term.set_cell_size(metrics.width, metrics.height);
        WINDOW.with(|cell| {
            *cell.borrow_mut() = Some(Window {
                hwnd,
                renderer,
                dpi,
                client: (0, 0),
                pane: Pane {
                    term,
                    grid: Grid::new(DEFAULT_COLS, DEFAULT_ROWS),
                    cursor: None,
                    shell: None,
                    exited: false,
                    font,
                    theme,
                    metrics,
                },
                menu,
                menu_visible: true,
                focused: false,
                full_redraw: true,
                high_surrogate: None,
                mouse: Mouse::default(),
                options: Options::default(),
                command: shell_command(),
                output_pending: Arc::new(AtomicBool::new(false)),
            });
        });
        with_window(Window::update_menu_checks);
        // Sends WM_SIZE, which sizes the terminal before the shell starts.
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )?;
        with_window(Window::start_shell);
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
    let item = |menu: HMENU, id: u32, text: &str| {
        // SAFETY: valid menu handle and string.
        unsafe { AppendMenuW(menu, MF_STRING, id as usize, &HSTRING::from(text)) }
    };
    let separator = |menu: HMENU| {
        // SAFETY: valid menu handle.
        unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, None) }
    };
    // SAFETY: menu construction with valid handles.
    unsafe {
        let file = CreatePopupMenu()?;
        item(file, cmd::RESTART_SHELL, "&Restart local shell")?;
        separator(file)?;
        item(file, cmd::EXIT, "E&xit\tAlt+F4")?;

        let edit = CreatePopupMenu()?;
        item(edit, cmd::COPY, "&Copy\tCtrl+Shift+C")?;
        item(edit, cmd::PASTE, "&Paste\tCtrl+Shift+V")?;
        separator(edit)?;
        item(edit, cmd::FIND, "&Find in scrollback\tCtrl+Shift+F")?;
        item(edit, cmd::CLEAR_SCROLLBACK, "C&lear scrollback")?;
        separator(edit)?;
        item(edit, cmd::COPY_ON_SELECT, "Copy on &select")?;
        item(edit, cmd::RIGHT_CLICK_PASTE, "&Right-click pastes")?;

        let fonts = CreatePopupMenu()?;
        for (id, name) in (cmd::FONT..).zip(FONT_CHOICES) {
            item(fonts, id, name)?;
        }
        let themes = CreatePopupMenu()?;
        for (id, theme) in (cmd::THEME..).zip(Theme::builtin()) {
            item(themes, id, theme.name)?;
        }

        let view = CreatePopupMenu()?;
        item(view, cmd::ZOOM_IN, "&Larger text\tCtrl+=")?;
        item(view, cmd::ZOOM_OUT, "&Smaller text\tCtrl+-")?;
        item(view, cmd::ZOOM_RESET, "&Reset text size\tCtrl+0")?;
        separator(view)?;
        AppendMenuW(view, MF_POPUP, fonts.0 as usize, w!("&Font"))?;
        AppendMenuW(view, MF_POPUP, themes.0 as usize, w!("&Theme"))?;
        separator(view)?;
        item(view, cmd::TEST_PATTERN, "Show test &pattern")?;
        separator(view)?;
        item(
            view,
            cmd::HIDE_MENU,
            "&Hide menu bar (Alt shows it)\tCtrl+Shift+M",
        )?;

        let bar = CreateMenu()?;
        AppendMenuW(bar, MF_POPUP, file.0 as usize, w!("&File"))?;
        AppendMenuW(bar, MF_POPUP, edit.0 as usize, w!("&Edit"))?;
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

/// The modifier keys held now.
fn key_mods() -> Mods {
    // SAFETY: plain key-state queries.
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(i32::from(vk.0)) } < 0;
    Mods {
        shift: down(VK_SHIFT),
        alt: down(VK_MENU),
        ctrl: down(VK_CONTROL),
    }
}

/// Removes the WM_CHAR or WM_SYSCHAR that TranslateMessage queued for a
/// key this window already handled on key-down.
fn drop_pending_char(hwnd: HWND) {
    let mut msg = MSG::default();
    // SAFETY: our window and a local out-pointer.
    unsafe {
        let _ = PeekMessageW(&mut msg, Some(hwnd), WM_CHAR, WM_CHAR, PM_REMOVE);
        let _ = PeekMessageW(&mut msg, Some(hwnd), WM_SYSCHAR, WM_SYSCHAR, PM_REMOVE);
    }
}

/// The terminal key for a virtual key, if it is one that does not type
/// text.
fn terminal_key(vk: VIRTUAL_KEY) -> Option<Key> {
    Some(match vk {
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_INSERT => Key::Insert,
        VK_DELETE => Key::Delete,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_BACK => Key::Backspace,
        VK_TAB => Key::Tab,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        v if (VK_F1.0..=VK_F12.0).contains(&v.0) => Key::F((v.0 - VK_F1.0 + 1) as u8),
        _ => return None,
    })
}

fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16;
    (i32::from(x), i32::from(y))
}

/// What a key-down handler decided.
enum KeyOutcome {
    Handled,
    /// Not ours: let DefWindowProc and the queued WM_CHAR handle it.
    Default,
    Run(Command),
}

impl Window {
    fn start_shell(&mut self) {
        self.pane.shell = None;
        let pending = self.output_pending.clone();
        let hwnd = self.hwnd.0 as isize;
        let notify = move || {
            if !pending.swap(true, Ordering::AcqRel) {
                // SAFETY: posting to our window; if it is gone the post
                // fails harmlessly.
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(hwnd as *mut c_void)),
                        WM_SHELL_OUTPUT,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        };
        let (cols, rows) = (self.pane.term.cols(), self.pane.term.rows());
        match Shell::spawn(&self.command, cols, rows, notify) {
            Ok(shell) => {
                self.pane.shell = Some(shell);
                self.pane.exited = false;
            }
            Err(e) => {
                let text = format!(
                    "\x1b[0;31mCould not start {}: {}\x1b[0m\r\nPress Enter to try again.\r\n",
                    self.command,
                    e.message()
                );
                self.pane.term.feed(text.as_bytes());
                self.pane.exited = true;
            }
        }
        self.refresh();
    }

    /// Sends the host's answers and other non-user bytes.
    fn send(&self, bytes: Vec<u8>) {
        if let Some(shell) = &self.pane.shell
            && !self.pane.exited
        {
            shell.write(bytes);
        }
    }

    /// Sends what the user typed or pasted; brings the view to the bottom.
    fn input(&mut self, bytes: Vec<u8>) {
        if self.pane.exited {
            return;
        }
        if self.pane.term.display_offset() != 0 {
            self.pane.term.scroll_to_bottom();
            self.refresh();
        }
        self.send(bytes);
    }

    /// Reads waiting shell output into the terminal.
    fn on_output(&mut self) -> Vec<Command> {
        self.output_pending.store(false, Ordering::Release);
        let mut fed = 0;
        while let Some(out) = self.pane.shell.as_ref().and_then(Shell::try_read) {
            match out {
                Output::Data(data) => {
                    fed += data.len();
                    self.pane.term.feed(&data);
                }
                Output::Exited(code) => {
                    self.pane.exited = true;
                    let text = format!(
                        "\r\n\x1b[0;90m[Process exited with code {code}. Press Enter to start a new one.]\x1b[0m\r\n"
                    );
                    self.pane.term.feed(text.as_bytes());
                }
            }
            if fed >= OUTPUT_BUDGET {
                // Let the window paint, then carry on.
                if !self.output_pending.swap(true, Ordering::AcqRel) {
                    // SAFETY: posting to our own window.
                    unsafe {
                        let _ =
                            PostMessageW(Some(self.hwnd), WM_SHELL_OUTPUT, WPARAM(0), LPARAM(0));
                    }
                }
                break;
            }
        }
        self.after_feed()
    }

    /// Acts on what the terminal asked for and redraws.
    fn after_feed(&mut self) -> Vec<Command> {
        let mut commands = Vec::new();
        for action in self.pane.term.take_actions() {
            match action {
                Action::Reply(bytes) => self.send(bytes),
                Action::Title(title) => commands.push(Command::SetTitle(title)),
                Action::ClipboardRequest(text) => commands.push(Command::ClipboardWrite(text)),
                Action::Bell if !self.focused => commands.push(Command::Bell),
                Action::Bell => {}
            }
        }
        if let Some(deadline) = self.pane.term.sync_deadline() {
            let wait = deadline.saturating_duration_since(Instant::now());
            let ms = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX).max(1);
            // SAFETY: our window; replaces any earlier timer with this id.
            unsafe { SetTimer(Some(self.hwnd), TIMER_SYNC, ms, None) };
        }
        self.refresh();
        commands
    }

    fn on_sync_timer(&mut self) -> Vec<Command> {
        // SAFETY: our window and timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SYNC);
        }
        if self
            .pane
            .term
            .sync_deadline()
            .is_some_and(|d| d <= Instant::now() + Duration::from_millis(1))
        {
            self.pane.term.flush_sync();
        }
        self.after_feed()
    }

    /// Copies the terminal's visible state into the pane's grid and asks
    /// Windows to repaint the rows that changed.
    fn refresh(&mut self) {
        let old = self.pane.cursor;
        let pane = &mut self.pane;
        pane.cursor = pane.term.draw_into(&mut pane.grid, self.focused);
        if old != pane.cursor {
            for c in [old, pane.cursor].into_iter().flatten() {
                pane.grid.mark_dirty(c.y);
            }
        }
        self.invalidate_dirty();
    }

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
        let m = self.pane.metrics;
        let cols = ((w - 2 * pad).max(0) / m.width.max(1) as i32).max(1) as usize;
        let rows = ((h - 2 * pad).max(0) / m.height.max(1) as i32).max(1) as usize;
        self.pane.term.resize(cols, rows);
        self.pane.term.set_cell_size(m.width, m.height);
        if let Some(shell) = &self.pane.shell {
            shell.resize(self.pane.term.cols(), self.pane.term.rows());
        }
        self.refresh();
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
        let Some((first, last)) = self.pane.grid.dirty_span() else {
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
        let rows = self.pane.grid.rows() as i32;
        if update.top < pad || update.bottom > pad + rows * ch {
            self.full_redraw = true;
        } else {
            for y in (update.top - pad) / ch..=(update.bottom - 1 - pad) / ch {
                self.pane.grid.mark_dirty(y.max(0) as usize);
            }
        }

        let view = PaneView {
            grid: &self.pane.grid,
            font: &self.pane.font,
            theme: &self.pane.theme,
            rect: PixelRect {
                left: 0,
                top: 0,
                right: self.client.0,
                bottom: self.client.1,
            },
            origin: (pad, pad),
            cursor: self.pane.cursor,
            full: self.full_redraw,
        };
        match self.renderer.draw(&[view], self.dpi) {
            Ok(Frame::Presented) => {
                self.pane.grid.clear_dirty();
                self.full_redraw = false;
            }
            Ok(Frame::Lost) => self.request_full_redraw(),
            // Leave the rows dirty; the next change or resize retries.
            Err(_) => {}
        }
    }

    fn on_char(&mut self, unit: u16, alt: bool) {
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
        if self.pane.term.search_active() {
            if !ch.is_control() {
                let mut query = self.pane.term.search_query().unwrap_or_default().to_owned();
                query.push(ch);
                self.pane.term.search_set(&query);
                self.refresh();
            }
            return;
        }
        let bytes = self.pane.term.text(ch, alt);
        self.input(bytes);
    }

    /// Handles a key-down: window shortcuts first, then the search bar,
    /// then keys that do not type text. Text keys are left to the WM_CHAR
    /// that TranslateMessage made from the keyboard layout, which is what
    /// gets AltGr, dead keys and IME input right.
    fn on_key_down(&mut self, vk: VIRTUAL_KEY) -> KeyOutcome {
        let mods = key_mods();
        if mods.alt && vk == VK_F4 {
            return KeyOutcome::Default;
        }
        if let Some(id) = shortcut(vk, mods) {
            drop_pending_char(self.hwnd);
            return KeyOutcome::Run(Command::Id(id));
        }
        if self.pane.term.search_active() {
            return self.search_key(vk, mods);
        }
        if let Some(key) = terminal_key(vk) {
            drop_pending_char(self.hwnd);
            if self.pane.exited {
                if key == Key::Enter {
                    self.start_shell();
                }
                return KeyOutcome::Handled;
            }
            if let Some(bytes) = self.pane.term.key(key, mods) {
                self.input(bytes);
            }
            return KeyOutcome::Handled;
        }
        // Control codes that have no character on most layouts. AltGr
        // (Ctrl+Alt) is left alone.
        if mods.ctrl && !mods.alt {
            let code = match vk {
                VK_SPACE => Some(0x00),
                VIRTUAL_KEY(0x32) => Some(0x00), // Ctrl+2, as in xterm
                VIRTUAL_KEY(0x36) => Some(0x1E), // Ctrl+6
                VK_OEM_2 => Some(0x1F),          // Ctrl+/
                _ => None,
            };
            if let Some(code) = code {
                drop_pending_char(self.hwnd);
                self.input(vec![code]);
                return KeyOutcome::Handled;
            }
        }
        KeyOutcome::Default
    }

    fn search_key(&mut self, vk: VIRTUAL_KEY, mods: Mods) -> KeyOutcome {
        let term = &mut self.pane.term;
        match vk {
            VK_ESCAPE => term.search_close(),
            VK_RETURN | VK_F3 => term.search_next(!mods.shift),
            VK_BACK => {
                let mut query = term.search_query().unwrap_or_default().to_owned();
                query.pop();
                term.search_set(&query);
            }
            // Characters arrive as WM_CHAR.
            _ => return KeyOutcome::Default,
        }
        drop_pending_char(self.hwnd);
        self.refresh();
        KeyOutcome::Handled
    }

    /// The viewport cell under a client-area point, and whether the point
    /// is in the cell's right half.
    fn cell_at(&self, x: i32, y: i32) -> (usize, usize, bool) {
        let pad = padding(self.dpi);
        let m = &self.pane.metrics;
        let (cw, ch) = (m.width.max(1) as i32, m.height.max(1) as i32);
        let (rx, ry) = ((x - pad).max(0), (y - pad).max(0));
        let col = ((rx / cw) as usize).min(self.pane.term.cols() - 1);
        let row = ((ry / ch) as usize).min(self.pane.term.rows() - 1);
        (col, row, rx % cw >= cw / 2)
    }

    fn capture(&self, on: bool) {
        // SAFETY: our window.
        unsafe {
            if on {
                SetCapture(self.hwnd);
            } else {
                let _ = ReleaseCapture();
            }
        }
    }

    fn on_button(
        &mut self,
        button: MouseButton,
        down: bool,
        (x, y): (i32, i32),
        mods: Mods,
    ) -> Option<Command> {
        let (col, row, right_half) = self.cell_at(x, y);
        if !down {
            self.capture(false);
            if self.mouse.reporting == Some(button) {
                self.mouse.reporting = None;
                if let Some(bytes) =
                    self.pane
                        .term
                        .mouse(MouseEvent::Release(button), col, row, mods)
                {
                    self.send(bytes);
                }
                return None;
            }
            if button == MouseButton::Left
                && std::mem::take(&mut self.mouse.selecting)
                && self.options.copy_on_select
                && self.pane.term.has_selection()
            {
                return Some(Command::Id(cmd::COPY));
            }
            return None;
        }

        self.capture(true);
        // Shift-click always selects, even when the host wants the mouse.
        if self.pane.term.mouse_reporting() && !mods.shift {
            if let Some(bytes) = self
                .pane
                .term
                .mouse(MouseEvent::Press(button), col, row, mods)
            {
                self.send(bytes);
            }
            self.mouse.reporting = Some(button);
            self.mouse.last_cell = Some((col, row));
            return None;
        }
        match button {
            MouseButton::Left => {
                if mods.ctrl
                    && let Some(link) = self.pane.term.link_at(col, row)
                {
                    return Some(Command::OpenLink(link));
                }
                // SAFETY: plain query.
                let double = Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }));
                let now = Instant::now();
                let count = match self.mouse.last_click {
                    Some((at, cell, n)) if now - at <= double && cell == (col, row) => n % 3 + 1,
                    _ => 1,
                };
                self.mouse.last_click = Some((now, (col, row), count));
                let kind = match count {
                    2 => SelectionKind::Word,
                    3 => SelectionKind::Line,
                    _ if mods.alt => SelectionKind::Block,
                    _ => SelectionKind::Simple,
                };
                self.pane.term.select_start(col, row, right_half, kind);
                self.mouse.selecting = true;
                self.refresh();
            }
            MouseButton::Right if self.options.right_click_paste => {
                return Some(Command::Id(cmd::PASTE));
            }
            _ => {}
        }
        None
    }

    fn on_mouse_move(&mut self, (x, y): (i32, i32), mods: Mods) {
        let (col, row, right_half) = self.cell_at(x, y);
        if self.pane.term.mouse_reporting() && self.mouse.last_cell != Some((col, row)) {
            self.mouse.last_cell = Some((col, row));
            let event = MouseEvent::Motion(self.mouse.reporting);
            if let Some(bytes) = self.pane.term.mouse(event, col, row, mods) {
                self.send(bytes);
            }
        }
        if self.mouse.selecting {
            // Dragging past the top or bottom edge scrolls the history.
            let pad = padding(self.dpi);
            let bottom = pad + self.pane.term.rows() as i32 * self.pane.metrics.height as i32;
            if y < pad {
                self.pane.term.scroll_lines(1);
            } else if y >= bottom {
                self.pane.term.scroll_lines(-1);
            }
            self.pane.term.select_update(col, row, right_half);
            self.refresh();
        }
    }

    fn on_wheel(&mut self, delta: i16, screen: (i32, i32), mods: Mods) -> Option<Command> {
        if mods.ctrl {
            let id = if delta > 0 {
                cmd::ZOOM_IN
            } else {
                cmd::ZOOM_OUT
            };
            return Some(Command::Id(id));
        }
        self.mouse.wheel_rest += i32::from(delta);
        let lines = self.mouse.wheel_rest / WHEEL_PER_LINE;
        if lines == 0 {
            return None;
        }
        self.mouse.wheel_rest -= lines * WHEEL_PER_LINE;
        let mut pt = POINT {
            x: screen.0,
            y: screen.1,
        };
        // SAFETY: our window and a local point.
        let _ = unsafe { ScreenToClient(self.hwnd, &mut pt) };
        let (col, row, _) = self.cell_at(pt.x, pt.y);
        if let Some(bytes) = self.pane.term.wheel(lines, col, row, mods) {
            self.send(bytes);
        }
        self.refresh();
        None
    }

    /// Puts the IME's composition and candidate windows at the cursor, in
    /// the pane's font.
    fn place_ime(&self) {
        let Some(cursor) = self.pane.cursor else {
            return;
        };
        let pad = padding(self.dpi);
        let m = &self.pane.metrics;
        let x = pad + cursor.x as i32 * m.width as i32;
        let y = pad + cursor.y as i32 * m.height as i32;
        let mut font = LOGFONTW {
            lfHeight: -(m.em_px.round() as i32),
            ..Default::default()
        };
        for (dst, src) in font
            .lfFaceName
            .iter_mut()
            .take(31)
            .zip(self.pane.font.family.encode_utf16())
        {
            *dst = src;
        }
        // SAFETY: our window's input context, released below; the structs
        // are valid locals.
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_invalid() {
                return;
            }
            let composition = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT { x, y },
                rcArea: RECT::default(),
            };
            let _ = ImmSetCompositionWindow(himc, &composition);
            let candidate = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_CANDIDATEPOS,
                ptCurrentPos: POINT {
                    x,
                    y: y + m.height as i32,
                },
                rcArea: RECT::default(),
            };
            let _ = ImmSetCandidateWindow(himc, &candidate);
            let _ = ImmSetCompositionFontW(himc, &font);
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
    }

    /// Runs a menu command that needs no call back into the window.
    fn command(&mut self, id: u32) -> Option<Command> {
        match id {
            cmd::EXIT => return Some(Command::Exit),
            cmd::HIDE_MENU => return Some(Command::SetMenuVisible(!self.menu_visible)),
            cmd::COPY => return Some(Command::Copy),
            cmd::PASTE => return Some(Command::Paste),
            cmd::RESTART_SHELL => self.start_shell(),
            cmd::ZOOM_IN => self.set_font(self.pane.font.zoomed(1)),
            cmd::ZOOM_OUT => self.set_font(self.pane.font.zoomed(-1)),
            cmd::ZOOM_RESET => {
                self.set_font(FontSpec::new(&self.pane.font.family, FontSpec::DEFAULT_PT));
            }
            cmd::TEST_PATTERN => {
                self.pane.term.scroll_to_bottom();
                self.pane.term.feed(pattern::test_pattern().as_bytes());
                // The pattern asks for nothing, so there are no commands.
                let _ = self.after_feed();
            }
            cmd::FIND => {
                self.pane.term.search_open();
                self.refresh();
            }
            cmd::CLEAR_SCROLLBACK => {
                self.pane.term.clear_scrollback();
                self.refresh();
            }
            cmd::COPY_ON_SELECT => {
                self.options.copy_on_select = !self.options.copy_on_select;
                self.update_menu_checks();
            }
            cmd::RIGHT_CLICK_PASTE => {
                self.options.right_click_paste = !self.options.right_click_paste;
                self.update_menu_checks();
            }
            cmd::SCROLL_PAGE_UP | cmd::SCROLL_PAGE_DOWN | cmd::SCROLL_TOP | cmd::SCROLL_BOTTOM => {
                let term = &mut self.pane.term;
                match id {
                    cmd::SCROLL_PAGE_UP => term.scroll_pages(1),
                    cmd::SCROLL_PAGE_DOWN => term.scroll_pages(-1),
                    cmd::SCROLL_TOP => term.scroll_to_top(),
                    _ => term.scroll_to_bottom(),
                }
                self.refresh();
            }
            id if (cmd::FONT..cmd::FONT + FONT_CHOICES.len() as u32).contains(&id) => {
                if let Some(family) = FONT_CHOICES.get((id - cmd::FONT) as usize) {
                    self.set_font(FontSpec::new(family, self.pane.font.size_pt));
                }
            }
            id if (cmd::THEME..cmd::THEME + Theme::builtin().len() as u32).contains(&id) => {
                if let Some(theme) = Theme::builtin().into_iter().nth((id - cmd::THEME) as usize) {
                    self.pane.term.set_theme(&theme);
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
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        // SAFETY: valid menu handle; ids are ours. A missing id just leaves
        // the group unchecked.
        unsafe {
            let _ = CheckMenuRadioItem(self.menu, cmd::FONT, last_font, font, MF_BYCOMMAND.0);
            let _ = CheckMenuRadioItem(self.menu, cmd::THEME, last_theme, theme, MF_BYCOMMAND.0);
            CheckMenuItem(
                self.menu,
                cmd::COPY_ON_SELECT,
                (MF_BYCOMMAND | check(self.options.copy_on_select)).0,
            );
            CheckMenuItem(
                self.menu,
                cmd::RIGHT_CLICK_PASTE,
                (MF_BYCOMMAND | check(self.options.right_click_paste)).0,
            );
        }
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(bytes) = self.pane.term.focus(focused) {
            self.send(bytes);
        }
        if !focused {
            self.mouse.selecting = false;
            self.mouse.reporting = None;
        }
        // The cursor changes shape; its row must redraw even if nothing
        // else changed.
        if let Some(c) = self.pane.cursor {
            self.pane.grid.mark_dirty(c.y);
        }
        self.refresh();
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
        Some(window_size(
            &metrics,
            self.pane.term.cols(),
            self.pane.term.rows(),
            dpi,
            self.menu_visible,
        ))
    }
}

/// The window shortcut for a key, as a command id.
fn shortcut(vk: VIRTUAL_KEY, m: Mods) -> Option<u32> {
    let key = |c: u8| VIRTUAL_KEY(u16::from(c));
    if m.ctrl && !m.alt {
        return match vk {
            VK_OEM_PLUS | VK_ADD => Some(cmd::ZOOM_IN),
            VK_OEM_MINUS | VK_SUBTRACT if !m.shift => Some(cmd::ZOOM_OUT),
            VK_NUMPAD0 if !m.shift => Some(cmd::ZOOM_RESET),
            v if v == key(b'0') && !m.shift => Some(cmd::ZOOM_RESET),
            v if v == key(b'M') && m.shift => Some(cmd::HIDE_MENU),
            v if v == key(b'C') && m.shift => Some(cmd::COPY),
            v if v == key(b'V') && m.shift => Some(cmd::PASTE),
            v if v == key(b'F') && m.shift => Some(cmd::FIND),
            VK_INSERT if !m.shift => Some(cmd::COPY),
            VK_HOME if m.shift => Some(cmd::SCROLL_TOP),
            VK_END if m.shift => Some(cmd::SCROLL_BOTTOM),
            _ => None,
        };
    }
    if m.shift && !m.ctrl && !m.alt {
        return match vk {
            VK_PRIOR => Some(cmd::SCROLL_PAGE_UP),
            VK_NEXT => Some(cmd::SCROLL_PAGE_DOWN),
            VK_INSERT => Some(cmd::PASTE),
            _ => None,
        };
    }
    None
}

/// Follow-up actions that call into Win32 functions which send messages to
/// the window or open dialogs, so they run after the state borrow is
/// released.
enum Command {
    Id(u32),
    Exit,
    SetMenuVisible(bool),
    Copy,
    Paste,
    /// The host asked to put text on the clipboard.
    ClipboardWrite(String),
    /// Ctrl+click on an OSC 8 link; the target is unchecked.
    OpenLink(String),
    SetTitle(Option<String>),
    Bell,
}

/// A modal message box; `None` if another one is already open.
fn dialog(hwnd: HWND, text: &str, style: MESSAGEBOX_STYLE) -> Option<MESSAGEBOX_RESULT> {
    if DIALOG_OPEN.get() {
        return None;
    }
    DIALOG_OPEN.set(true);
    // SAFETY: our window and valid strings. The box runs a modal loop that
    // dispatches messages back to the window; no state borrow is held.
    let result = unsafe { MessageBoxW(Some(hwnd), &HSTRING::from(text), w!("FeroSSH"), style) };
    DIALOG_OPEN.set(false);
    Some(result)
}

fn run_commands(hwnd: HWND, commands: Vec<Command>) {
    for command in commands {
        run_command(hwnd, command);
    }
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
        Command::Copy => {
            if let Some(text) = with_window(|w| w.pane.term.selection_text()).flatten() {
                clipboard::set_text(hwnd, &text);
            }
        }
        Command::Paste => {
            let Some(text) = clipboard::get_text(hwnd) else {
                return;
            };
            let Some(paste) = with_window(|w| w.pane.term.paste(&text)) else {
                return;
            };
            if paste.needs_confirmation() {
                let lines = paste.line_breaks;
                let text = format!(
                    "The text you are pasting contains {lines} line break{s}. Each one acts as \
                     Enter, so a shell runs every line as soon as it arrives.\n\n{}\n\nPaste it?",
                    clipboard_preview(&text, PREVIEW_CHARS),
                    s = if lines == 1 { "" } else { "s" },
                );
                let answer = dialog(hwnd, &text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2);
                if answer != Some(IDYES) {
                    return;
                }
            }
            with_window(|w| w.input(paste.bytes));
        }
        Command::ClipboardWrite(text) => {
            let prompt = format!(
                "The remote program wants to copy {} characters to your clipboard:\n\n{}\n\nAllow it?",
                text.chars().count(),
                clipboard_preview(&text, PREVIEW_CHARS)
            );
            // While another prompt is open, further requests are refused.
            if dialog(hwnd, &prompt, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == Some(IDYES) {
                clipboard::set_text(hwnd, &text);
            }
        }
        Command::OpenLink(raw) => match check_link(&raw) {
            Some(uri) => {
                let prompt = format!("Open this link in your browser?\n\n{uri}");
                if dialog(hwnd, &prompt, MB_YESNO | MB_ICONQUESTION) == Some(IDYES) {
                    // SAFETY: a checked http(s)/mailto URL handed to the
                    // default handler.
                    unsafe {
                        ShellExecuteW(
                            Some(hwnd),
                            w!("open"),
                            &HSTRING::from(uri),
                            None,
                            None,
                            SW_SHOWNORMAL,
                        );
                    }
                }
            }
            None => {
                let text = format!(
                    "FeroSSH does not open this link. Only http, https and mailto links \
                     without spaces or hidden characters can be opened.\n\n{}",
                    clipboard_preview(&raw, PREVIEW_CHARS)
                );
                dialog(hwnd, &text, MB_OK | MB_ICONWARNING);
            }
        },
        Command::SetTitle(title) => {
            let text = match title.as_deref() {
                Some(t) if !t.is_empty() => format!("{t} - {APP_NAME}"),
                _ => APP_NAME.to_owned(),
            };
            // SAFETY: our window and a valid string.
            let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(text)) };
        }
        Command::Bell => {
            let info = FLASHWINFO {
                cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            };
            // SAFETY: valid struct for our window.
            let _ = unsafe { FlashWindowEx(&info) };
        }
    }
}

fn loword(v: usize) -> u32 {
    (v & 0xFFFF) as u32
}

/// Modifiers for a mouse message: Shift and Ctrl from its flags, Alt from
/// the key state.
fn mouse_mods(wparam: WPARAM) -> Mods {
    const MK_SHIFT: usize = 0x4;
    const MK_CONTROL: usize = 0x8;
    Mods {
        shift: wparam.0 & MK_SHIFT != 0,
        ctrl: wparam.0 & MK_CONTROL != 0,
        alt: key_mods().alt,
    }
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
            WM_SHELL_OUTPUT => {
                if let Some(commands) = with_window(Window::on_output) {
                    run_commands(hwnd, commands);
                }
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_SYNC => {
                if let Some(commands) = with_window(Window::on_sync_timer) {
                    run_commands(hwnd, commands);
                }
                LRESULT(0)
            }
            WM_CHAR => {
                with_window(|w| w.on_char(wparam.0 as u16, false));
                LRESULT(0)
            }
            // Alt+Space opens the system menu as usual.
            WM_SYSCHAR if wparam.0 != usize::from(b' ') => {
                with_window(|w| w.on_char(wparam.0 as u16, true));
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = VIRTUAL_KEY(wparam.0 as u16);
                match with_window(|w| w.on_key_down(vk)) {
                    Some(KeyOutcome::Handled) => LRESULT(0),
                    Some(KeyOutcome::Run(command)) => {
                        run_command(hwnd, command);
                        LRESULT(0)
                    }
                    Some(KeyOutcome::Default) | None => DefWindowProcW(hwnd, msg, wparam, lparam),
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
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
            | WM_MBUTTONUP => {
                let (button, down) = match msg {
                    WM_LBUTTONDOWN => (MouseButton::Left, true),
                    WM_LBUTTONUP => (MouseButton::Left, false),
                    WM_RBUTTONDOWN => (MouseButton::Right, true),
                    WM_RBUTTONUP => (MouseButton::Right, false),
                    WM_MBUTTONDOWN => (MouseButton::Middle, true),
                    _ => (MouseButton::Middle, false),
                };
                let at = lparam_point(lparam);
                let mods = mouse_mods(wparam);
                if let Some(command) =
                    with_window(|w| w.on_button(button, down, at, mods)).flatten()
                {
                    run_command(hwnd, command);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let at = lparam_point(lparam);
                let mods = mouse_mods(wparam);
                with_window(|w| w.on_mouse_move(at, mods));
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
                let at = lparam_point(lparam);
                let mods = mouse_mods(wparam);
                if let Some(command) = with_window(|w| w.on_wheel(delta, at, mods)).flatten() {
                    run_command(hwnd, command);
                }
                LRESULT(0)
            }
            WM_IME_STARTCOMPOSITION | WM_IME_COMPOSITION => {
                with_window(|w| w.place_ime());
                // The default handling turns the result string into WM_CHAR.
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
                // Dropping the window closes the shell's console.
                let window =
                    WINDOW.with(|cell| cell.try_borrow_mut().ok().and_then(|mut w| w.take()));
                drop(window);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
