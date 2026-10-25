//! The Win32 window, menu, input and message loop.
//!
//! The window holds tabs, each a tree of split panes (see [`crate::layout`]).
//! A window opens with one tab and one pane; the tab strip appears with the
//! second tab, and the side pane (the session manager) only when asked for.
//! Each pane has its own terminal, host, log, font and colour theme.

// Win32 calls are FFI; each unsafe block below is a direct, documented API call.
#![allow(unsafe_code)]

use std::cell::{Cell as StdCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ferossh_auth::Broker;
use ferossh_config::{Settings, putty, ssh_config};
use ferossh_ipc::{ChannelId, Message};
use ferossh_render::{
    CellMetrics, Cursor, FontSpec, Frame, Grid, PaneView, PixelRect, Renderer, Rgb, Theme,
};
use ferossh_term::{
    Action, Key, LogWriter, Mods, MouseButton, MouseEvent, SelectionKind, Settings as TermSettings,
    Terminal, check_link, clipboard_preview, log_file_name,
};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, LOGFONTW, PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
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
    FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetClientRect, GetCursorPos,
    GetMessageW, HCURSOR, HMENU, HTCLIENT, IDC_ARROW, IDC_IBEAM, IDC_SIZEALL, IDC_SIZENS,
    IDC_SIZEWE, IDYES, KillTimer, LoadCursorW, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION,
    MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE,
    MF_BYCOMMAND, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, MessageBoxW,
    PM_REMOVE, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW, SIZE_MINIMIZED,
    SW_SHOW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetCursor, SetMenu, SetTimer,
    SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETDPISCALEDSIZE, WM_IME_COMPOSITION, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_SIZE, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::Launch;
use crate::clipboard;
use crate::conpty::{self, Output, Shell};
use crate::layout::{self, Axis, Closed, Dir, Drop, PaneId, Rect, Workspace};
use crate::link::{Link, LinkEvent};
use crate::look;
use crate::pattern;
use crate::putty_reg;
use crate::sandbox;
use crate::sessions::{HostItem, SessionList, SideAction, SideKey};
use crate::ssh::{Effect, Env, SshSession, State, Target};
use crate::tabstrip::{self, Health, Hit, Palette, StripLayout, TabLabel};

const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;
/// Space between a pane's edge and its grid, in DIPs.
const PADDING_DIP: i32 = 4;
/// Space around the tab strip's and side pane's text, in DIPs.
const CHROME_PADDING_DIP: i32 = 3;
/// Extra pixels either side of a divider that still grab it, in DIPs.
const GRAB_DIP: i32 = 3;
/// The side pane's width when first shown, in chrome cells.
const SIDE_COLS: i32 = 34;
/// The tab strip and side pane font size.
const CHROME_PT: f32 = 9.5;
/// How far Alt+Shift+arrow moves a divider.
const RESIZE_STEP: f32 = 0.05;
/// Pixels the mouse moves before a press becomes a drag.
const DRAG_START_PX: i32 = 6;
const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_APPWINDOW;
const APP_NAME: &str = "FeroSSH";

/// Posted by the shell's, worker link's and broker's threads when
/// something is waiting.
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
    pub const CONNECT: u32 = 102;
    pub const RECONNECT: u32 = 103;
    pub const DISCONNECT: u32 = 104;
    pub const NEW_TAB: u32 = 105;
    pub const CLOSE_PANE: u32 = 106;
    pub const IMPORT_PUTTY: u32 = 107;
    pub const ZOOM_IN: u32 = 200;
    pub const ZOOM_OUT: u32 = 201;
    pub const ZOOM_RESET: u32 = 202;
    pub const HIDE_MENU: u32 = 203;
    pub const TEST_PATTERN: u32 = 204;
    pub const SESSIONS: u32 = 205;
    pub const SIDE_RIGHT: u32 = 206;
    pub const TAB_STRIP: u32 = 207;
    pub const SAVE_LOOK: u32 = 208;
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
    pub const LOG_TOGGLE: u32 = 600;
    pub const LOG_RAW: u32 = 601;
    pub const LOG_FOLDER: u32 = 602;
    pub const REPLACE_KEY: u32 = 603;
    pub const SPLIT_RIGHT: u32 = 700;
    pub const SPLIT_DOWN: u32 = 701;
    pub const NEXT_TAB: u32 = 702;
    pub const PREV_TAB: u32 = 703;
    pub const TAB_LEFT: u32 = 704;
    pub const TAB_RIGHT: u32 = 705;
    pub const PANE_TO_TAB: u32 = 706;
    pub const JOIN_TAB: u32 = 707;
    /// Plus 0-3: left, right, up, down.
    pub const FOCUS: u32 = 710;
    /// Plus 0-3: left, right, up, down.
    pub const RESIZE: u32 = 720;
}

const DIRS: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

/// The prompt shown when no host was given.
const CONNECT_PROMPT: &str =
    "\x1b[0mConnect to (user@host[:port], or a Host from ~/.ssh/config; Ctrl+Shift+S lists them): ";

/// What a pane is connected to.
enum Host {
    /// Asking for a destination; the text typed so far.
    Ask(String),
    /// A local shell on ConPTY (`--local`); `exited` once it ended, when
    /// Enter starts a new one.
    Local {
        shell: Option<Shell>,
        exited: bool,
    },
    Ssh(Box<SshSession>),
}

/// The worker process and the pipes to it.
struct WorkerConn {
    link: Link,
    _process: sandbox::Worker,
}

/// Session logging for a pane: on or off, plain or raw, and the open file.
#[derive(Default)]
struct Logging {
    enabled: bool,
    raw: bool,
    writer: Option<LogWriter<File>>,
    path: Option<PathBuf>,
}

/// One terminal pane with its own font and colour theme.
struct Pane {
    id: PaneId,
    term: Terminal,
    /// What the renderer draws: the visible part of `term`.
    grid: Grid,
    cursor: Option<Cursor>,
    host: Host,
    log: Logging,
    font: FontSpec,
    theme: Theme,
    metrics: CellMetrics,
    /// The pane's area in the client, padding included.
    rect: Rect,
    /// The title the host set (OSC 0/2).
    title: Option<String>,
    /// A bell rang while the pane was out of sight.
    bell: bool,
}

impl Pane {
    /// Top-left corner of cell (0, 0).
    fn origin(&self, dpi: u32) -> (i32, i32) {
        let pad = padding(dpi);
        (self.rect.x + pad, self.rect.y + pad)
    }

    /// What the tab strip and window title call this pane.
    fn label(&self) -> String {
        if let Some(t) = self.title.as_deref().filter(|t| !t.is_empty()) {
            return t.to_owned();
        }
        match &self.host {
            Host::Ssh(s) => s.target.label(),
            Host::Local { .. } => "local".to_owned(),
            Host::Ask(_) => "new tab".to_owned(),
        }
    }

    fn health(&self) -> Health {
        match &self.host {
            Host::Ssh(s) => match s.state() {
                State::Connected => Health::Connected,
                State::Connecting => Health::Connecting,
                State::Blocked | State::Ended => Health::Idle,
            },
            _ => Health::Idle,
        }
    }

    fn connected(&self) -> bool {
        matches!(&self.host, Host::Ssh(s) if s.state() == State::Connected)
    }
}

#[derive(Default)]
struct Mouse {
    /// The pane that got the button press and gets the motion until it is
    /// released.
    pane: Option<PaneId>,
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
    /// The last point seen in the client, for the mouse cursor shape.
    at: (i32, i32),
}

/// What a mouse drag moves.
enum Drag {
    /// A split divider in the active tab.
    Divider {
        path: Vec<bool>,
        axis: Axis,
        span: Rect,
    },
    /// The side pane's edge.
    SideEdge,
    /// A tab in the strip. Dragged below the strip (`out`), the window
    /// shows the tab with pane `home` instead, and dropping on one of its
    /// panes merges the dragged tab in there.
    Tab {
        index: usize,
        home: Option<PaneId>,
        out: bool,
    },
    /// A pane (Ctrl+Shift+drag): `start` is where the press was; `active`
    /// once it moved far enough.
    Pane {
        id: PaneId,
        start: (i32, i32),
        active: bool,
    },
}

/// The side pane: the session manager.
struct Side {
    visible: bool,
    /// Has the keyboard.
    focused: bool,
    list: SessionList,
    grid: Grid,
    /// Width in pixels, edge excluded; 0 until first shown.
    width: i32,
    /// Last click time and row, for double clicks.
    last_click: Option<(Instant, usize)>,
}

/// The parts of the client area.
#[derive(Clone, Copy, Debug, Default)]
struct Areas {
    side: Option<Rect>,
    side_edge: Option<Rect>,
    strip: Option<Rect>,
    panes: Rect,
}

struct Window {
    hwnd: HWND,
    renderer: Renderer,
    dpi: u32,
    client: (i32, i32),
    ws: Workspace,
    panes: HashMap<PaneId, Pane>,
    areas: Areas,
    strip_grid: Grid,
    strip_layout: StripLayout,
    side: Side,
    chrome_font: FontSpec,
    chrome_metrics: CellMetrics,
    menu: HMENU,
    menu_visible: bool,
    focused: bool,
    /// Repaint everything on the next WM_PAINT, not only dirty rows.
    full_redraw: bool,
    high_surrogate: Option<u16>,
    mouse: Mouse,
    drag: Option<Drag>,
    settings: Settings,
    /// The theme panes start with (`--theme`, or Nord).
    default_theme: Theme,
    /// The command line the local shell runs.
    command: String,
    /// Run the worker in the AppContainer (off only with `--no-sandbox`).
    sandbox: bool,
    worker: Option<WorkerConn>,
    broker: Option<Broker>,
    /// The channel of each job handed to the broker, oldest first. The
    /// broker answers in order, so each reply goes to the front one.
    broker_jobs: VecDeque<ChannelId>,
    /// The channel id of the next SSH shell.
    next_channel: u32,
    /// Set while a WM_SHELL_OUTPUT is queued, so the background threads
    /// post one message per batch rather than one per read.
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

pub fn run(launch: Launch, theme: Option<String>) -> Result<()> {
    let result = create_and_run(launch, theme);
    if let Err(e) = &result {
        let text = HSTRING::from(format!("FeroSSH could not start:\n\n{}", e.message()));
        // SAFETY: plain message box with valid strings.
        unsafe { MessageBoxW(None, &text, w!("FeroSSH"), MB_OK | MB_ICONERROR) };
    }
    result
}

/// A command line FeroSSH does not understand.
pub fn run_error(problem: &str) -> Result<()> {
    let text = HSTRING::from(format!(
        "{problem}\n\nUsage:\n  ferossh [--theme NAME] [-l user] [-p port] [--no-sandbox] [user@]host[:port]\n  \
         ferossh [--theme NAME] --local [command]"
    ));
    // SAFETY: plain message box with valid strings.
    unsafe { MessageBoxW(None, &text, w!("FeroSSH"), MB_OK | MB_ICONERROR) };
    Ok(())
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// FeroSSH's own folder for settings: `%APPDATA%\FeroSSH`.
fn app_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("FeroSSH")
}

fn settings_path() -> PathBuf {
    app_dir().join("ferossh.conf")
}

/// Where imported PuTTY sessions go, in ssh_config syntax. FeroSSH reads it
/// after `~/.ssh/config`; OpenSSH never sees it.
fn putty_hosts_path() -> PathBuf {
    app_dir().join("putty-sessions.conf")
}

/// The user's home folder, account name, ssh_config and imported hosts.
fn ssh_env() -> Env {
    let home = home();
    let mut config = std::fs::read_to_string(home.join(".ssh").join("config")).unwrap_or_default();
    if let Ok(imported) = std::fs::read_to_string(putty_hosts_path()) {
        // A Host line first: the user's last block must not swallow it.
        config.push_str("\nHost !*\n");
        config.push_str(&imported);
    }
    Env {
        local_user: std::env::var("USERNAME").unwrap_or_default(),
        home,
        config,
    }
}

fn load_settings() -> Settings {
    std::fs::read_to_string(settings_path())
        .map(|t| Settings::parse(&t))
        .unwrap_or_default()
}

/// Writes a file in FeroSSH's folder through a temporary file, so a crash
/// never leaves half a file.
fn write_app_file(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The hosts the session manager lists.
fn host_items(env: &Env) -> Vec<HostItem> {
    let includes = ssh_config::DirIncludes {
        home: env.home.clone(),
    };
    ssh_config::hosts(&env.config, &includes)
        .into_iter()
        .map(|h| {
            let detail = Target::resolve(&h.name, env)
                .map(|t| {
                    let mut d = format!("{}@{}", t.user, t.hostname);
                    if t.port != 22 {
                        d.push_str(&format!(":{}", t.port));
                    }
                    d
                })
                .unwrap_or_default();
            HostItem {
                name: h.name,
                folder: h.folder,
                detail,
            }
        })
        .collect()
}

/// Where tab logs go: `%LOCALAPPDATA%\FeroSSH\logs`.
fn log_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("FeroSSH")
        .join("logs")
}

/// The local time as (year, month, day, hour, minute, second).
fn local_time() -> (u16, u8, u8, u8, u8, u8) {
    // SAFETY: plain query.
    let t = unsafe { GetLocalTime() };
    let b = |v: u16| u8::try_from(v).unwrap_or(0);
    (
        t.wYear,
        b(t.wMonth),
        b(t.wDay),
        b(t.wHour),
        b(t.wMinute),
        b(t.wSecond),
    )
}

fn timestamp() -> String {
    let (y, mo, d, h, mi, s) = local_time();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

fn term_settings(lines: Option<usize>) -> TermSettings {
    lines.map_or_else(TermSettings::default, TermSettings::new)
}

fn create_and_run(launch: Launch, theme: Option<String>) -> Result<()> {
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
        let settings = load_settings();
        let font = FontSpec::default();
        // An unknown theme name falls back to the default (Nord).
        let default_theme = theme.and_then(|n| Theme::by_name(&n)).unwrap_or_default();
        let mut renderer = Renderer::new(hwnd, 1, 1)?;
        let metrics = renderer.metrics(&font, dpi)?;
        let chrome_font = FontSpec::new(&font.family, CHROME_PT);
        let chrome_metrics = renderer.metrics(&chrome_font, dpi)?;
        let (width, height) = window_size(&metrics, DEFAULT_COLS, DEFAULT_ROWS, dpi, true);
        let (ws, first) = Workspace::new();
        let pane = new_pane(
            first,
            font,
            default_theme.clone(),
            metrics,
            settings.scrollback,
        );
        WINDOW.with(|cell| {
            *cell.borrow_mut() = Some(Window {
                hwnd,
                renderer,
                dpi,
                client: (0, 0),
                ws,
                panes: HashMap::from([(first, pane)]),
                areas: Areas::default(),
                strip_grid: Grid::new(1, 1),
                strip_layout: StripLayout::default(),
                side: Side {
                    visible: false,
                    focused: false,
                    list: SessionList::default(),
                    grid: Grid::new(1, 1),
                    width: 0,
                    last_click: None,
                },
                chrome_font,
                chrome_metrics,
                menu,
                menu_visible: true,
                focused: false,
                full_redraw: true,
                high_surrogate: None,
                mouse: Mouse::default(),
                drag: None,
                settings,
                default_theme,
                command: match &launch {
                    Launch::Local(c) if !c.is_empty() => c.clone(),
                    _ => conpty::default_command(),
                },
                sandbox: !matches!(launch, Launch::Ssh { sandbox: false, .. }),
                worker: None,
                broker: None,
                broker_jobs: VecDeque::new(),
                next_channel: 1,
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
        match launch {
            Launch::Local(_) => with_window(|w| w.start_shell(first)),
            Launch::Ssh {
                destination: Some(d),
                ..
            } => with_window(|w| w.connect(first, &d)),
            _ => with_window(|w| w.ask_destination(first)),
        };
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn new_pane(
    id: PaneId,
    font: FontSpec,
    theme: Theme,
    metrics: CellMetrics,
    scrollback: Option<usize>,
) -> Pane {
    let mut term = Terminal::new(DEFAULT_COLS, DEFAULT_ROWS, &term_settings(scrollback));
    term.set_theme(&theme);
    term.set_cell_size(metrics.width, metrics.height);
    Pane {
        id,
        term,
        grid: Grid::new(DEFAULT_COLS, DEFAULT_ROWS),
        cursor: None,
        host: Host::Ask(String::new()),
        log: Logging::default(),
        font,
        theme,
        metrics,
        rect: Rect::default(),
        title: None,
        bell: false,
    }
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
        item(file, cmd::NEW_TAB, "&New tab\tCtrl+Shift+T")?;
        item(file, cmd::CONNECT, "&Connect to...\tCtrl+Shift+N")?;
        item(file, cmd::RECONNECT, "&Reconnect\tCtrl+Shift+R")?;
        item(file, cmd::DISCONNECT, "&Disconnect")?;
        item(file, cmd::CLOSE_PANE, "Close &pane\tCtrl+Shift+W")?;
        separator(file)?;
        item(file, cmd::RESTART_SHELL, "Open &local shell")?;
        item(file, cmd::IMPORT_PUTTY, "&Import PuTTY sessions...")?;
        separator(file)?;
        item(file, cmd::EXIT, "E&xit\tAlt+F4")?;

        let session = CreatePopupMenu()?;
        item(session, cmd::LOG_TOGGLE, "&Log to file")?;
        item(session, cmd::LOG_RAW, "Log &raw output (keep escape codes)")?;
        item(session, cmd::LOG_FOLDER, "Open log &folder")?;
        separator(session)?;
        item(
            session,
            cmd::SAVE_LOOK,
            "&Save font and theme for this host",
        )?;
        separator(session)?;
        item(session, cmd::REPLACE_KEY, "Replace host &key...")?;

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
            // Group the warning themes and the Windows classics.
            if matches!(theme.name, "Production" | "Campbell") {
                separator(themes)?;
            }
            item(themes, id, theme.name)?;
        }

        let view = CreatePopupMenu()?;
        item(view, cmd::SESSIONS, "S&ession manager\tCtrl+Shift+S")?;
        item(view, cmd::SIDE_RIGHT, "Side pane on the &right")?;
        item(view, cmd::TAB_STRIP, "Ta&b strip")?;
        separator(view)?;
        item(view, cmd::ZOOM_IN, "&Larger text\tCtrl+=")?;
        item(view, cmd::ZOOM_OUT, "&Smaller text\tCtrl+-")?;
        item(view, cmd::ZOOM_RESET, "&Reset text size\tCtrl+0")?;
        separator(view)?;
        AppendMenuW(view, MF_POPUP, fonts.0 as usize, w!("&Font (this pane)"))?;
        AppendMenuW(view, MF_POPUP, themes.0 as usize, w!("&Theme (this pane)"))?;
        separator(view)?;
        item(view, cmd::TEST_PATTERN, "Show test &pattern")?;
        separator(view)?;
        item(
            view,
            cmd::HIDE_MENU,
            "&Hide menu bar (Alt shows it)\tCtrl+Shift+M",
        )?;

        let window = CreatePopupMenu()?;
        item(window, cmd::SPLIT_RIGHT, "Split &right\tAlt+Shift+=")?;
        item(window, cmd::SPLIT_DOWN, "Split &down\tAlt+Shift+-")?;
        item(window, cmd::PANE_TO_TAB, "Move pane to its own &tab")?;
        item(
            window,
            cmd::JOIN_TAB,
            "&Join tab into the tab before it\tCtrl+Shift+J",
        )?;
        separator(window)?;
        item(window, cmd::NEXT_TAB, "&Next tab\tCtrl+Tab")?;
        item(window, cmd::PREV_TAB, "&Previous tab\tCtrl+Shift+Tab")?;
        item(window, cmd::TAB_LEFT, "Move tab &left\tCtrl+Shift+PgUp")?;
        item(window, cmd::TAB_RIGHT, "Move tab ri&ght\tCtrl+Shift+PgDn")?;
        separator(window)?;
        let names = ["left", "right", "up", "down"];
        let keys = ["Left", "Right", "Up", "Down"];
        for (i, (name, key)) in names.iter().zip(keys).enumerate() {
            item(
                window,
                cmd::FOCUS + i as u32,
                &format!("Focus pane {name}\tAlt+{key}"),
            )?;
        }
        separator(window)?;
        for (i, (name, key)) in names.iter().zip(keys).enumerate() {
            item(
                window,
                cmd::RESIZE + i as u32,
                &format!("Grow pane {name}\tAlt+Shift+{key}"),
            )?;
        }

        let bar = CreateMenu()?;
        AppendMenuW(bar, MF_POPUP, file.0 as usize, w!("&File"))?;
        AppendMenuW(bar, MF_POPUP, edit.0 as usize, w!("&Edit"))?;
        AppendMenuW(bar, MF_POPUP, view.0 as usize, w!("&View"))?;
        AppendMenuW(bar, MF_POPUP, window.0 as usize, w!("&Window"))?;
        AppendMenuW(bar, MF_POPUP, session.0 as usize, w!("&Session"))?;
        Ok(bar)
    }
}

fn io_error(e: std::io::Error) -> windows::core::Error {
    windows::core::Error::new(E_FAIL, e.to_string())
}

fn scale(dip: i32, dpi: u32) -> i32 {
    dip * dpi as i32 / 96
}

fn padding(dpi: u32) -> i32 {
    scale(PADDING_DIP, dpi)
}

/// Pixels between split panes.
fn gap(dpi: u32) -> i32 {
    scale(1, dpi).max(1)
}

/// Outer window size for a grid of `cols` x `rows` cells.
fn window_size(m: &CellMetrics, cols: usize, rows: usize, dpi: u32, menu: bool) -> (i32, i32) {
    let pad = padding(dpi);
    client_to_window(
        cols as i32 * m.width as i32 + 2 * pad,
        rows as i32 * m.height as i32 + 2 * pad,
        dpi,
        menu,
    )
}

fn client_to_window(w: i32, h: i32, dpi: u32, menu: bool) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    // SAFETY: valid pointer to a local rect. On failure the client size is
    // used as is, which only makes the window slightly small.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut r, STYLE, menu, EX_STYLE, dpi) };
    (r.right - r.left, r.bottom - r.top)
}

fn pixel(r: Rect) -> PixelRect {
    PixelRect {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

/// `r` grown by `by` pixels on every side.
fn inflate(r: Rect, by: i32) -> Rect {
    Rect::new(r.x - by, r.y - by, r.w + 2 * by, r.h + 2 * by)
}

fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) * (1.0 - amount) + f32::from(y) * amount).round() as u8;
    Rgb::new(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// The four thin rects that outline `r`, `t` pixels thick.
fn frame(r: Rect, t: i32, colour: Rgb, out: &mut Vec<(PixelRect, Rgb)>) {
    for e in [
        Rect::new(r.x, r.y, r.w, t),
        Rect::new(r.x, r.bottom() - t, r.w, t),
        Rect::new(r.x, r.y, t, r.h),
        Rect::new(r.right() - t, r.y, t, r.h),
    ] {
        out.push((pixel(e), colour));
    }
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

/// The side pane key for a virtual key.
fn side_key(vk: VIRTUAL_KEY, mods: Mods) -> Option<SideKey> {
    Some(match vk {
        VK_UP => SideKey::Up,
        VK_DOWN => SideKey::Down,
        VK_PRIOR => SideKey::PageUp,
        VK_NEXT => SideKey::PageDown,
        VK_HOME => SideKey::Home,
        VK_END => SideKey::End,
        VK_LEFT => SideKey::Left,
        VK_RIGHT => SideKey::Right,
        VK_RETURN => SideKey::Enter { split: mods.shift },
        VK_ESCAPE => SideKey::Escape,
        VK_BACK => SideKey::Backspace,
        v if mods.ctrl && !mods.alt && v == VIRTUAL_KEY(u16::from(b'D')) => {
            SideKey::ToggleFavourite
        }
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

/// Where a point in the client falls.
enum Spot {
    Side(usize),
    SideEdge,
    Strip(usize),
    Divider(layout::Divider),
    Pane(PaneId),
    Nothing,
}

fn clamp16(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

impl Window {
    /// Wakes the window from a background thread: one WM_SHELL_OUTPUT per
    /// batch.
    fn notifier(&self) -> impl Fn() + Send + Clone + 'static {
        let pending = self.output_pending.clone();
        let hwnd = self.hwnd.0 as isize;
        move || {
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
        }
    }

    fn focused_id(&self) -> PaneId {
        self.ws.focused()
    }

    /// The panes of the active tab, in layout order.
    fn visible_ids(&self) -> Vec<PaneId> {
        self.ws.tab().map(|t| t.root.panes()).unwrap_or_default()
    }

    fn is_visible(&self, id: PaneId) -> bool {
        self.ws.tab().is_some_and(|t| t.root.contains(id))
    }

    /// Whether `id` shows a focused cursor.
    fn has_keyboard(&self, id: PaneId) -> bool {
        self.focused && !self.side.focused && id == self.focused_id()
    }

    fn pane_of_channel(&self, channel: ChannelId) -> Option<PaneId> {
        self.panes.values().find_map(|p| match &p.host {
            Host::Ssh(s) if s.channel == channel => Some(p.id),
            _ => None,
        })
    }

    /// Feeds local text (not from the host) to a pane's terminal.
    fn show(&mut self, id: PaneId, text: &str) {
        if let Some(p) = self.panes.get_mut(&id) {
            p.term.feed(text.as_bytes());
        }
    }

    /// Ends whatever the pane is connected to.
    fn end_host(&mut self, id: PaneId) {
        let effects = match self.panes.get_mut(&id).map(|p| &mut p.host) {
            Some(Host::Ssh(session)) => session.close(),
            _ => Vec::new(),
        };
        self.apply(id, effects);
        self.stop_log(id);
        if let Some(p) = self.panes.get_mut(&id) {
            p.host = Host::Ask(String::new());
            p.title = None;
        }
    }

    fn start_shell(&mut self, id: PaneId) {
        self.end_host(id);
        let notify = self.notifier();
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let (cols, rows) = (p.term.cols(), p.term.rows());
        match Shell::spawn(&self.command, cols, rows, notify) {
            Ok(shell) => {
                p.host = Host::Local {
                    shell: Some(shell),
                    exited: false,
                };
                self.start_log(id);
            }
            Err(e) => {
                let text = format!(
                    "\x1b[0;31mCould not start {}: {}\x1b[0m\r\nPress Enter to try again.\r\n",
                    self.command,
                    e.message()
                );
                p.host = Host::Local {
                    shell: None,
                    exited: true,
                };
                self.show(id, &text);
            }
        }
        self.update_title();
        self.refresh(id);
    }

    /// Shows the connect prompt.
    fn ask_destination(&mut self, id: PaneId) {
        self.end_host(id);
        if let Some(p) = self.panes.get_mut(&id) {
            p.term.scroll_to_bottom();
        }
        self.show(id, &format!("\r\n{CONNECT_PROMPT}"));
        self.update_title();
        self.refresh_chrome();
        self.refresh(id);
    }

    fn set_title_text(&self, text: &str) {
        // SAFETY: our window and a valid string.
        let _ = unsafe { SetWindowTextW(self.hwnd, &HSTRING::from(text)) };
    }

    /// The window title follows the focused pane.
    fn update_title(&self) {
        let label = self.panes.get(&self.focused_id()).map(Pane::label);
        match label {
            Some(l) if l != "new tab" => self.set_title_text(&format!("{l} - {APP_NAME}")),
            _ => self.set_title_text(APP_NAME),
        }
    }

    /// Starts the worker if it is not running.
    fn ensure_worker(&mut self, id: PaneId) -> bool {
        if self.worker.is_some() {
            return true;
        }
        let result = sandbox::spawn_worker(self.sandbox).and_then(|w| {
            let input = w.from_worker.try_clone().map_err(io_error)?;
            let output = w.to_worker.try_clone().map_err(io_error)?;
            let link = Link::start(input, output, self.notifier()).map_err(io_error)?;
            Ok(WorkerConn { link, _process: w })
        });
        match result {
            Ok(conn) => {
                self.worker = Some(conn);
                true
            }
            Err(e) => {
                let text = format!(
                    "\x1b[0;31mCould not start the sandboxed SSH worker: {}\x1b[0m\r\n\
                     If this Windows does not allow AppContainers, start FeroSSH with \
                     --no-sandbox.\r\n",
                    e.message()
                );
                self.show(id, &text);
                false
            }
        }
    }

    fn ensure_broker(&mut self, id: PaneId) -> bool {
        if self.broker.is_some() {
            return true;
        }
        match Broker::spawn(self.notifier()) {
            Ok(b) => {
                self.broker = Some(b);
                true
            }
            Err(e) => {
                self.show(
                    id,
                    &format!("\x1b[0;31mCould not start the signing broker: {e}\x1b[0m\r\n"),
                );
                false
            }
        }
    }

    /// Connects pane `id` to `destination` (`[user@]host[:port]` or a
    /// Host from ssh_config).
    fn connect(&mut self, id: PaneId, destination: &str) {
        let env = ssh_env();
        let Some(target) = Target::resolve(destination, &env) else {
            let text = format!(
                "\x1b[0;31m\"{}\" is not a host name.\x1b[0m\r\n",
                crate::ssh::sanitize(destination)
            );
            self.end_host(id);
            self.show(id, &text);
            self.ask_destination(id);
            return;
        };
        self.connect_to(id, target, &env);
    }

    /// Connects pane `id` to `target`. When another pane is logged in to
    /// the same host, port and user, the new shell is a channel on that
    /// connection: no second login.
    fn connect_to(&mut self, id: PaneId, target: Target, env: &Env) {
        self.end_host(id);
        if let Some(p) = self.panes.get_mut(&id) {
            p.term.scroll_to_bottom();
        }
        if !self.ensure_worker(id) || !self.ensure_broker(id) {
            self.refresh(id);
            return;
        }
        if !self.sandbox {
            self.show(
                id,
                "\x1b[0;31mThe SSH worker runs without its sandbox (--no-sandbox).\x1b[0m\r\n",
            );
        }
        self.apply_look(id, &target);
        let shared = self.panes.values().find_map(|p| match &p.host {
            Host::Ssh(s) if p.id != id && s.can_share(&target) => Some(s.connection),
            _ => None,
        });
        let channel = self.next_channel;
        self.next_channel = self.next_channel.wrapping_add(1).max(1);
        let mut session = SshSession::new(channel, target, env.known_hosts());
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let (cols, rows) = (clamp16(p.term.cols()), clamp16(p.term.rows()));
        let effects = match shared {
            Some(connection) => session.start_on(connection, cols, rows),
            None => session.start(cols, rows),
        };
        p.host = Host::Ssh(Box::new(session));
        self.apply(id, effects);
        self.update_title();
        self.refresh_chrome();
        self.refresh(id);
    }

    /// Gives a pane the font, size, theme and scrollback its host's
    /// profile and the theme rules pick.
    fn apply_look(&mut self, id: PaneId, target: &Target) {
        let (font, theme) = look::for_host(
            &self.settings,
            &target.alias,
            &target.hostname,
            target.tag.as_deref(),
            &FontSpec::default(),
            &self.default_theme,
        );
        let scrollback = self
            .settings
            .profile(&target.alias)
            .and_then(|p| p.scrollback)
            .or(self.settings.scrollback);
        if let Some(p) = self.panes.get_mut(&id) {
            p.term.set_settings(&term_settings(scrollback));
        }
        self.set_theme(id, theme);
        self.set_font(id, font);
    }

    /// Connects again to the pane's host, or asks for one.
    fn reconnect(&mut self, id: PaneId) {
        let target = match self.panes.get(&id).map(|p| &p.host) {
            Some(Host::Ssh(s)) => Some(s.target.clone()),
            Some(Host::Local { .. }) => {
                self.start_shell(id);
                return;
            }
            _ => None,
        };
        if let Some(target) = target {
            self.connect_to(id, target, &ssh_env());
        }
    }

    /// Carries out what a pane's SSH session decided.
    fn apply(&mut self, id: PaneId, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Send(msg) => {
                    if let Some(w) = &self.worker {
                        w.link.send(msg);
                    }
                }
                Effect::Show(text) => self.show(id, &text),
                Effect::Output(bytes) => {
                    if let Some(p) = self.panes.get_mut(&id) {
                        p.term.feed(&bytes);
                    }
                    self.log(id, &bytes);
                }
                Effect::Broker(job) => {
                    let channel = match self.panes.get(&id).map(|p| &p.host) {
                        Some(Host::Ssh(s)) => s.channel,
                        _ => 0,
                    };
                    if let Some(b) = &self.broker {
                        self.broker_jobs.push_back(channel);
                        b.submit(job);
                    }
                }
                Effect::Connected => {
                    self.start_log(id);
                    self.refresh_chrome();
                }
                Effect::Ended => {
                    self.stop_log(id);
                    self.refresh_chrome();
                }
            }
        }
    }

    /// Opens the pane's log if logging is on and the pane has a host.
    fn start_log(&mut self, id: PaneId) {
        let Some(p) = self.panes.get(&id) else {
            return;
        };
        if !p.log.enabled || p.log.writer.is_some() {
            return;
        }
        let label = match &p.host {
            Host::Ssh(s) if s.state() == State::Connected => s.target.label(),
            Host::Local { exited: false, .. } => "local".to_owned(),
            _ => return,
        };
        let raw = p.log.raw;
        let host = label.rsplit('@').next().unwrap_or(&label).to_owned();
        let dir = log_dir();
        let base = log_file_name(&host, id as usize, local_time());
        let opened = std::fs::create_dir_all(&dir).and_then(|()| {
            // Never overwrite: a second log in the same minute gets a number.
            let mut n = 1;
            loop {
                let name = if n == 1 {
                    base.clone()
                } else {
                    base.replace(".log", &format!("-{n}.log"))
                };
                let path = dir.join(name);
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(f) => return Ok((f, path)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
                    Err(e) => return Err(e),
                }
            }
        });
        let header = format!("FeroSSH log of {label}, started {}", timestamp());
        match opened.and_then(|(f, path)| Ok((LogWriter::new(f, raw, &header)?, path))) {
            Ok((writer, path)) => {
                self.show(
                    id,
                    &format!("\x1b[0;90mLogging to {}\x1b[0m\r\n", path.display()),
                );
                if let Some(p) = self.panes.get_mut(&id) {
                    p.log.writer = Some(writer);
                    p.log.path = Some(path);
                }
            }
            Err(e) => {
                self.show(
                    id,
                    &format!(
                        "\x1b[0;31mCould not open a log file in {}: {e}\x1b[0m\r\n",
                        dir.display()
                    ),
                );
                if let Some(p) = self.panes.get_mut(&id) {
                    p.log.enabled = false;
                }
                self.update_menu_checks();
            }
        }
    }

    fn stop_log(&mut self, id: PaneId) {
        if let Some(writer) = self.panes.get_mut(&id).and_then(|p| p.log.writer.take()) {
            let _ = writer.finish(&format!("Log ended {}", timestamp()));
        }
    }

    fn log(&mut self, id: PaneId, bytes: &[u8]) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let Some(writer) = &mut p.log.writer else {
            return;
        };
        if let Err(e) = writer.write(bytes) {
            p.log.writer = None;
            p.log.enabled = false;
            self.show(
                id,
                &format!("\r\n\x1b[0;31mLogging stopped: {e}\x1b[0m\r\n"),
            );
            self.update_menu_checks();
        }
    }

    /// Sends the terminal's answers and other non-user bytes to the host.
    /// They never go into a local prompt.
    fn send(&mut self, id: PaneId, bytes: Vec<u8>) {
        let effects = match self.panes.get_mut(&id).map(|p| &mut p.host) {
            Some(Host::Local {
                shell: Some(shell),
                exited: false,
            }) => {
                shell.write(bytes);
                return;
            }
            Some(Host::Ssh(session)) if !session.prompt_active() => session.input(&bytes),
            _ => return,
        };
        self.apply(id, effects);
    }

    /// Sends what the user typed or pasted; brings the view to the bottom.
    /// Local prompts, the connect prompt and "Press Enter to reconnect"
    /// take it first.
    fn input(&mut self, id: PaneId, bytes: Vec<u8>) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        if p.term.display_offset() != 0 {
            p.term.scroll_to_bottom();
            self.refresh(id);
        }
        let enter = matches!(bytes.as_slice(), b"\r" | b"\r\n" | b"\n");
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        match &mut p.host {
            Host::Local { exited: true, .. } => {
                if enter {
                    self.start_shell(id);
                }
            }
            Host::Local { .. } => self.send(id, bytes),
            Host::Ask(text) => {
                if enter {
                    let destination = std::mem::take(text);
                    self.show(id, "\r\n");
                    if destination.trim().is_empty() {
                        self.show(id, CONNECT_PROMPT);
                    } else {
                        self.connect(id, destination.trim());
                    }
                } else if let [0x7f] | [0x08] = bytes.as_slice() {
                    if text.pop().is_some() {
                        self.show(id, "\x08 \x08");
                    }
                } else if !bytes.starts_with(&[0x1b]) {
                    let typed: String = String::from_utf8_lossy(&bytes)
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect();
                    text.push_str(&typed);
                    self.show(id, &typed);
                }
                self.refresh(id);
            }
            Host::Ssh(session) if session.prompt_active() => {
                let effects = session.prompt_input(&bytes);
                self.apply(id, effects);
                self.refresh(id);
            }
            Host::Ssh(session) if session.state() == State::Ended => {
                if enter {
                    self.reconnect(id);
                }
            }
            Host::Ssh(_) => self.send(id, bytes),
        }
    }

    /// Pastes into a local prompt; `false` if no prompt is open and the
    /// paste goes to the host.
    fn paste_local(&mut self, id: PaneId, text: &str) -> bool {
        let Some(p) = self.panes.get_mut(&id) else {
            return true;
        };
        match &mut p.host {
            Host::Ssh(session) if session.prompt_active() => {
                let effects = session.prompt_paste(text);
                self.apply(id, effects);
            }
            Host::Ask(typed) => {
                let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                typed.push_str(&clean);
                self.show(id, &clean);
            }
            _ => return false,
        }
        self.refresh(id);
        true
    }

    /// Reads what the shells, the worker and the broker have queued.
    fn on_output(&mut self) -> Vec<Command> {
        self.output_pending.store(false, Ordering::Release);
        let mut fed = 0;
        let mut more = false;
        // The local shells.
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in ids {
            while let Some(Pane {
                host:
                    Host::Local {
                        shell: Some(shell),
                        exited: false,
                    },
                ..
            }) = self.panes.get_mut(&id)
            {
                let Some(out) = shell.try_read() else {
                    break;
                };
                match out {
                    Output::Data(data) => {
                        fed += data.len();
                        if let Some(p) = self.panes.get_mut(&id) {
                            p.term.feed(&data);
                        }
                        self.log(id, &data);
                    }
                    Output::Exited(code) => {
                        self.stop_log(id);
                        if let Some(p) = self.panes.get_mut(&id) {
                            p.host = Host::Local {
                                shell: None,
                                exited: true,
                            };
                        }
                        let text = format!(
                            "\r\n\x1b[0;90m[Process exited with code {code}. Press Enter to start a new one.]\x1b[0m\r\n"
                        );
                        self.show(id, &text);
                    }
                }
                if fed >= OUTPUT_BUDGET {
                    more = true;
                    break;
                }
            }
        }
        // The signing broker answers in the order it was asked.
        while let Some(reply) = self.broker.as_ref().and_then(Broker::try_recv) {
            let channel = self.broker_jobs.pop_front().unwrap_or(0);
            let Some(id) = self.pane_of_channel(channel) else {
                continue;
            };
            let effects = match self.panes.get_mut(&id).map(|p| &mut p.host) {
                Some(Host::Ssh(session)) => session.on_broker(reply),
                _ => Vec::new(),
            };
            self.apply(id, effects);
        }
        // The worker.
        while !more {
            let Some(event) = self.worker.as_ref().and_then(|w| w.link.try_recv()) else {
                break;
            };
            match event {
                LinkEvent::Message(msg) => {
                    if let Message::Data { bytes, .. } = &msg {
                        fed += bytes.len();
                    }
                    let Some(id) = msg.channel().and_then(|c| self.pane_of_channel(c)) else {
                        continue;
                    };
                    let effects = match self.panes.get_mut(&id).map(|p| &mut p.host) {
                        Some(Host::Ssh(session)) => session.on_worker(msg),
                        _ => Vec::new(),
                    };
                    self.apply(id, effects);
                }
                LinkEvent::Closed(why) => {
                    self.worker = None;
                    self.broker_jobs.clear();
                    let ids: Vec<PaneId> = self.panes.keys().copied().collect();
                    for id in ids {
                        let effects = match self.panes.get_mut(&id).map(|p| &mut p.host) {
                            Some(Host::Ssh(session))
                                if matches!(
                                    session.state(),
                                    State::Connecting | State::Connected
                                ) =>
                            {
                                session.on_worker(Message::Disconnected {
                                    channel: session.channel,
                                    reason: why.clone(),
                                })
                            }
                            _ => Vec::new(),
                        };
                        self.apply(id, effects);
                    }
                }
            }
            more = fed >= OUTPUT_BUDGET;
        }
        if more && !self.output_pending.swap(true, Ordering::AcqRel) {
            // Let the window paint, then carry on.
            // SAFETY: posting to our own window.
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_SHELL_OUTPUT, WPARAM(0), LPARAM(0));
            }
        }
        self.after_feed()
    }

    /// Acts on what the terminals asked for and redraws.
    fn after_feed(&mut self) -> Vec<Command> {
        let mut commands = Vec::new();
        let mut deadline: Option<Instant> = None;
        let focused = self.focused_id();
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        let mut chrome = false;
        for id in ids {
            let visible = self.is_visible(id);
            let Some(p) = self.panes.get_mut(&id) else {
                continue;
            };
            let actions = p.term.take_actions();
            if let Some(d) = p.term.sync_deadline() {
                deadline = Some(deadline.map_or(d, |old| old.min(d)));
            }
            for action in actions {
                match action {
                    Action::Reply(bytes) => self.send(id, bytes),
                    Action::Title(title) => {
                        if let Some(p) = self.panes.get_mut(&id) {
                            p.title = title;
                        }
                        if id == focused {
                            commands.push(Command::UpdateTitle);
                        }
                        chrome = true;
                    }
                    Action::ClipboardRequest(text) => commands.push(Command::ClipboardWrite(text)),
                    Action::Bell => {
                        if !visible && let Some(p) = self.panes.get_mut(&id) {
                            p.bell = true;
                            chrome = true;
                        }
                        if !self.focused {
                            commands.push(Command::Bell);
                        }
                    }
                }
            }
        }
        if let Some(deadline) = deadline {
            let wait = deadline.saturating_duration_since(Instant::now());
            let ms = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX).max(1);
            // SAFETY: our window; replaces any earlier timer with this id.
            unsafe { SetTimer(Some(self.hwnd), TIMER_SYNC, ms, None) };
        }
        if chrome {
            self.refresh_chrome();
        }
        self.refresh_visible();
        commands
    }

    fn on_sync_timer(&mut self) -> Vec<Command> {
        // SAFETY: our window and timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SYNC);
        }
        let soon = Instant::now() + Duration::from_millis(1);
        for p in self.panes.values_mut() {
            if p.term.sync_deadline().is_some_and(|d| d <= soon) {
                p.term.flush_sync();
            }
        }
        self.after_feed()
    }

    /// Copies a visible pane's terminal into its grid and asks Windows to
    /// repaint the rows that changed.
    fn refresh(&mut self, id: PaneId) {
        if !self.is_visible(id) {
            return;
        }
        let keyboard = self.has_keyboard(id);
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let old = pane.cursor;
        pane.cursor = pane.term.draw_into(&mut pane.grid, keyboard);
        if old != pane.cursor {
            for c in [old, pane.cursor].into_iter().flatten() {
                pane.grid.mark_dirty(c.y);
            }
        }
        self.invalidate_dirty(id);
    }

    fn refresh_visible(&mut self) {
        for id in self.visible_ids() {
            self.refresh(id);
        }
    }

    /// The colours of the tab strip and side pane: the focused pane's.
    fn chrome_palette(&self) -> Palette {
        let theme = self
            .panes
            .get(&self.focused_id())
            .map_or_else(|| self.default_theme.clone(), |p| p.theme.clone());
        Palette::of(&theme)
    }

    /// A theme whose background is the chrome's, for drawing its padding.
    fn chrome_theme(&self) -> Theme {
        let mut t = self
            .panes
            .get(&self.focused_id())
            .map_or_else(|| self.default_theme.clone(), |p| p.theme.clone());
        t.background = Palette::of(&t).strip;
        t
    }

    /// Redraws the tab strip and side pane grids.
    fn refresh_chrome(&mut self) {
        let palette = self.chrome_palette();
        if let Some(strip) = self.areas.strip {
            let active = self.ws.active;
            let labels: Vec<TabLabel> = self
                .ws
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let pane = self.panes.get(&t.focus);
                    let bell = i != active
                        && t.root
                            .panes()
                            .iter()
                            .any(|id| self.panes.get(id).is_some_and(|p| p.bell));
                    TabLabel {
                        title: pane.map(Pane::label).unwrap_or_default(),
                        health: pane.map_or(Health::Idle, Pane::health),
                        bell,
                    }
                })
                .collect();
            self.strip_layout = tabstrip::draw(&mut self.strip_grid, &labels, active, &palette);
            self.invalidate_grid(strip, false);
        }
        if let Some(side) = self.areas.side {
            let focused = self.side.focused && self.focused;
            self.side.list.draw(&mut self.side.grid, &palette, focused);
            self.invalidate_grid(side, true);
        }
    }

    /// Invalidates the dirty rows of a chrome grid in `area`.
    fn invalidate_grid(&self, area: Rect, side: bool) {
        let grid = if side {
            &self.side.grid
        } else {
            &self.strip_grid
        };
        let Some((first, last)) = grid.dirty_span() else {
            return;
        };
        let pad = scale(CHROME_PADDING_DIP, self.dpi);
        let ch = self.chrome_metrics.height as i32;
        let r = RECT {
            left: area.x,
            top: area.y + pad + first as i32 * ch,
            right: area.right(),
            bottom: area.y + pad + (last as i32 + 1) * ch,
        };
        // SAFETY: valid window and rect.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&r), false);
        }
    }

    /// Splits the client area into side pane, tab strip and panes.
    fn compute_areas(&mut self) -> Areas {
        let (w, h) = self.client;
        let g = gap(self.dpi);
        let cpad = scale(CHROME_PADDING_DIP, self.dpi);
        let cm = self.chrome_metrics;
        let mut main = Rect::new(0, 0, w, h);
        let mut areas = Areas::default();
        if self.side.visible {
            if self.side.width <= 0 {
                self.side.width = SIDE_COLS * cm.width as i32 + 2 * cpad;
            }
            let width = self.side.width.clamp(80, (w / 2).max(80)).min(w - g);
            self.side.width = width;
            if self.settings.side_pane_right {
                areas.side = Some(Rect::new(w - width, 0, width, h));
                areas.side_edge = Some(Rect::new(w - width - g, 0, g, h));
                main = Rect::new(0, 0, (w - width - g).max(0), h);
            } else {
                areas.side = Some(Rect::new(0, 0, width, h));
                areas.side_edge = Some(Rect::new(width, 0, g, h));
                main = Rect::new(width + g, 0, (w - width - g).max(0), h);
            }
        }
        if self.settings.tab_strip && self.ws.tabs.len() >= 2 {
            let sh = cm.height as i32 + 2 * cpad;
            areas.strip = Some(Rect::new(main.x, main.y, main.w, sh));
            main = Rect::new(main.x, main.y + sh, main.w, (main.h - sh).max(0));
        }
        areas.panes = main;
        areas
    }

    /// Fits every pane, and the chrome, to the client area.
    fn layout(&mut self) {
        let mut r = RECT::default();
        // SAFETY: valid window and out-pointer.
        if unsafe { GetClientRect(self.hwnd, &mut r) }.is_err() {
            return;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        self.client = (w, h);
        self.renderer.resize(w.max(1) as u32, h.max(1) as u32);
        self.areas = self.compute_areas();
        let cpad = scale(CHROME_PADDING_DIP, self.dpi);
        let cm = self.chrome_metrics;
        let cells = |r: Rect| {
            (
                ((r.w - 2 * cpad).max(0) / cm.width.max(1) as i32).max(1) as usize,
                ((r.h - 2 * cpad).max(0) / cm.height.max(1) as i32).max(1) as usize,
            )
        };
        if let Some(s) = self.areas.strip {
            self.strip_grid.resize(cells(s).0, 1);
            self.strip_grid.mark_all_dirty();
        }
        if let Some(s) = self.areas.side {
            let (c, rows) = cells(s);
            self.side.grid.resize(c, rows);
            self.side.grid.mark_all_dirty();
        }
        // Every tab, so background shells know their size too.
        let pad = padding(self.dpi);
        let g = gap(self.dpi);
        let placed: Vec<(PaneId, Rect)> = self
            .ws
            .tabs
            .iter()
            .flat_map(|t| t.root.layout(self.areas.panes, g))
            .collect();
        for (id, rect) in placed {
            let Some(p) = self.panes.get_mut(&id) else {
                continue;
            };
            p.rect = rect;
            let m = p.metrics;
            let cols = ((rect.w - 2 * pad).max(0) / m.width.max(1) as i32).max(1) as usize;
            let rows = ((rect.h - 2 * pad).max(0) / m.height.max(1) as i32).max(1) as usize;
            p.term.resize(cols, rows);
            p.term.set_cell_size(m.width, m.height);
            let (cols, rows) = (p.term.cols(), p.term.rows());
            let effect = match &p.host {
                Host::Local {
                    shell: Some(shell), ..
                } => {
                    shell.resize(cols, rows);
                    None
                }
                Host::Ssh(session) => session.resize(clamp16(cols), clamp16(rows)),
                _ => None,
            };
            if let Some(e) = effect {
                self.apply(id, vec![e]);
            }
        }
        self.refresh_chrome();
        self.refresh_visible();
        self.request_full_redraw();
    }

    fn request_full_redraw(&mut self) {
        self.full_redraw = true;
        // SAFETY: valid window; no erase, the renderer paints everything.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Asks Windows to repaint the band of rows a pane's grid marked dirty.
    fn invalidate_dirty(&self, id: PaneId) {
        let Some(p) = self.panes.get(&id) else {
            return;
        };
        let Some((first, last)) = p.grid.dirty_span() else {
            return;
        };
        let (_, oy) = p.origin(self.dpi);
        let ch = p.metrics.height as i32;
        let r = RECT {
            left: p.rect.x,
            top: oy + first as i32 * ch,
            right: p.rect.right(),
            bottom: oy + (last as i32 + 1) * ch,
        };
        // SAFETY: valid window and rect.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&r), false);
        }
    }

    /// Dividers, the focus frame, the side pane's edge and a drop marker.
    fn fills(&self) -> Vec<(PixelRect, Rgb)> {
        let mut out = Vec::new();
        let palette = self.chrome_palette();
        let line = mix(palette.strip, palette.text, 0.18);
        let g = gap(self.dpi);
        if let Some(e) = self.areas.side_edge {
            out.push((pixel(e), line));
        }
        let Some(tab) = self.ws.tab() else {
            return out;
        };
        for d in tab.root.dividers(self.areas.panes, g) {
            out.push((pixel(d.rect), line));
        }
        let split = tab.root.panes().len() > 1;
        if split
            && !self.side.focused
            && let Some(p) = self.panes.get(&tab.focus)
        {
            frame(p.rect, g, p.theme.cursor, &mut out);
        }
        if let Some(Drag::Pane { active: true, .. } | Drag::Tab { out: true, .. }) = &self.drag {
            let accent = Rgb::new(0xe5, 0xb5, 0x3b);
            if let Some(r) = self.drop_marker() {
                frame(r, 2 * g, accent, &mut out);
            }
        }
        out
    }

    /// Where a dragged pane or tab would land, outlined while dragging.
    fn drop_marker(&self) -> Option<Rect> {
        let (x, y) = self.mouse.at;
        let (pane, tab) = match &self.drag {
            Some(Drag::Pane { id, .. }) => (Some(*id), None),
            Some(Drag::Tab {
                index, out: true, ..
            }) => (None, Some(*index)),
            _ => return None,
        };
        if pane.is_some()
            && let Some(s) = self.areas.strip
            && s.contains(x, y)
        {
            return Some(s);
        }
        let Spot::Pane(target) = self.spot(x, y) else {
            return None;
        };
        if Some(target) == pane || tab.is_some_and(|t| self.ws.tab_of(target) == Some(t)) {
            return None;
        }
        let r = self.panes.get(&target)?.rect;
        let zone = if tab.is_some() {
            Drop::Side(layout::join_side(r, x, y))
        } else {
            layout::drop_zone(r, x, y)
        };
        Some(match zone {
            Drop::Swap => r,
            Drop::Side(Dir::Left) => Rect::new(r.x, r.y, r.w / 2, r.h),
            Drop::Side(Dir::Right) => Rect::new(r.x + r.w / 2, r.y, r.w - r.w / 2, r.h),
            Drop::Side(Dir::Up) => Rect::new(r.x, r.y, r.w, r.h / 2),
            Drop::Side(Dir::Down) => Rect::new(r.x, r.y + r.h / 2, r.w, r.h - r.h / 2),
        })
    }

    fn paint(&mut self, update: RECT) {
        // Rows Windows wants repainted (e.g. after a restore) join the
        // grids' own dirty rows; anything outside a pane's rows means
        // everything.
        let ids = self.visible_ids();
        let mut inside = false;
        for id in &ids {
            let Some(p) = self.panes.get_mut(id) else {
                continue;
            };
            let r = p.rect;
            if update.right <= r.x
                || update.left >= r.right()
                || update.bottom <= r.y
                || update.top >= r.bottom()
            {
                continue;
            }
            let pad = padding(self.dpi);
            let ch = p.metrics.height.max(1) as i32;
            let rows = p.grid.rows() as i32;
            let top = r.y + pad;
            if update.top >= top && update.bottom <= top + rows * ch {
                inside = true;
                for y in (update.top - top) / ch..=(update.bottom - 1 - top) / ch {
                    p.grid.mark_dirty(y.max(0) as usize);
                }
            }
        }
        let in_chrome = [self.areas.strip, self.areas.side]
            .into_iter()
            .flatten()
            .any(|r| {
                update.left >= r.x
                    && update.right <= r.right()
                    && update.top >= r.y
                    && update.bottom <= r.bottom()
            });
        if !inside && !in_chrome {
            self.full_redraw = true;
        }
        let full = self.full_redraw;
        let chrome_theme = self.chrome_theme();
        let cpad = scale(CHROME_PADDING_DIP, self.dpi);
        let fills = self.fills();
        let mut views: Vec<PaneView<'_>> = Vec::new();
        for id in &ids {
            let Some(p) = self.panes.get(id) else {
                continue;
            };
            views.push(PaneView {
                grid: &p.grid,
                font: &p.font,
                theme: &p.theme,
                rect: pixel(p.rect),
                origin: p.origin(self.dpi),
                cursor: p.cursor,
                full,
            });
        }
        if let Some(s) = self.areas.strip {
            views.push(PaneView {
                grid: &self.strip_grid,
                font: &self.chrome_font,
                theme: &chrome_theme,
                rect: pixel(s),
                origin: (s.x + cpad, s.y + cpad),
                cursor: None,
                full,
            });
        }
        if let Some(s) = self.areas.side {
            views.push(PaneView {
                grid: &self.side.grid,
                font: &self.chrome_font,
                theme: &chrome_theme,
                rect: pixel(s),
                origin: (s.x + cpad, s.y + cpad),
                cursor: None,
                full,
            });
        }
        let result = self.renderer.draw(&views, &fills, self.dpi);
        drop(views);
        match result {
            Ok(Frame::Presented) => {
                for id in &ids {
                    if let Some(p) = self.panes.get_mut(id) {
                        p.grid.clear_dirty();
                    }
                }
                self.strip_grid.clear_dirty();
                self.side.grid.clear_dirty();
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
        if self.side.focused {
            if !ch.is_control() {
                self.side_key(SideKey::Char(ch));
            }
            return;
        }
        let id = self.focused_id();
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        if p.term.search_active() {
            if !ch.is_control() {
                let mut query = p.term.search_query().unwrap_or_default().to_owned();
                query.push(ch);
                p.term.search_set(&query);
                self.refresh(id);
            }
            return;
        }
        let bytes = p.term.text(ch, alt);
        self.input(id, bytes);
    }

    /// Handles a key-down: window shortcuts first, then the side pane or
    /// the search bar, then keys that do not type text. Text keys are left
    /// to the WM_CHAR that TranslateMessage made from the keyboard layout,
    /// which is what gets AltGr, dead keys and IME input right.
    fn on_key_down(&mut self, vk: VIRTUAL_KEY) -> KeyOutcome {
        let mods = key_mods();
        if mods.alt && vk == VK_F4 {
            return KeyOutcome::Default;
        }
        let split = self.ws.tab().is_some_and(|t| t.root.panes().len() > 1);
        if let Some(id) = shortcut(vk, mods, self.ws.tabs.len() > 1, split) {
            drop_pending_char(self.hwnd);
            return KeyOutcome::Run(Command::Id(id));
        }
        if self.side.focused {
            if let Some(key) = side_key(vk, mods) {
                drop_pending_char(self.hwnd);
                return match self.side_key(key) {
                    Some(command) => KeyOutcome::Run(command),
                    None => KeyOutcome::Handled,
                };
            }
            return KeyOutcome::Default;
        }
        let id = self.focused_id();
        let Some(p) = self.panes.get_mut(&id) else {
            return KeyOutcome::Default;
        };
        if p.term.search_active() {
            return self.search_key(id, vk, mods);
        }
        if let Some(key) = terminal_key(vk) {
            drop_pending_char(self.hwnd);
            if let Some(bytes) = p.term.key(key, mods) {
                self.input(id, bytes);
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
                self.input(id, vec![code]);
                return KeyOutcome::Handled;
            }
        }
        KeyOutcome::Default
    }

    fn search_key(&mut self, id: PaneId, vk: VIRTUAL_KEY, mods: Mods) -> KeyOutcome {
        let Some(p) = self.panes.get_mut(&id) else {
            return KeyOutcome::Default;
        };
        let term = &mut p.term;
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
        self.refresh(id);
        KeyOutcome::Handled
    }

    /// A key for the side pane; what it picked runs here.
    fn side_key(&mut self, key: SideKey) -> Option<Command> {
        let visible = SessionList::visible(self.side.grid.rows());
        let action = self.side.list.key(key, visible);
        self.side_action(action)
    }

    fn side_action(&mut self, action: Option<SideAction>) -> Option<Command> {
        match action {
            Some(SideAction::Open { host, split }) => {
                self.open_host(&host, split);
            }
            Some(SideAction::ToggleFavourite(host)) => {
                self.settings.toggle_favourite(&host);
                self.side
                    .list
                    .set_favourites(self.settings.favourites.clone());
                self.save_settings();
            }
            Some(SideAction::Leave) => self.focus_terminal(),
            None => {}
        }
        self.refresh_chrome();
        None
    }

    /// Opens `host` from the session manager: in the focused pane if it is
    /// waiting at the connect prompt, else in a new tab (or a split).
    fn open_host(&mut self, host: &str, split: bool) {
        let focused = self.focused_id();
        let idle = matches!(
            self.panes.get(&focused).map(|p| &p.host),
            Some(Host::Ask(t)) if t.is_empty()
        );
        let id = if split {
            self.split(Dir::Right)
        } else if idle {
            Some(focused)
        } else {
            Some(self.new_tab_pane())
        };
        if let Some(id) = id {
            self.focus_terminal();
            self.connect(id, host);
        }
    }

    /// Gives the keyboard back to the focused pane.
    fn focus_terminal(&mut self) {
        if self.side.focused {
            self.side.focused = false;
            self.refresh_chrome();
            self.request_full_redraw();
        }
        let id = self.focused_id();
        if let Some(p) = self.panes.get_mut(&id)
            && let Some(c) = p.cursor
        {
            p.grid.mark_dirty(c.y);
        }
        self.refresh(id);
    }

    /// Shows the session manager, re-reading ssh_config.
    fn show_side(&mut self, focus: bool) {
        let env = ssh_env();
        self.side
            .list
            .set_hosts(host_items(&env), self.settings.favourites.clone());
        self.side.visible = true;
        self.side.focused = focus;
        self.layout();
    }

    fn hide_side(&mut self) {
        self.side.visible = false;
        self.side.focused = false;
        self.layout();
    }

    /// A pane for a new tab, with the default look.
    fn new_tab_pane(&mut self) -> PaneId {
        let id = self.ws.new_tab();
        self.add_pane(id);
        self.after_structure_change();
        id
    }

    fn add_pane(&mut self, id: PaneId) {
        let font = FontSpec::default();
        let metrics = self
            .renderer
            .metrics(&font, self.dpi)
            .unwrap_or(self.chrome_metrics);
        let pane = new_pane(
            id,
            font,
            self.default_theme.clone(),
            metrics,
            self.settings.scrollback,
        );
        self.panes.insert(id, pane);
    }

    /// Splits the focused pane; the new pane takes the focus.
    fn split(&mut self, side: Dir) -> Option<PaneId> {
        let id = self.ws.split(side)?;
        self.add_pane(id);
        self.after_structure_change();
        Some(id)
    }

    /// Opens a new pane like `from`: the same host as a new channel on its
    /// connection, a local shell, or the connect prompt.
    fn fill_like(&mut self, id: PaneId, from: PaneId) {
        enum Like {
            Ssh(Target),
            Local,
            Ask,
        }
        let like = match self.panes.get(&from).map(|p| &p.host) {
            Some(Host::Ssh(s)) => Like::Ssh(s.target.clone()),
            Some(Host::Local { .. }) => Like::Local,
            _ => Like::Ask,
        };
        match like {
            Like::Ssh(target) => self.connect_to(id, target, &ssh_env()),
            Like::Local => self.start_shell(id),
            Like::Ask => self.ask_destination(id),
        }
    }

    /// After tabs or panes changed: lay out, redraw, retitle.
    fn after_structure_change(&mut self) {
        if let Some(p) = self.panes.get_mut(&self.ws.focused()) {
            p.bell = false;
        }
        self.layout();
        self.update_title();
        self.update_menu_checks();
    }

    /// Closes a pane; the last one closes the window.
    fn close_pane(&mut self, id: PaneId) -> Option<Command> {
        match self.ws.close(id) {
            Some(Closed::Window) => return Some(Command::Destroy),
            Some(_) => {}
            None => return None,
        }
        self.end_host(id);
        self.panes.remove(&id);
        if self.mouse.pane == Some(id) {
            self.mouse.pane = None;
        }
        self.after_structure_change();
        None
    }

    /// Focuses a pane (and its tab).
    fn focus_pane(&mut self, id: PaneId) {
        let before = self.focused_id();
        let tab_before = self.ws.active;
        if !self.ws.focus(id) {
            return;
        }
        let was_side = std::mem::take(&mut self.side.focused);
        if before == id && tab_before == self.ws.active && !was_side {
            return;
        }
        if let Some(p) = self.panes.get_mut(&id) {
            p.bell = false;
        }
        for pid in [before, id] {
            if let Some(p) = self.panes.get_mut(&pid)
                && let Some(c) = p.cursor
            {
                p.grid.mark_dirty(c.y);
            }
        }
        if tab_before != self.ws.active {
            self.after_structure_change();
        } else {
            self.refresh(before);
            self.refresh(id);
            self.refresh_chrome();
            self.update_title();
            self.update_menu_checks();
            // The focus frame moves.
            self.request_full_redraw();
        }
    }

    fn select_tab(&mut self, i: usize) {
        if let Some(focus) = self.ws.tabs.get(i).map(|t| t.focus) {
            self.focus_pane(focus);
        }
    }

    /// Where a client point falls.
    fn spot(&self, x: i32, y: i32) -> Spot {
        let grab = scale(GRAB_DIP, self.dpi);
        if let Some(e) = self.areas.side_edge
            && inflate(e, grab).contains(x, y)
        {
            return Spot::SideEdge;
        }
        if let Some(s) = self.areas.side
            && s.contains(x, y)
        {
            let cpad = scale(CHROME_PADDING_DIP, self.dpi);
            let row = ((y - s.y - cpad).max(0) / self.chrome_metrics.height.max(1) as i32) as usize;
            return Spot::Side(row);
        }
        if let Some(s) = self.areas.strip
            && s.contains(x, y)
        {
            let cpad = scale(CHROME_PADDING_DIP, self.dpi);
            let col = ((x - s.x - cpad).max(0) / self.chrome_metrics.width.max(1) as i32) as usize;
            return Spot::Strip(col);
        }
        if let Some(tab) = self.ws.tab() {
            for d in tab.root.dividers(self.areas.panes, gap(self.dpi)) {
                let hit = match d.axis {
                    Axis::Row => {
                        Rect::new(d.rect.x - grab, d.rect.y, d.rect.w + 2 * grab, d.rect.h)
                    }
                    Axis::Column => {
                        Rect::new(d.rect.x, d.rect.y - grab, d.rect.w, d.rect.h + 2 * grab)
                    }
                };
                if hit.contains(x, y) {
                    return Spot::Divider(d);
                }
            }
            for id in tab.root.panes() {
                if self.panes.get(&id).is_some_and(|p| p.rect.contains(x, y)) {
                    return Spot::Pane(id);
                }
            }
        }
        Spot::Nothing
    }

    /// The mouse cursor for a client point.
    fn cursor_at(&self, x: i32, y: i32) -> PCWSTR {
        if let Some(drag) = &self.drag {
            return match drag {
                Drag::Divider {
                    axis: Axis::Row, ..
                }
                | Drag::SideEdge => IDC_SIZEWE,
                Drag::Divider {
                    axis: Axis::Column, ..
                } => IDC_SIZENS,
                Drag::Pane { active: true, .. } | Drag::Tab { out: true, .. } => IDC_SIZEALL,
                Drag::Pane { .. } => IDC_IBEAM,
                Drag::Tab { .. } => IDC_ARROW,
            };
        }
        match self.spot(x, y) {
            Spot::SideEdge => IDC_SIZEWE,
            Spot::Divider(d) if d.axis == Axis::Row => IDC_SIZEWE,
            Spot::Divider(_) => IDC_SIZENS,
            Spot::Pane(_) => IDC_IBEAM,
            _ => IDC_ARROW,
        }
    }

    /// The viewport cell under a client-area point in pane `id`, and
    /// whether the point is in the cell's right half.
    fn cell_at(&self, id: PaneId, x: i32, y: i32) -> (usize, usize, bool) {
        let Some(p) = self.panes.get(&id) else {
            return (0, 0, false);
        };
        let (ox, oy) = p.origin(self.dpi);
        let m = &p.metrics;
        let (cw, ch) = (m.width.max(1) as i32, m.height.max(1) as i32);
        let (rx, ry) = ((x - ox).max(0), (y - oy).max(0));
        let col = ((rx / cw) as usize).min(p.term.cols().saturating_sub(1));
        let row = ((ry / ch) as usize).min(p.term.rows().saturating_sub(1));
        (col, row, rx % cw >= cw / 2)
    }

    fn capture(&self, on: bool) {
        capture(self.hwnd, on);
    }

    /// A button press or release anywhere in the client.
    fn on_button(
        &mut self,
        button: MouseButton,
        down: bool,
        (x, y): (i32, i32),
        mods: Mods,
    ) -> Option<Command> {
        self.mouse.at = (x, y);
        if !down {
            if button == MouseButton::Left
                && let Some(drag) = self.drag.take()
            {
                self.capture(false);
                return self.end_drag(drag, (x, y));
            }
            let id = self.mouse.pane.take()?;
            return self.pane_button(id, button, false, (x, y), mods);
        }
        match self.spot(x, y) {
            Spot::SideEdge if button == MouseButton::Left => {
                self.capture(true);
                self.drag = Some(Drag::SideEdge);
                None
            }
            Spot::Side(row) => {
                if !self.side.focused {
                    self.side.focused = true;
                    self.request_full_redraw();
                }
                if button != MouseButton::Left {
                    self.refresh_chrome();
                    return None;
                }
                // SAFETY: plain query.
                let limit = Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }));
                let now = Instant::now();
                let double = self
                    .side
                    .last_click
                    .is_some_and(|(at, r)| r == row && now - at <= limit);
                self.side.last_click = Some((now, row));
                let visible = SessionList::visible(self.side.grid.rows());
                let action = self.side.list.click(row, double, visible);
                self.side_action(action)
            }
            Spot::Strip(col) => {
                match (button, self.strip_layout.hit(col)) {
                    (MouseButton::Left, Some(Hit::Tab(i))) => {
                        // The tab a drag below the strip shows: the one
                        // active before, or a neighbour.
                        let before = self.ws.active;
                        let home_tab = if before != i {
                            Some(before)
                        } else if i > 0 {
                            Some(i - 1)
                        } else {
                            (self.ws.tabs.len() > 1).then_some(1)
                        };
                        let home = home_tab.and_then(|t| self.ws.tabs.get(t)).map(|t| t.focus);
                        self.select_tab(i);
                        self.capture(true);
                        self.drag = Some(Drag::Tab {
                            index: i,
                            home,
                            out: false,
                        });
                    }
                    (MouseButton::Left, Some(Hit::Close(i))) => {
                        return Some(Command::CloseTab(i));
                    }
                    (MouseButton::Left, Some(Hit::NewTab)) => {
                        return Some(Command::Id(cmd::NEW_TAB));
                    }
                    // Middle-click closes a tab, as in browsers.
                    (MouseButton::Middle, Some(Hit::Tab(i) | Hit::Close(i))) => {
                        return Some(Command::CloseTab(i));
                    }
                    _ => {}
                }
                None
            }
            Spot::Divider(d) if button == MouseButton::Left => {
                self.capture(true);
                self.drag = Some(Drag::Divider {
                    path: d.path,
                    axis: d.axis,
                    span: d.span,
                });
                None
            }
            Spot::Pane(id) => {
                self.focus_pane(id);
                if button == MouseButton::Left && mods.ctrl && mods.shift {
                    self.capture(true);
                    self.drag = Some(Drag::Pane {
                        id,
                        start: (x, y),
                        active: false,
                    });
                    return None;
                }
                self.mouse.pane = Some(id);
                self.pane_button(id, button, true, (x, y), mods)
            }
            _ => None,
        }
    }

    /// Finishes a drag.
    fn end_drag(&mut self, drag: Drag, (x, y): (i32, i32)) -> Option<Command> {
        if let Drag::Tab {
            index, out: true, ..
        } = drag
        {
            let joined = match self.spot(x, y) {
                Spot::Pane(target) if self.ws.tab_of(target) != Some(index) => {
                    let r = self.panes.get(&target).map(|p| p.rect).unwrap_or_default();
                    self.ws.join_tab(index, target, layout::join_side(r, x, y))
                }
                _ => false,
            };
            if !joined && let Some(focus) = self.ws.tabs.get(index).map(|t| t.focus) {
                // Dropped elsewhere: back to the dragged tab.
                self.ws.focus(focus);
            }
            self.after_structure_change();
            self.request_full_redraw();
            return None;
        }
        if let Drag::Pane {
            id, active: true, ..
        } = drag
        {
            let moved = if self.areas.strip.is_some_and(|s| s.contains(x, y)) {
                self.ws.pane_to_tab(id)
            } else if let Spot::Pane(target) = self.spot(x, y) {
                let r = self.panes.get(&target).map(|p| p.rect).unwrap_or_default();
                self.ws.move_pane(id, target, layout::drop_zone(r, x, y))
            } else {
                false
            };
            if moved {
                self.after_structure_change();
            } else {
                self.request_full_redraw();
            }
        }
        None
    }

    fn on_mouse_move(&mut self, (x, y): (i32, i32), mods: Mods) {
        self.mouse.at = (x, y);
        if let Some(drag) = &mut self.drag {
            match drag {
                Drag::Divider { path, axis, span } => {
                    let ratio = match axis {
                        Axis::Row => (x - span.x) as f32 / span.w.max(1) as f32,
                        Axis::Column => (y - span.y) as f32 / span.h.max(1) as f32,
                    };
                    let path = path.clone();
                    let active = self.ws.active;
                    if let Some(tab) = self.ws.tabs.get_mut(active) {
                        tab.root.set_ratio(&path, ratio);
                    }
                    self.layout();
                }
                Drag::SideEdge => {
                    let (w, _) = self.client;
                    self.side.width = if self.settings.side_pane_right {
                        w - x
                    } else {
                        x
                    };
                    self.layout();
                }
                Drag::Tab { index, home, out } => {
                    let Some(s) = self.areas.strip else {
                        return;
                    };
                    let below = home.is_some() && y >= s.bottom();
                    if below != *out {
                        // Show the tab to drop into, or the dragged one again.
                        *out = below;
                        let shown = if below {
                            *home
                        } else {
                            self.ws.tabs.get(*index).map(|t| t.focus)
                        };
                        if let Some(focus) = shown {
                            self.ws.focus(focus);
                        }
                        self.after_structure_change();
                        self.request_full_redraw();
                        return;
                    }
                    if *out {
                        self.request_full_redraw();
                        return;
                    }
                    let cpad = scale(CHROME_PADDING_DIP, self.dpi);
                    let col = ((x - s.x - cpad).max(0) / self.chrome_metrics.width.max(1) as i32)
                        as usize;
                    let to = self.strip_layout.slot(col);
                    if to != *index {
                        let from = *index;
                        *index = to;
                        self.ws.move_tab(from, to);
                        self.refresh_chrome();
                        self.update_menu_checks();
                    }
                }
                Drag::Pane { start, active, .. } => {
                    if !*active
                        && ((x - start.0).abs() > DRAG_START_PX
                            || (y - start.1).abs() > DRAG_START_PX)
                    {
                        *active = true;
                    }
                    if *active {
                        self.request_full_redraw();
                    }
                }
            }
            return;
        }
        // Motion goes to the pane holding the button, or the one under the
        // mouse for hover reports.
        let id = match self.mouse.pane {
            Some(id) => id,
            None => match self.spot(x, y) {
                Spot::Pane(id) => id,
                _ => return,
            },
        };
        let (col, row, right_half) = self.cell_at(id, x, y);
        let dpi = self.dpi;
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let mut reply = None;
        if p.term.mouse_reporting() && self.mouse.last_cell != Some((col, row)) {
            self.mouse.last_cell = Some((col, row));
            let event = MouseEvent::Motion(self.mouse.reporting);
            reply = p.term.mouse(event, col, row, mods);
        }
        if self.mouse.selecting && self.mouse.pane == Some(id) {
            // Dragging past the top or bottom edge scrolls the history.
            let (_, oy) = p.origin(dpi);
            let bottom = oy + p.term.rows() as i32 * p.metrics.height as i32;
            if y < oy {
                p.term.scroll_lines(1);
            } else if y >= bottom {
                p.term.scroll_lines(-1);
            }
            p.term.select_update(col, row, right_half);
            self.refresh(id);
        }
        if let Some(bytes) = reply {
            self.send(id, bytes);
        }
    }

    /// A button event inside pane `id`.
    fn pane_button(
        &mut self,
        id: PaneId,
        button: MouseButton,
        down: bool,
        (x, y): (i32, i32),
        mods: Mods,
    ) -> Option<Command> {
        let (col, row, right_half) = self.cell_at(id, x, y);
        let copy_on_select = self.settings.copy_on_select;
        let right_click_paste = self.settings.right_click_paste;
        let hwnd = self.hwnd;
        let p = self.panes.get_mut(&id)?;
        if !down {
            capture(hwnd, false);
            if self.mouse.reporting == Some(button) {
                self.mouse.reporting = None;
                if let Some(bytes) = p.term.mouse(MouseEvent::Release(button), col, row, mods) {
                    self.send(id, bytes);
                }
                return None;
            }
            if button == MouseButton::Left
                && std::mem::take(&mut self.mouse.selecting)
                && copy_on_select
                && p.term.has_selection()
            {
                return Some(Command::Id(cmd::COPY));
            }
            return None;
        }

        capture(hwnd, true);
        // Shift-click always selects, even when the host wants the mouse.
        if p.term.mouse_reporting() && !mods.shift {
            let reply = p.term.mouse(MouseEvent::Press(button), col, row, mods);
            self.mouse.reporting = Some(button);
            self.mouse.last_cell = Some((col, row));
            if let Some(bytes) = reply {
                self.send(id, bytes);
            }
            return None;
        }
        match button {
            MouseButton::Left => {
                if mods.ctrl
                    && let Some(link) = p.term.link_at(col, row)
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
                p.term.select_start(col, row, right_half, kind);
                self.mouse.selecting = true;
                self.refresh(id);
            }
            MouseButton::Right if right_click_paste => {
                return Some(Command::Id(cmd::PASTE));
            }
            _ => {}
        }
        None
    }

    fn on_wheel(&mut self, delta: i16, screen: (i32, i32), mods: Mods) -> Option<Command> {
        let mut pt = POINT {
            x: screen.0,
            y: screen.1,
        };
        // SAFETY: our window and a local point.
        let _ = unsafe { ScreenToClient(self.hwnd, &mut pt) };
        self.mouse.wheel_rest += i32::from(delta);
        let lines = self.mouse.wheel_rest / WHEEL_PER_LINE;
        if lines == 0 {
            return None;
        }
        self.mouse.wheel_rest -= lines * WHEEL_PER_LINE;
        let id = match self.spot(pt.x, pt.y) {
            Spot::Side(_) => {
                let visible = SessionList::visible(self.side.grid.rows());
                self.side.list.scroll_by(lines, visible);
                self.refresh_chrome();
                return None;
            }
            Spot::Pane(id) => id,
            _ => self.focused_id(),
        };
        if mods.ctrl {
            self.focus_pane(id);
            let cmd = if delta > 0 {
                cmd::ZOOM_IN
            } else {
                cmd::ZOOM_OUT
            };
            return Some(Command::Id(cmd));
        }
        let (col, row, _) = self.cell_at(id, pt.x, pt.y);
        let reply = self
            .panes
            .get_mut(&id)
            .and_then(|p| p.term.wheel(lines, col, row, mods));
        if let Some(bytes) = reply {
            self.send(id, bytes);
        }
        self.refresh(id);
        None
    }

    /// Puts the IME's composition and candidate windows at the focused
    /// pane's cursor, in its font.
    fn place_ime(&self) {
        let Some(p) = self.panes.get(&self.focused_id()) else {
            return;
        };
        let Some(cursor) = p.cursor else {
            return;
        };
        let (ox, oy) = p.origin(self.dpi);
        let m = &p.metrics;
        let x = ox + cursor.x as i32 * m.width as i32;
        let y = oy + cursor.y as i32 * m.height as i32;
        let mut font = LOGFONTW {
            lfHeight: -(m.em_px.round() as i32),
            ..Default::default()
        };
        for (dst, src) in font
            .lfFaceName
            .iter_mut()
            .take(31)
            .zip(p.font.family.encode_utf16())
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

    fn save_settings(&mut self) {
        if let Err(e) = write_app_file(&settings_path(), &self.settings.to_text()) {
            let id = self.focused_id();
            self.show(
                id,
                &format!(
                    "\r\n\x1b[0;31mCould not save settings to {}: {e}\x1b[0m\r\n",
                    settings_path().display()
                ),
            );
            self.refresh(id);
        }
    }

    /// Runs a menu command that needs no call back into the window.
    fn command(&mut self, id: u32) -> Option<Command> {
        let pane = self.focused_id();
        match id {
            cmd::EXIT => return Some(Command::Exit),
            cmd::HIDE_MENU => return Some(Command::SetMenuVisible(!self.menu_visible)),
            cmd::COPY => return Some(Command::Copy),
            cmd::PASTE => return Some(Command::Paste),
            cmd::REPLACE_KEY => return Some(Command::ReplaceHostKey),
            cmd::IMPORT_PUTTY => return Some(Command::ImportPutty),
            cmd::RESTART_SHELL => self.start_shell(pane),
            cmd::CONNECT => self.ask_destination(pane),
            cmd::RECONNECT => self.reconnect(pane),
            cmd::NEW_TAB => {
                let id = self.new_tab_pane();
                self.fill_like(id, pane);
            }
            cmd::SPLIT_RIGHT | cmd::SPLIT_DOWN => {
                let dir = if id == cmd::SPLIT_RIGHT {
                    Dir::Right
                } else {
                    Dir::Down
                };
                if let Some(new) = self.split(dir) {
                    self.fill_like(new, pane);
                }
            }
            cmd::CLOSE_PANE => return self.close_pane(pane),
            cmd::PANE_TO_TAB => {
                if self.ws.pane_to_tab(pane) {
                    self.after_structure_change();
                }
            }
            cmd::JOIN_TAB => {
                if self.ws.join_neighbour() {
                    self.after_structure_change();
                }
            }
            cmd::NEXT_TAB | cmd::PREV_TAB => {
                let mut ws = self.ws.clone();
                ws.cycle_tab(id == cmd::NEXT_TAB);
                if let Some(t) = ws.tab() {
                    let focus = t.focus;
                    self.focus_pane(focus);
                }
            }
            cmd::TAB_LEFT | cmd::TAB_RIGHT => {
                let from = self.ws.active;
                let to = if id == cmd::TAB_LEFT {
                    from.checked_sub(1)
                } else {
                    Some(from + 1).filter(|&t| t < self.ws.tabs.len())
                };
                if let Some(to) = to {
                    self.ws.move_tab(from, to);
                    self.refresh_chrome();
                }
            }
            id if (cmd::FOCUS..cmd::FOCUS + 4).contains(&id) => {
                let dir = DIRS.get((id - cmd::FOCUS) as usize).copied()?;
                let g = gap(self.dpi);
                let next = self.ws.tab().and_then(|t| {
                    layout::neighbour(&t.root.layout(self.areas.panes, g), t.focus, dir)
                });
                if let Some(next) = next {
                    self.focus_pane(next);
                }
            }
            id if (cmd::RESIZE..cmd::RESIZE + 4).contains(&id) => {
                let dir = DIRS.get((id - cmd::RESIZE) as usize).copied()?;
                let active = self.ws.active;
                let changed = self
                    .ws
                    .tabs
                    .get_mut(active)
                    .is_some_and(|t| t.root.resize(t.focus, dir, RESIZE_STEP));
                if changed {
                    self.layout();
                }
            }
            cmd::SESSIONS => {
                match (self.side.visible, self.side.focused) {
                    (false, _) => self.show_side(true),
                    (true, true) => self.hide_side(),
                    (true, false) => {
                        self.side.focused = true;
                        self.refresh_chrome();
                        self.request_full_redraw();
                        self.refresh(pane);
                    }
                }
                self.update_menu_checks();
            }
            cmd::SIDE_RIGHT => {
                self.settings.side_pane_right = !self.settings.side_pane_right;
                self.save_settings();
                self.update_menu_checks();
                self.layout();
            }
            cmd::TAB_STRIP => {
                self.settings.tab_strip = !self.settings.tab_strip;
                self.save_settings();
                self.update_menu_checks();
                self.layout();
            }
            cmd::SAVE_LOOK => return Some(Command::SaveLook),
            cmd::DISCONNECT => {
                let effects = match self.panes.get_mut(&pane).map(|p| &mut p.host) {
                    Some(Host::Ssh(session))
                        if matches!(session.state(), State::Connecting | State::Connected) =>
                    {
                        session.close()
                    }
                    _ => return None,
                };
                self.apply(pane, effects);
                self.stop_log(pane);
                self.show(
                    pane,
                    "\r\n\x1b[0;90m[Disconnected. Press Enter to reconnect.]\x1b[0m\r\n",
                );
                self.refresh_chrome();
                self.refresh(pane);
            }
            cmd::LOG_TOGGLE => {
                let p = self.panes.get_mut(&pane)?;
                p.log.enabled = !p.log.enabled;
                if p.log.enabled {
                    self.start_log(pane);
                } else {
                    let path = p.log.path.clone();
                    self.stop_log(pane);
                    if let Some(path) = path {
                        let text = format!("\x1b[0;90mLog saved to {}\x1b[0m\r\n", path.display());
                        self.show(pane, &text);
                    }
                }
                self.update_menu_checks();
                self.refresh(pane);
            }
            cmd::LOG_RAW => {
                // Takes effect for the next log file.
                let p = self.panes.get_mut(&pane)?;
                p.log.raw = !p.log.raw;
                self.update_menu_checks();
            }
            cmd::LOG_FOLDER => return Some(Command::OpenFolder(log_dir())),
            cmd::ZOOM_IN | cmd::ZOOM_OUT | cmd::ZOOM_RESET => {
                let font = self.panes.get(&pane)?.font.clone();
                let font = match id {
                    cmd::ZOOM_IN => font.zoomed(1),
                    cmd::ZOOM_OUT => font.zoomed(-1),
                    _ => FontSpec::new(&font.family, FontSpec::DEFAULT_PT),
                };
                self.set_font(pane, font);
                self.layout();
            }
            cmd::TEST_PATTERN => {
                let p = self.panes.get_mut(&pane)?;
                p.term.scroll_to_bottom();
                p.term.feed(pattern::test_pattern().as_bytes());
                // The pattern asks for nothing, so there are no commands.
                let _ = self.after_feed();
            }
            cmd::FIND => {
                self.panes.get_mut(&pane)?.term.search_open();
                self.refresh(pane);
            }
            cmd::CLEAR_SCROLLBACK => {
                self.panes.get_mut(&pane)?.term.clear_scrollback();
                self.refresh(pane);
            }
            cmd::COPY_ON_SELECT => {
                self.settings.copy_on_select = !self.settings.copy_on_select;
                self.save_settings();
                self.update_menu_checks();
            }
            cmd::RIGHT_CLICK_PASTE => {
                self.settings.right_click_paste = !self.settings.right_click_paste;
                self.save_settings();
                self.update_menu_checks();
            }
            cmd::SCROLL_PAGE_UP | cmd::SCROLL_PAGE_DOWN | cmd::SCROLL_TOP | cmd::SCROLL_BOTTOM => {
                let term = &mut self.panes.get_mut(&pane)?.term;
                match id {
                    cmd::SCROLL_PAGE_UP => term.scroll_pages(1),
                    cmd::SCROLL_PAGE_DOWN => term.scroll_pages(-1),
                    cmd::SCROLL_TOP => term.scroll_to_top(),
                    _ => term.scroll_to_bottom(),
                }
                self.refresh(pane);
            }
            id if (cmd::FONT..cmd::FONT + FONT_CHOICES.len() as u32).contains(&id) => {
                if let Some(family) = FONT_CHOICES.get((id - cmd::FONT) as usize) {
                    let size = self.panes.get(&pane)?.font.size_pt;
                    self.set_font(pane, FontSpec::new(family, size));
                    self.layout();
                }
            }
            id if (cmd::THEME..cmd::THEME + Theme::builtin().len() as u32).contains(&id) => {
                if let Some(theme) = Theme::builtin().get((id - cmd::THEME) as usize) {
                    self.set_theme(pane, theme.clone());
                    self.refresh_chrome();
                    self.request_full_redraw();
                }
            }
            _ => {}
        }
        None
    }

    /// Changes a pane's font; the window keeps its size and the grid gains
    /// or loses cells at the next layout.
    fn set_font(&mut self, id: PaneId, font: FontSpec) {
        let Ok(metrics) = self.renderer.metrics(&font, self.dpi) else {
            return;
        };
        if let Some(p) = self.panes.get_mut(&id) {
            p.font = font;
            p.metrics = metrics;
        }
        self.update_menu_checks();
    }

    fn set_theme(&mut self, id: PaneId, theme: Theme) {
        if let Some(p) = self.panes.get_mut(&id) {
            p.term.set_theme(&theme);
            p.theme = theme;
        }
        self.update_menu_checks();
    }

    /// Session > Save font and theme for this host.
    fn save_look(&mut self) -> std::result::Result<String, String> {
        let pane = self.focused_id();
        let p = self.panes.get(&pane).ok_or_else(String::new)?;
        let Host::Ssh(s) = &p.host else {
            return Err(
                "Only a pane connected to an SSH host has a host to save the look for.".into(),
            );
        };
        let host = s.target.alias.clone();
        let (font, size, theme) = (p.font.family.clone(), p.font.size_pt, p.theme.name);
        let profile = self.settings.profile_mut(&host);
        profile.font = Some(font.clone());
        profile.font_size = Some(size);
        profile.theme = Some(theme.to_owned());
        self.save_settings();
        Ok(format!(
            "Saved for {host}: {font} {size} pt, {theme} theme. New panes on this host use it."
        ))
    }

    fn update_menu_checks(&mut self) {
        let p = self.panes.get(&self.focused_id());
        let font = p
            .and_then(|p| FONT_CHOICES.iter().position(|f| *f == p.font.family))
            .map_or(u32::MAX, |i| cmd::FONT + i as u32);
        let theme = p
            .and_then(|p| Theme::builtin().iter().position(|t| t.name == p.theme.name))
            .map_or(u32::MAX, |i| cmd::THEME + i as u32);
        let (log, raw) = p.map_or((false, false), |p| (p.log.enabled, p.log.raw));
        let last_font = cmd::FONT + FONT_CHOICES.len() as u32 - 1;
        let last_theme = cmd::THEME + Theme::builtin().len() as u32 - 1;
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let s = &self.settings;
        // SAFETY: valid menu handle; ids are ours. A missing id just leaves
        // the group unchecked.
        unsafe {
            let _ = CheckMenuRadioItem(self.menu, cmd::FONT, last_font, font, MF_BYCOMMAND.0);
            let _ = CheckMenuRadioItem(self.menu, cmd::THEME, last_theme, theme, MF_BYCOMMAND.0);
            for (id, on) in [
                (cmd::COPY_ON_SELECT, s.copy_on_select),
                (cmd::RIGHT_CLICK_PASTE, s.right_click_paste),
                (cmd::LOG_TOGGLE, log),
                (cmd::LOG_RAW, raw),
                (cmd::SESSIONS, self.side.visible),
                (cmd::SIDE_RIGHT, s.side_pane_right),
                (cmd::TAB_STRIP, s.tab_strip),
            ] {
                CheckMenuItem(self.menu, id, (MF_BYCOMMAND | check(on)).0);
            }
        }
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        let id = self.focused_id();
        let reply = self.panes.get_mut(&id).and_then(|p| p.term.focus(focused));
        if let Some(bytes) = reply {
            self.send(id, bytes);
        }
        if !focused {
            self.mouse.selecting = false;
            self.mouse.reporting = None;
        }
        // The cursor changes shape; its row must redraw even if nothing
        // else changed.
        if let Some(p) = self.panes.get_mut(&id)
            && let Some(c) = p.cursor
        {
            p.grid.mark_dirty(c.y);
        }
        self.refresh(id);
        self.refresh_chrome();
    }

    /// The DPI changed: reload fonts at the new size.
    fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.renderer.clear_font_cache();
        for p in self.panes.values_mut() {
            if let Ok(metrics) = self.renderer.metrics(&p.font, dpi) {
                p.metrics = metrics;
            }
        }
        if let Ok(m) = self.renderer.metrics(&self.chrome_font, dpi) {
            self.chrome_metrics = m;
        }
        self.side.width = 0;
        self.full_redraw = true;
    }

    /// The window size that keeps the layout at `dpi`: the client scaled.
    fn size_for_dpi(&self, dpi: u32) -> Option<(i32, i32)> {
        if self.dpi == 0 {
            return None;
        }
        let s = |v: i32| v * dpi as i32 / self.dpi as i32;
        Some(client_to_window(
            s(self.client.0),
            s(self.client.1),
            dpi,
            self.menu_visible,
        ))
    }

    /// SSH sessions logged in, in all tabs or in one.
    fn live_sessions(&self, tab: Option<usize>) -> usize {
        let ids = match tab {
            Some(i) => self
                .ws
                .tabs
                .get(i)
                .map(|t| t.root.panes())
                .unwrap_or_default(),
            None => self.ws.all_panes(),
        };
        ids.iter()
            .filter(|id| self.panes.get(id).is_some_and(Pane::connected))
            .count()
    }

    fn close_tab(&mut self, i: usize) -> Option<Command> {
        let ids = self.ws.tabs.get(i).map(|t| t.root.panes())?;
        for id in ids {
            if let Some(c) = self.close_pane(id) {
                return Some(c);
            }
        }
        None
    }
}

/// The window shortcut for a key, as a command id. Tab and pane keys only
/// apply when there is more than one tab or pane, so that otherwise the
/// keys reach the terminal.
fn shortcut(vk: VIRTUAL_KEY, m: Mods, tabs: bool, split: bool) -> Option<u32> {
    let key = |c: u8| VIRTUAL_KEY(u16::from(c));
    let arrow = match vk {
        VK_LEFT => Some(0),
        VK_RIGHT => Some(1),
        VK_UP => Some(2),
        VK_DOWN => Some(3),
        _ => None,
    };
    if m.alt && !m.ctrl {
        if m.shift {
            return match vk {
                VK_OEM_PLUS | VK_ADD => Some(cmd::SPLIT_RIGHT),
                VK_OEM_MINUS | VK_SUBTRACT => Some(cmd::SPLIT_DOWN),
                _ if split => arrow.map(|a| cmd::RESIZE + a),
                _ => None,
            };
        }
        return if split {
            arrow.map(|a| cmd::FOCUS + a)
        } else {
            None
        };
    }
    if m.ctrl && !m.alt {
        return match vk {
            VK_TAB if tabs => Some(if m.shift {
                cmd::PREV_TAB
            } else {
                cmd::NEXT_TAB
            }),
            VK_PRIOR if m.shift && tabs => Some(cmd::TAB_LEFT),
            VK_NEXT if m.shift && tabs => Some(cmd::TAB_RIGHT),
            v if v == key(b'J') && m.shift && tabs => Some(cmd::JOIN_TAB),
            VK_OEM_PLUS | VK_ADD => Some(cmd::ZOOM_IN),
            VK_OEM_MINUS | VK_SUBTRACT if !m.shift => Some(cmd::ZOOM_OUT),
            VK_NUMPAD0 if !m.shift => Some(cmd::ZOOM_RESET),
            v if v == key(b'0') && !m.shift => Some(cmd::ZOOM_RESET),
            v if v == key(b'M') && m.shift => Some(cmd::HIDE_MENU),
            v if v == key(b'C') && m.shift => Some(cmd::COPY),
            v if v == key(b'V') && m.shift => Some(cmd::PASTE),
            v if v == key(b'F') && m.shift => Some(cmd::FIND),
            v if v == key(b'N') && m.shift => Some(cmd::CONNECT),
            v if v == key(b'R') && m.shift => Some(cmd::RECONNECT),
            v if v == key(b'T') && m.shift => Some(cmd::NEW_TAB),
            v if v == key(b'W') && m.shift => Some(cmd::CLOSE_PANE),
            v if v == key(b'S') && m.shift => Some(cmd::SESSIONS),
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
    /// File > Exit or the close box: asks first when sessions are open.
    Exit,
    /// The last pane closed: the window goes.
    Destroy,
    SetMenuVisible(bool),
    Copy,
    Paste,
    /// The host asked to put text on the clipboard.
    ClipboardWrite(String),
    /// Ctrl+click on an OSC 8 link; the target is unchecked.
    OpenLink(String),
    UpdateTitle,
    Bell,
    /// Session > Replace host key: two confirmations, then replace.
    ReplaceHostKey,
    OpenFolder(PathBuf),
    /// The tab strip's close mark: asks first when sessions are open.
    CloseTab(usize),
    ImportPutty,
    SaveLook,
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

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn run_command(hwnd: HWND, command: Command) {
    match command {
        Command::Id(id) => {
            if let Some(next) = with_window(|w| w.command(id)).flatten() {
                run_command(hwnd, next);
            }
        }
        Command::Exit => {
            let live = with_window(|w| w.live_sessions(None)).unwrap_or(0);
            if live > 1 {
                let text = format!("{live} SSH sessions are open. Close them all and exit?");
                if dialog(hwnd, &text, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) != Some(IDYES) {
                    return;
                }
            }
            // SAFETY: our own window.
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Command::Destroy => {
            // SAFETY: our own window.
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Command::CloseTab(i) => {
            let live = with_window(|w| w.live_sessions(Some(i))).unwrap_or(0);
            if live > 1 {
                let text = format!("This tab has {live} open SSH sessions. Close them?");
                if dialog(hwnd, &text, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) != Some(IDYES) {
                    return;
                }
            }
            if let Some(next) = with_window(|w| w.close_tab(i)).flatten() {
                run_command(hwnd, next);
            }
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
            let text = with_window(|w| {
                w.panes
                    .get(&w.focused_id())
                    .and_then(|p| p.term.selection_text())
            })
            .flatten();
            if let Some(text) = text {
                clipboard::set_text(hwnd, &text);
            }
        }
        Command::Paste => {
            let Some(text) = clipboard::get_text(hwnd) else {
                return;
            };
            let Some(id) = with_window(|w| w.focused_id()) else {
                return;
            };
            if with_window(|w| w.paste_local(id, &text)) != Some(false) {
                return;
            }
            let Some(paste) =
                with_window(|w| w.panes.get(&id).map(|p| p.term.paste(&text))).flatten()
            else {
                return;
            };
            if paste.needs_confirmation() {
                let lines = paste.line_breaks;
                let text = format!(
                    "The text you are pasting contains {lines} line break{s}. Each one acts as \
                     Enter, so a shell runs every line as soon as it arrives.\n\n{}\n\nPaste it?",
                    clipboard_preview(&text, PREVIEW_CHARS),
                    s = plural(lines),
                );
                let answer = dialog(hwnd, &text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2);
                if answer != Some(IDYES) {
                    return;
                }
            }
            with_window(|w| w.input(id, paste.bytes));
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
        Command::UpdateTitle => {
            with_window(|w| w.update_title());
        }
        Command::ReplaceHostKey => replace_host_key(hwnd),
        Command::ImportPutty => import_putty(hwnd),
        Command::SaveLook => {
            if let Some(result) = with_window(Window::save_look) {
                let (text, icon) = match result {
                    Ok(t) => (t, MB_ICONINFORMATION),
                    Err(t) => (t, MB_ICONWARNING),
                };
                if !text.is_empty() {
                    dialog(hwnd, &text, MB_OK | icon);
                }
            }
        }
        Command::OpenFolder(dir) => {
            let _ = std::fs::create_dir_all(&dir);
            // SAFETY: our own log folder handed to Explorer.
            unsafe {
                ShellExecuteW(
                    Some(hwnd),
                    w!("open"),
                    &HSTRING::from(dir.as_os_str()),
                    None,
                    None,
                    SW_SHOWNORMAL,
                );
            }
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

/// File > Import PuTTY sessions: reads PuTTY's saved SSH sessions, writes
/// them to FeroSSH's own hosts file (never to `~/.ssh/config`) and their
/// fonts to profiles, after a confirmation.
fn import_putty(hwnd: HWND) {
    let sessions = putty_reg::sessions();
    if sessions.is_empty() {
        dialog(
            hwnd,
            "PuTTY has no saved SSH sessions on this account.",
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }
    // Hosts the user's own ssh_config already names win: they are not
    // imported again.
    let mut env = ssh_env();
    env.config = std::fs::read_to_string(home().join(".ssh").join("config")).unwrap_or_default();
    let includes = ssh_config::DirIncludes {
        home: env.home.clone(),
    };
    let taken: Vec<String> = ssh_config::hosts(&env.config, &includes)
        .into_iter()
        .map(|h| h.name.to_ascii_lowercase())
        .collect();
    let is_taken = |alias: &str| taken.contains(&alias.to_ascii_lowercase());
    let fresh: Vec<&putty::Session> = sessions.iter().filter(|s| !is_taken(&s.alias)).collect();
    let names: Vec<&str> = fresh.iter().take(12).map(|s| s.name.as_str()).collect();
    let more = fresh.len().saturating_sub(names.len());
    let mut text = format!(
        "Import {n} PuTTY session{s}?\n\n{}{}\n\nThey go to {} in ssh_config form, which FeroSSH \
         reads after your ~/.ssh/config. Your ssh_config and PuTTY are not changed. A new import \
         replaces that file.",
        names.join("\n"),
        if more > 0 {
            format!("\n... and {more} more")
        } else {
            String::new()
        },
        putty_hosts_path().display(),
        n = fresh.len(),
        s = plural(fresh.len()),
    );
    let skipped = sessions.len() - fresh.len();
    if skipped > 0 {
        text.push_str(&format!(
            "\n\n{skipped} session{} named like a host in your ssh_config {} skipped.",
            plural(skipped),
            if skipped == 1 { "is" } else { "are" }
        ));
    }
    if fresh.is_empty() {
        dialog(
            hwnd,
            "Every PuTTY session is already a host in your ssh_config; nothing to import.",
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }
    if dialog(hwnd, &text, MB_YESNO | MB_ICONQUESTION) != Some(IDYES) {
        return;
    }
    let config = putty::to_config(&sessions, is_taken);
    if let Err(e) = write_app_file(&putty_hosts_path(), &config) {
        dialog(
            hwnd,
            &format!("Could not write {}: {e}", putty_hosts_path().display()),
            MB_OK | MB_ICONERROR,
        );
        return;
    }
    with_window(|w| {
        // Fonts become profiles, unless the host already has one.
        for s in &fresh {
            if let Some(p) = putty::profile(s)
                && w.settings.profile(&p.host).is_none()
            {
                let host = p.host.clone();
                *w.settings.profile_mut(&host) = p;
            }
        }
        w.save_settings();
        w.show_side(true);
        w.update_menu_checks();
    });
}

/// The changed-key screen's only way forward: replace the stored key,
/// behind two confirmations, then reconnect.
fn replace_host_key(hwnd: HWND) {
    let info = with_window(|w| {
        let id = w.focused_id();
        match w.panes.get(&id).map(|p| &p.host) {
            Some(Host::Ssh(s)) => s.changed_key().map(|k| {
                let desc = ferossh_config::hostkey::describe(k)
                    .map(|d| format!("{} {}", d.kind, d.fingerprint))
                    .unwrap_or_default();
                (id, s.target.hostname.clone(), desc)
            }),
            _ => None,
        }
    })
    .flatten();
    let Some((id, host, fingerprint)) = info else {
        dialog(
            hwnd,
            "There is no changed host key to replace. This is only needed after FeroSSH \
             refused a connection because a host key changed.",
            MB_OK | MB_ICONWARNING,
        );
        return;
    };
    let first = format!(
        "Replace the stored host key of {host}?\n\nThe server now offers:\n{fingerprint}\n\n\
         Only do this if you have checked this fingerprint with the server's administrator \
         or on the server's console. If the key changed unexpectedly, someone may be \
         intercepting the connection."
    );
    if dialog(hwnd, &first, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != Some(IDYES) {
        return;
    }
    let second = format!(
        "Are you sure? FeroSSH will forget every stored key of {host} and trust\n{fingerprint}\n\n\
         from now on."
    );
    if dialog(hwnd, &second, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != Some(IDYES) {
        return;
    }
    with_window(|w| {
        let result = match w.panes.get_mut(&id).map(|p| &mut p.host) {
            Some(Host::Ssh(s)) => s.replace_host_key(),
            _ => Ok(()),
        };
        match result {
            Ok(()) => w.reconnect(id),
            Err(e) => {
                w.show(
                    id,
                    &format!("\x1b[0;31mCould not update known_hosts: {e}\x1b[0m\r\n"),
                );
                w.refresh(id);
            }
        }
    });
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
            WM_SETCURSOR if loword(lparam.0 as usize) == HTCLIENT => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
                let shape = with_window(|w| w.cursor_at(pt.x, pt.y)).unwrap_or(IDC_IBEAM);
                if let Ok(c) = LoadCursorW(None, shape) {
                    let _: HCURSOR = SetCursor(Some(c));
                }
                LRESULT(1)
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
            WM_CLOSE => {
                run_command(hwnd, Command::Exit);
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
                // Dropping the window closes the shells' consoles, finishes
                // the logs and closes the worker's job, which ends it.
                with_window(|w| {
                    let ids: Vec<PaneId> = w.panes.keys().copied().collect();
                    for id in ids {
                        w.end_host(id);
                    }
                });
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

/// Captures the mouse for `hwnd` while a button is held, or releases it.
fn capture(hwnd: HWND, on: bool) {
    // SAFETY: our window.
    unsafe {
        if on {
            SetCapture(hwnd);
        } else {
            let _ = ReleaseCapture();
        }
    }
}
