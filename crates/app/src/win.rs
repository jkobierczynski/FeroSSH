//! The Win32 window, menu, input and message loop.
//!
//! The window holds tabs, each a tree of split panes (see [`crate::layout`]).
//! A window opens with one tab and one pane; the tab strip appears with the
//! second tab, and the side pane (the session manager, or file transfer)
//! only when asked for. Each pane has its own terminal, host, log, font and
//! colour theme.

// Win32 calls are FFI; each unsafe block below is a direct, documented API call.
#![allow(unsafe_code)]

use std::cell::{Cell as StdCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ferossh_auth::Broker;
use ferossh_config::import::{self, Client};
use ferossh_config::password::{self as pw_source, PasswordSource};
use ferossh_config::saved::{self, SavedHost};
use ferossh_config::serial::{self, SerialTarget};
use ferossh_config::settings::{DEFAULT_DESKTOP_PORT, DEFAULT_VNC_PORT};
use ferossh_config::{
    SessionKind, Settings, mobaxterm, mremoteng, putty, securecrt, ssh_config, winscp,
};
use ferossh_forward::x11::{Display, find_cookie, xauth_entries};
use ferossh_ipc::{ChannelId, DirEntry, FORWARD_IDS, Message, SftpReply, X11Auth};
use ferossh_render::{
    CellMetrics, Cursor, FontSpec, Frame, Grid, PaneView, PixelRect, Renderer, Rgb, Theme,
};
use ferossh_sftp::{Remote, names};
use ferossh_term::{
    Action, Key, LogWriter, Mods, MouseButton, MouseEvent, SelectionKind, Settings as TermSettings,
    Terminal, WorkingDir, chain, check_link, clipboard_preview, log_file_name,
};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_MENUBAR, COLOR_MENUTEXT, COLOR_WINDOW,
    EndPaint, GetSysColor, InvalidateRect, LOGFONTW, PAINTSTRUCT, SYS_COLOR_INDEX, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
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
    VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F12,
    VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT, VK_NUMPAD0, VK_OEM_2, VK_OEM_MINUS, VK_OEM_PLUS,
    VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_SUBTRACT, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CheckMenuItem, CheckMenuRadioItem,
    CreateMenu, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetClientRect,
    GetCursorPos, GetMessageW, HCURSOR, HMENU, HTCLIENT, IDC_ARROW, IDC_IBEAM, IDC_SIZEALL,
    IDC_SIZENS, IDC_SIZEWE, IDYES, KillTimer, LoadCursorW, MB_DEFBUTTON2, MB_ICONERROR,
    MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO, MESSAGEBOX_RESULT,
    MESSAGEBOX_STYLE, MF_BYCOMMAND, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    MSG, MessageBoxW, PM_REMOVE, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW,
    SIZE_MINIMIZED, SW_SHOW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetCursor,
    SetMenu, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD,
    TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_CLOSE,
    WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_GETDPISCALEDSIZE,
    WM_IME_COMPOSITION, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_SIZE, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, Result, w};

use crate::Launch;
use crate::broadcast;
use crate::clipboard;
use crate::conpty::{self, Output, Shell};
use crate::credman;
use crate::desktop::{Desktop, Protocol, Update as DesktopUpdate};
use crate::files::{FilesAction, FilesView};
use crate::forwards::{ConnView, DesktopEvent, Forwarder, Setup as ForwardSetup, desktop_port_for};
use crate::fwdview::{ForwardsView, FwdAction};
use crate::import_reg;
use crate::import_view::{Found, ImportAction, ImportView};
use crate::layout::{self, Axis, Closed, Dir, Drop, PaneId, Rect, Workspace};
use crate::link::{Link, LinkEvent};
use crate::look;
use crate::pattern;
use crate::pwcommand;
use crate::sandbox;
use crate::scrollmark;
use crate::serial_port::{Output as SerialOutput, SerialPort};
use crate::session_form::{self, FormAction, Hints, SessionForm, Source, Values};
use crate::sessions::{HostItem, SessionList, SideAction, SideKey};
use crate::ssh::{Effect, Env, SshSession, State, Target};
use crate::tabstrip::{self, Forwarding, Health, Hit, Palette, StripLayout, TabLabel};
use crate::tmux::{self, Mode as TmuxMode, Tmux};
use ferossh_tmuxcc::saved as tmux_saved;
use zeroize::Zeroizing;

const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;
/// Space between a pane's edge and its grid, in DIPs.
const PADDING_DIP: i32 = 4;
/// The scroll mark's width, its gap to the pane's right edge and its
/// shortest length, in DIPs. Width plus gap stays inside the padding.
const MARK_WIDTH_DIP: i32 = 3;
const MARK_INSET_DIP: i32 = 1;
const MARK_MIN_DIP: i32 = 12;
/// Space around the tab strip's and side pane's text, in DIPs.
const CHROME_PADDING_DIP: i32 = 3;
/// Extra pixels either side of a divider that still grab it, in DIPs.
const GRAB_DIP: i32 = 3;
/// The side pane's width when first shown, in chrome cells.
const SIDE_COLS: i32 = 34;
/// The side pane font size.
const CHROME_PT: f32 = 9.5;
/// The tab strip font size.
const STRIP_PT: f32 = 10.5;
/// Space above and below the tab titles, in DIPs.
const STRIP_PAD_DIP: i32 = 6;
/// The line over the active tab, in DIPs.
const ACCENT_DIP: i32 = 2;
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
/// Updates the transfer queue's progress while files move.
const TIMER_TRANSFER: usize = 2;
const TRANSFER_TICK_MS: u32 = 250;
/// Gives remote desktops their pane's new size once it stops changing,
/// so dragging a divider does not resize the desktop at every step.
const TIMER_DESKTOP_SIZE: usize = 3;
const DESKTOP_SIZE_MS: u32 = 300;
/// Output processed per message before the window gets a chance to paint.
const OUTPUT_BUDGET: usize = 1 << 20;
/// Wheel units (120 per notch) per scrolled line: three lines a notch.
const WHEEL_PER_LINE: i32 = 40;
/// One wheel notch, as WM_MOUSEWHEEL counts it.
const WHEEL_DELTA_NOTCH: i32 = 120;
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
    pub const IMPORT: u32 = 107;
    pub const UPLOAD: u32 = 108;
    pub const DOWNLOADS: u32 = 109;
    pub const ZOOM_IN: u32 = 200;
    pub const ZOOM_OUT: u32 = 201;
    pub const ZOOM_RESET: u32 = 202;
    pub const HIDE_MENU: u32 = 203;
    pub const TEST_PATTERN: u32 = 204;
    pub const SESSIONS: u32 = 205;
    pub const SIDE_RIGHT: u32 = 206;
    pub const TAB_STRIP: u32 = 207;
    pub const SAVE_LOOK: u32 = 208;
    pub const FILES: u32 = 209;
    pub const FOLLOW_CD: u32 = 210;
    pub const FORWARDS: u32 = 211;
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
    pub const LOG_VERIFY: u32 = 605;
    pub const SERIAL_BREAK: u32 = 606;
    pub const REPLACE_KEY: u32 = 603;
    pub const VERIFY: u32 = 604;
    pub const TMUX_ATTACH: u32 = 610;
    pub const TMUX_DETACH: u32 = 611;
    pub const TMUX_PICK: u32 = 612;
    pub const TMUX_LAYOUT: u32 = 613;
    pub const DESKTOP: u32 = 614;
    pub const DESKTOP_TAB: u32 = 615;
    pub const DESKTOP_TAB_RDP: u32 = 616;
    /// Plus the index in the session picker's list; the one past the
    /// last is "New session".
    pub const TMUX_SESSION: u32 = 800;
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
    pub const BROADCAST_TAB: u32 = 730;
    pub const BROADCAST_ALL: u32 = 731;
    pub const BROADCAST_SKIP: u32 = 732;
}

const DIRS: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

/// The frame around every pane that broadcast input reaches.
const BROADCAST_FRAME: Rgb = Rgb::new(0xe5, 0x8e, 0x26);

/// Something typed in the focused pane, for the panes broadcast reaches.
#[derive(Debug)]
enum Typed {
    Char(char, bool),
    Key(Key, Mods),
    Bytes(Vec<u8>),
    Paste(String),
}

/// The prompt shown when no host was given.
const CONNECT_PROMPT: &str = "\x1b[0mConnect to (user@host[:port], a Host from ~/.ssh/config, or a COM port such as COM3:115200; Ctrl+Shift+S lists them): ";

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
    /// A pane of the tmux session that SSH pane `gateway` runs in control
    /// mode; `label` names the host.
    Tmux {
        gateway: PaneId,
        label: String,
    },
    /// The built-in viewer's remote desktop (in the pane's `desktop`);
    /// `label` names the host, and `connection` is the SSH connection the
    /// tunnel runs on, which stays open while the desktop shows.
    #[cfg_attr(not(feature = "viewer"), allow(dead_code))]
    Desktop {
        label: String,
        connection: ChannelId,
    },
    /// A serial console; `port` is `None` once it closed, when Enter
    /// opens it again.
    Serial {
        port: Option<Box<SerialPort>>,
        target: SerialTarget,
    },
}

/// The worker process, the pipes to it, and the forwarding engine that
/// shares them.
struct WorkerConn {
    link: Link,
    forwarder: Forwarder,
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
    /// The remote working directory the shell last reported (OSC 7).
    cwd: Option<Vec<u8>>,
    /// The host name in the first report: reports naming another host
    /// come from a nested `ssh` and are not this connection's.
    cwd_host: Option<String>,
    /// Where the scroll mark was last drawn, so moving or hiding it can
    /// repaint the rows it covered.
    mark: Option<Rect>,
    /// A remote desktop shown instead of the terminal, which then only
    /// carries messages (the `desktop-viewer` feature).
    desktop: Option<Box<Desktop>>,
    /// Window > Leave this pane out of broadcast input.
    no_broadcast: bool,
}

impl Pane {
    /// Top-left corner of cell (0, 0).
    fn origin(&self, dpi: u32) -> (i32, i32) {
        let pad = padding(dpi);
        (self.rect.x + pad, self.rect.y + pad)
    }

    /// The scroll mark's place while the view is scrolled back: a thin bar
    /// in the right padding, beside the rows, never over the text.
    fn scroll_mark(&self, dpi: u32) -> Option<Rect> {
        let (_, oy) = self.origin(dpi);
        let rows = self.grid.rows();
        let len = (rows as i32 * self.metrics.height as i32).min(self.rect.bottom() - oy);
        let (offset, size) = scrollmark::thumb(
            len,
            rows,
            self.term.history_len(),
            self.term.display_offset(),
            scale(MARK_MIN_DIP, dpi),
        )?;
        let w = scale(MARK_WIDTH_DIP, dpi).max(2);
        let right = self.rect.right() - scale(MARK_INSET_DIP, dpi).max(1);
        Some(Rect::new(right - w, oy + offset, w, size))
    }

    /// The scroll mark's colour: the theme's text, faded well into its
    /// background.
    fn mark_colour(&self) -> Rgb {
        mix(self.theme.background, self.theme.foreground, 0.4)
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
            Host::Tmux { label, .. } | Host::Desktop { label, .. } => label.clone(),
            Host::Serial { target, .. } => target.label(),
        }
    }

    /// The remote desktop's picture, while it shows.
    fn image(&self) -> Option<ferossh_render::Image<'_>> {
        self.desktop.as_ref()?.image(u64::from(self.id))
    }

    fn health(&self) -> Health {
        match &self.host {
            Host::Ssh(s) => match s.state() {
                State::Connected => Health::Connected,
                State::Connecting => Health::Connecting,
                State::Blocked | State::Ended => Health::Idle,
            },
            // tmux panes exist only while the gateway is logged in.
            Host::Tmux { .. } | Host::Serial { port: Some(_), .. } => Health::Connected,
            _ => Health::Idle,
        }
    }

    fn connected(&self) -> bool {
        matches!(&self.host, Host::Ssh(s) if s.state() == State::Connected)
            || matches!(self.host, Host::Tmux { .. })
    }

    /// The SSH pane carrying a tmux pane.
    fn gateway(&self) -> Option<PaneId> {
        match self.host {
            Host::Tmux { gateway, .. } => Some(gateway),
            _ => None,
        }
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

/// What the side pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SideView {
    Sessions,
    Files,
    Forwards,
}

/// The side pane: the session manager, file transfer or the forwards.
struct Side {
    visible: bool,
    view: SideView,
    /// Has the keyboard.
    focused: bool,
    list: SessionList,
    /// The session form, shown in place of the list while open.
    form: Option<SessionForm>,
    /// The import page, likewise.
    import: Option<ImportView>,
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
    strip_font: FontSpec,
    strip_metrics: CellMetrics,
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
    /// SFTP and the transfer queue, per `user@host:port`.
    sftp: HashMap<String, Remote>,
    files: FilesView,
    /// The host the file view shows: the focused pane's.
    files_key: Option<String>,
    transfer_timer: bool,
    forwards_view: ForwardsView,
    /// tmux control mode, per SSH pane that has run `tmux -CC`.
    tmux: HashMap<PaneId, Tmux>,
    /// SSH panes whose connection dropped while tmux ran: the host's
    /// queue key and the session to attach again after the next login.
    reattach: HashMap<PaneId, (String, String)>,
    /// Commands decided outside a handler that returns them; the next
    /// redraw runs them.
    queued: Vec<Command>,
    /// Remote desktops asked for and not yet open, by connection and
    /// desktop server (host as the SSH server sees it, and port).
    desktops: HashMap<(ChannelId, String, u16), DesktopWant>,
    /// Passwords from password manager commands, as they finish: the pane,
    /// its session's channel, the program and the result.
    passwords: Arc<std::sync::Mutex<Vec<PasswordAnswer>>>,
    /// Desktop sessions waiting for their SSH pane to log in.
    pending_desktops: HashMap<PaneId, PendingDesktop>,
    /// Window > Broadcast input: which panes also get what is typed.
    broadcast: broadcast::Scope,
}

/// What a password manager command gave for a pane's login.
type PasswordAnswer = (
    PaneId,
    ChannelId,
    String,
    std::result::Result<Zeroizing<String>, String>,
);

/// A desktop session whose SSH connection is still logging in.
#[derive(Debug, Clone)]
struct PendingDesktop {
    /// The session's name, for the tab and the tunnel's port.
    label: String,
    /// The desktop server as the SSH host sees it.
    host: String,
    port: u16,
    kind: SessionKind,
    user: Option<String>,
}

/// A remote desktop asked for.
#[derive(Debug, Clone)]
enum DesktopWant {
    /// Windows' Remote Desktop Connection, over RDP.
    Rdp { pane: PaneId },
    /// The built-in viewer in a new tab; `label` names the host and
    /// `user` is the SSH user, offered for RDP.
    Tab {
        label: String,
        user: String,
        protocol: Protocol,
    },
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
         ferossh [--theme NAME] --local [command]\n  \
         ferossh --new-security-key [--hello] [--ed25519] FILE"
    ));
    // SAFETY: plain message box with valid strings.
    unsafe { MessageBoxW(None, &text, w!("FeroSSH"), MB_OK | MB_ICONERROR) };
    Ok(())
}

/// `ferossh --new-security-key`: makes the key through Windows' WebAuthn
/// dialog and says what happened.
pub fn new_security_key(key: &crate::newkey::NewKey) -> Result<()> {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let comment = format!("{}@{}", env("USERNAME"), env("COMPUTERNAME"));
    let (text, icon) = match crate::newkey::make(&ferossh_auth::webauthn::WebAuthn, key, &comment) {
        Ok(line) => (
            format!(
                "Made {} and {}.pub.\n\nAdd this line to ~/.ssh/authorized_keys on the server \
                 (Ctrl+C copies this message):\n\n{line}\n\nThen log in with IdentityFile {} \
                 in ~/.ssh/config, or name the file id_ecdsa_sk or id_ed25519_sk in ~/.ssh.",
                key.path.display(),
                key.path.display(),
                key.path.display(),
            ),
            MB_ICONINFORMATION,
        ),
        Err(e) => (
            format!("Could not make the security key: {e}"),
            MB_ICONERROR,
        ),
    };
    let text = HSTRING::from(text);
    // SAFETY: plain message box with valid strings.
    unsafe { MessageBoxW(None, &text, w!("FeroSSH"), MB_OK | icon) };
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

/// Where sessions imported from `client` go, in ssh_config syntax.
/// FeroSSH reads it after `~/.ssh/config`; OpenSSH never sees it.
fn imported_path(client: Client) -> PathBuf {
    app_dir().join(client.file_name())
}

/// Sessions made in the session form, in ssh_config syntax. FeroSSH reads
/// it after `~/.ssh/config`; OpenSSH never sees it.
fn sessions_path() -> PathBuf {
    app_dir().join("sessions.conf")
}

fn load_saved() -> Vec<SavedHost> {
    std::fs::read_to_string(sessions_path())
        .map(|t| saved::parse(&t))
        .unwrap_or_default()
}

/// The user's home folder, account name, ssh_config, saved sessions and
/// imported hosts.
fn ssh_env() -> Env {
    let home = home();
    let mut config = std::fs::read_to_string(home.join(".ssh").join("config")).unwrap_or_default();
    if let Ok(made) = std::fs::read_to_string(sessions_path()) {
        // A Host line first: the user's last block must not swallow it.
        config.push_str("\nHost !*\n");
        config.push_str(&made);
    }
    for client in Client::ALL {
        if let Ok(imported) = std::fs::read_to_string(imported_path(client)) {
            // A Host line first: the user's last block must not swallow it.
            config.push_str("\nHost !*\n");
            config.push_str(&imported);
        }
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

/// The password manager commands the user allowed, one per line, in
/// `%LOCALAPPDATA%\FeroSSH`: on this PC only, not beside the settings
/// that may be copied or shared, so a settings file from elsewhere cannot
/// run a command without asking.
fn approved_commands_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("FeroSSH")
        .join("approved-password-commands.txt")
}

fn command_approved(line: &str) -> bool {
    std::fs::read_to_string(approved_commands_path())
        .is_ok_and(|t| t.lines().any(|l| l == line.trim()))
}

fn approve_command(line: &str) {
    let path = approved_commands_path();
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line.trim());
    text.push('\n');
    let _ = write_app_file(&path, &text);
}

/// The chain file that seals log `path`: the same name plus `.chain`.
fn chain_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".chain");
    PathBuf::from(name)
}

/// Appends a log that ended to the log index, linked to the line before.
fn add_to_log_index(name: &str, sealed: &ferossh_term::Sealed) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = log_dir().join(chain::INDEX_FILE);
    let index = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let line = chain::index_line(&index, name, sealed.bytes, &sealed.head);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(line.as_bytes())
}

/// Session > Check a log: the log against its chain and the log index.
/// `true` when nothing was found wrong.
fn check_log(path: &Path) -> (String, bool) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let log = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => return (format!("Could not read {}: {e}", path.display()), false),
    };
    let chain = std::fs::read_to_string(chain_path(path)).ok();
    let index_path = path.with_file_name(chain::INDEX_FILE);
    let index = std::fs::read_to_string(index_path).ok();
    let text = chain::report(&name, &log, chain.as_deref(), index.as_deref());
    let ok = chain
        .as_deref()
        .is_some_and(|c| chain::verify(&name, &log, c).is_ok())
        && !text.contains("WARNING");
    (text, ok)
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
        let strip_font = FontSpec::new(&font.family, STRIP_PT);
        let strip_metrics = renderer.metrics(&strip_font, dpi)?;
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
                    view: SideView::Sessions,
                    focused: false,
                    list: SessionList::default(),
                    form: None,
                    import: None,
                    grid: Grid::new(1, 1),
                    width: 0,
                    last_click: None,
                },
                chrome_font,
                chrome_metrics,
                strip_font,
                strip_metrics,
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
                sftp: HashMap::new(),
                files: FilesView::default(),
                files_key: None,
                transfer_timer: false,
                forwards_view: ForwardsView::default(),
                tmux: HashMap::new(),
                reattach: HashMap::new(),
                queued: Vec::new(),
                desktops: HashMap::new(),
                passwords: Arc::default(),
                pending_desktops: HashMap::new(),
                broadcast: broadcast::Scope::Off,
            });
        });
        // Files dropped from Explorer are uploaded, after a confirmation.
        DragAcceptFiles(hwnd, true);
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
        cwd: None,
        cwd_host: None,
        mark: None,
        desktop: None,
        no_broadcast: false,
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
        item(file, cmd::UPLOAD, "&Upload files...\tCtrl+Shift+U")?;
        item(file, cmd::DOWNLOADS, "Open do&wnload folder")?;
        separator(file)?;
        item(file, cmd::RESTART_SHELL, "Open &local shell")?;
        item(file, cmd::IMPORT, "&Import sessions...")?;
        separator(file)?;
        item(file, cmd::EXIT, "E&xit\tAlt+F4")?;

        let session = CreatePopupMenu()?;
        item(session, cmd::LOG_TOGGLE, "&Log to file")?;
        item(session, cmd::LOG_RAW, "Log &raw output (keep escape codes)")?;
        item(session, cmd::LOG_FOLDER, "Open log &folder")?;
        item(session, cmd::LOG_VERIFY, "C&heck a log for changes...")?;
        separator(session)?;
        item(
            session,
            cmd::SAVE_LOOK,
            "&Save font and theme for this host",
        )?;
        separator(session)?;
        item(session, cmd::REPLACE_KEY, "Replace host &key...")?;
        separator(session)?;
        item(session, cmd::VERIFY, "&Verify transfers (SHA-256)")?;
        separator(session)?;
        item(session, cmd::TMUX_ATTACH, "&Attach tmux")?;
        item(session, cmd::TMUX_PICK, "S&witch tmux session...")?;
        item(session, cmd::TMUX_DETACH, "&Detach tmux")?;
        item(
            session,
            cmd::TMUX_LAYOUT,
            "tmux: one &window per pane (own fonts)",
        )?;
        separator(session)?;
        item(session, cmd::SERIAL_BREAK, "Send &break (serial)")?;
        separator(session)?;
        item(session, cmd::DESKTOP, "Open remote &desktop\tCtrl+Shift+D")?;
        if cfg!(feature = "rdp-viewer") {
            item(
                session,
                cmd::DESKTOP_TAB_RDP,
                "Open remote desktop in a &tab (RDP)",
            )?;
        }
        if cfg!(feature = "vnc-viewer") {
            item(
                session,
                cmd::DESKTOP_TAB,
                "Open remote desktop in a tab (&VNC)",
            )?;
        }

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
        item(view, cmd::FILES, "File tra&nsfer\tCtrl+Shift+E")?;
        item(view, cmd::FORWARDS, "Port &forwards\tCtrl+Shift+P")?;
        item(view, cmd::FOLLOW_CD, "File transfer f&ollows cd")?;
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
        item(
            window,
            cmd::BROADCAST_TAB,
            "&Broadcast input to this tab's panes\tCtrl+Shift+B",
        )?;
        item(window, cmd::BROADCAST_ALL, "Broadcast input to &all tabs")?;
        item(
            window,
            cmd::BROADCAST_SKIP,
            "Leave this pane &out of broadcast",
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

fn win_rect(r: Rect) -> RECT {
    RECT {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

/// A system colour (`GetSysColor` is 0x00BBGGRR).
fn sys_color(index: SYS_COLOR_INDEX) -> Rgb {
    // SAFETY: plain query.
    let c = unsafe { GetSysColor(index) };
    Rgb::new(c as u8, (c >> 8) as u8, (c >> 16) as u8)
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
        VK_DELETE => SideKey::Delete,
        VK_TAB if !mods.ctrl => SideKey::Tab,
        VK_F5 => SideKey::Refresh,
        VK_F2 => SideKey::Edit,
        VK_INSERT => SideKey::New,
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
    /// Column and row in the side pane's grid.
    Side(usize, usize),
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

    /// An id for a new shell or SFTP channel, below the forwarding
    /// engine's range.
    fn new_channel(&mut self) -> ChannelId {
        let channel = self.next_channel;
        self.next_channel = ((self.next_channel + 1) % FORWARD_IDS).max(1);
        channel
    }

    fn pane_of_channel(&self, channel: ChannelId) -> Option<PaneId> {
        self.panes.values().find_map(|p| match &p.host {
            Host::Ssh(s) if s.channel == channel => Some(p.id),
            _ => None,
        })
    }

    /// The pane with the SSH session for pane `id`: the gateway of a tmux
    /// pane, else the pane itself.
    fn host_pane(&self, id: PaneId) -> PaneId {
        self.panes.get(&id).and_then(Pane::gateway).unwrap_or(id)
    }

    /// A tmux pane in tmux layout, whose place tmux decides: it does not
    /// move between tabs natively.
    fn locked(&self, id: PaneId) -> bool {
        self.panes
            .get(&id)
            .and_then(Pane::gateway)
            .and_then(|g| self.tmux.get(&g))
            .is_some_and(|t| t.mode() == TmuxMode::Tmux)
    }

    fn locked_tab(&self, i: usize) -> bool {
        self.ws
            .tabs
            .get(i)
            .is_some_and(|t| t.root.panes().iter().any(|id| self.locked(*id)))
    }

    /// Lines of scrollback for `target`'s panes.
    fn scrollback_for(&self, target: &Target) -> Option<usize> {
        self.settings
            .profile(&target.alias)
            .and_then(|p| p.scrollback)
            .or(self.settings.scrollback)
    }

    /// A tmux watcher for SSH pane `id`, with its host's preferences.
    fn new_tmux(&self, id: PaneId) -> Tmux {
        let (mode, history) = match self.panes.get(&id).map(|p| &p.host) {
            Some(Host::Ssh(s)) => (
                self.settings
                    .profile(&s.target.alias)
                    .and_then(|p| p.tmux_layout.as_deref())
                    .and_then(TmuxMode::parse)
                    .unwrap_or(TmuxMode::Ferossh),
                self.scrollback_for(&s.target),
            ),
            _ => (TmuxMode::Ferossh, None),
        };
        Tmux::new(
            id,
            mode,
            history.unwrap_or(TermSettings::DEFAULT_SCROLLBACK),
        )
    }

    /// Writes to the host of SSH pane `id`, past any local prompt: tmux
    /// commands, and the line that starts tmux.
    fn send_host(&mut self, id: PaneId, bytes: &[u8]) {
        let effects = match self.panes.get_mut(&id).map(|p| &mut p.host) {
            Some(Host::Ssh(s)) => s.input(bytes),
            _ => return,
        };
        self.apply(id, effects);
    }

    /// Output from SSH pane `id`'s host, which may be tmux in control
    /// mode.
    fn host_output(&mut self, id: PaneId, bytes: &[u8]) {
        if !self.tmux.contains_key(&id) {
            let t = self.new_tmux(id);
            self.tmux.insert(id, t);
        }
        let Some(t) = self.tmux.get_mut(&id) else {
            return;
        };
        let actions = t.feed(&mut self.ws, bytes);
        self.run_tmux(id, actions);
    }

    /// Carries out what gateway `gateway`'s tmux watcher decided.
    fn run_tmux(&mut self, gateway: PaneId, actions: Vec<tmux::Action>) {
        let mut structure = false;
        for action in actions {
            match action {
                tmux::Action::Write(bytes) => self.send_host(gateway, &bytes),
                tmux::Action::GatewayOutput(bytes) => {
                    if let Some(p) = self.panes.get_mut(&gateway) {
                        p.term.feed(&bytes);
                    }
                    self.log(gateway, &bytes);
                }
                tmux::Action::Create {
                    native,
                    look,
                    cols,
                    rows,
                    ..
                } => self.add_tmux_pane(gateway, native, &look, cols, rows),
                tmux::Action::Feed { native, bytes } => {
                    if let Some(p) = self.panes.get_mut(&native) {
                        p.term.feed(&bytes);
                    }
                    self.log(native, &bytes);
                }
                tmux::Action::Resize { native, cols, rows } => {
                    if let Some(p) = self.panes.get_mut(&native) {
                        p.term.resize(usize::from(cols), usize::from(rows));
                        p.grid.mark_all_dirty();
                    }
                }
                tmux::Action::Remove { native } => {
                    self.stop_log(native);
                    self.panes.remove(&native);
                    if self.mouse.pane == Some(native) {
                        self.mouse.pane = None;
                    }
                    structure = true;
                }
                tmux::Action::Focus { .. }
                | tmux::Action::ShowGateway
                | tmux::Action::Structure => structure = true,
                tmux::Action::Picker(names) => {
                    self.queued.push(Command::TmuxPicker { gateway, names });
                }
                tmux::Action::Notice(text) => {
                    let text = crate::ssh::sanitize(&text);
                    self.show(gateway, &format!("\x1b[0;90m{text}\x1b[0m\r\n"));
                }
            }
        }
        if structure {
            self.after_structure_change();
        }
    }

    /// A native pane for a tmux pane: the gateway's look, with the font,
    /// size and theme the session saved for its window.
    fn add_tmux_pane(
        &mut self,
        gateway: PaneId,
        native: PaneId,
        look: &tmux_saved::Look,
        cols: u16,
        rows: u16,
    ) {
        let Some(g) = self.panes.get(&gateway) else {
            return;
        };
        let (label, scrollback) = match &g.host {
            Host::Ssh(s) => (s.target.label(), self.scrollback_for(&s.target)),
            _ => (String::new(), None),
        };
        let mut font = g.font.clone();
        let mut metrics = g.metrics;
        let theme = look
            .theme
            .as_deref()
            .and_then(Theme::by_name)
            .unwrap_or_else(|| g.theme.clone());
        if look.font.is_some() || look.size.is_some() {
            let wanted = FontSpec::new(
                look.font.as_deref().unwrap_or(&font.family),
                look.size.unwrap_or(font.size_pt),
            );
            if let Ok(m) = self.renderer.metrics(&wanted, self.dpi) {
                font = wanted;
                metrics = m;
            }
        }
        let mut pane = new_pane(native, font, theme, metrics, scrollback);
        pane.term.resize(usize::from(cols), usize::from(rows));
        pane.host = Host::Tmux { gateway, label };
        pane.log.enabled = self.panes.get(&gateway).is_some_and(|g| g.log.enabled);
        pane.log.raw = self.panes.get(&gateway).is_some_and(|g| g.log.raw);
        self.panes.insert(native, pane);
        self.start_log(native);
    }

    /// Tells each tmux session its panes' sizes, and saves its layout and
    /// looks when they changed.
    fn sync_tmux(&mut self) {
        let pad = padding(self.dpi);
        let gateways: Vec<PaneId> = self
            .tmux
            .iter()
            .filter(|(_, t)| t.active())
            .map(|(g, _)| *g)
            .collect();
        for g in gateways {
            let panes = &self.panes;
            let cells = |n: PaneId| {
                let p = panes.get(&n)?;
                let m = p.metrics;
                let cols = (p.rect.w - 2 * pad).max(0) / m.width.max(1) as i32;
                let rows = (p.rect.h - 2 * pad).max(0) / m.height.max(1) as i32;
                Some((
                    u16::try_from(cols.max(1)).unwrap_or(u16::MAX),
                    u16::try_from(rows.max(1)).unwrap_or(u16::MAX),
                ))
            };
            let look_of = |n: PaneId| {
                panes
                    .get(&n)
                    .map(|p| tmux_saved::Look {
                        font: Some(p.font.family.clone()),
                        size: Some(p.font.size_pt),
                        theme: Some(p.theme.name.to_owned()),
                    })
                    .unwrap_or_default()
            };
            let Some(t) = self.tmux.get_mut(&g) else {
                continue;
            };
            let mut actions = t.sync_sizes(&self.ws, &cells);
            actions.extend(t.sync_saved(&self.ws, &look_of));
            self.run_tmux(g, actions);
        }
    }

    /// SSH pane `id` lost its connection: if it ran tmux, its panes go
    /// and the session is attached again after the next login.
    fn tmux_lost(&mut self, id: PaneId) {
        let key = self.pane_sftp_key(id);
        let Some(t) = self.tmux.get_mut(&id) else {
            return;
        };
        let actions = t.connection_lost(&mut self.ws);
        if let (Some(name), Some(key)) = (t.reattach.take(), key) {
            self.reattach.insert(id, (key, name));
        }
        self.run_tmux(id, actions);
    }

    /// After a login on SSH pane `id`: attach tmux again after a dropped
    /// connection, or when the host's profile asks for it. Only one pane
    /// per host runs tmux, so a second tab on the host stays a shell.
    fn tmux_after_login(&mut self, id: PaneId) {
        let Some(Host::Ssh(s)) = self.panes.get(&id).map(|p| &p.host) else {
            return;
        };
        let key = sftp_key(&s.target);
        let profile = self
            .settings
            .profile(&s.target.alias)
            .and_then(|p| p.tmux.clone());
        let name = match self.reattach.remove(&id) {
            Some((k, name)) if k == key => name,
            _ => match profile {
                Some(name) => name,
                None => return,
            },
        };
        let busy = self.tmux.iter().any(|(g, t)| {
            *g != id && t.active() && self.pane_sftp_key(*g).as_deref() == Some(key.as_str())
        });
        if !busy {
            self.attach_tmux(id, &name, false);
        }
    }

    /// Types the line that starts tmux in control mode at pane `id`'s
    /// shell.
    fn attach_tmux(&mut self, id: PaneId, name: &str, pick: bool) {
        if !self.tmux.contains_key(&id) {
            let t = self.new_tmux(id);
            self.tmux.insert(id, t);
        }
        let Some(t) = self.tmux.get_mut(&id) else {
            return;
        };
        let line = t.attach(name, pick);
        self.send_host(id, &line);
    }

    /// Feeds local text (not from the host) to a pane's terminal.
    fn show(&mut self, id: PaneId, text: &str) {
        if let Some(p) = self.panes.get_mut(&id) {
            p.term.feed(text.as_bytes());
        }
    }

    /// Ends whatever the pane is connected to.
    fn end_host(&mut self, id: PaneId) {
        // tmux on it is over too; a reattach after a dropped connection
        // stays wanted.
        if let Some(mut t) = self.tmux.remove(&id) {
            let actions = t.connection_lost(&mut self.ws);
            self.run_tmux(id, actions);
        }
        let effects = match self.panes.get_mut(&id).map(|p| &mut p.host) {
            Some(Host::Ssh(session)) => session.close(),
            _ => Vec::new(),
        };
        self.apply(id, effects);
        self.stop_log(id);
        if let Some(p) = self.panes.get_mut(&id) {
            p.host = Host::Ask(String::new());
            p.desktop = None;
            p.title = None;
            p.cwd = None;
            p.cwd_host = None;
        }
        self.prune_sftp();
        self.prune_forwards();
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
        let focused = self.focused_id();
        let label = self.panes.get(&focused).map(Pane::label);
        let badge = self.forwarding(&self.forward_view(), focused).badge();
        let legacy = if self.legacy_in_use(focused) {
            tabstrip::LEGACY_BADGE
        } else {
            ""
        };
        let broadcast = match self.broadcast {
            broadcast::Scope::Off => "",
            broadcast::Scope::Tab => " (broadcast to tab)",
            broadcast::Scope::All => " (broadcast to all tabs)",
        };
        match label {
            Some(l) if l != "new tab" => {
                self.set_title_text(&format!("{l}{legacy}{badge}{broadcast} - {APP_NAME}"));
            }
            _ => self.set_title_text(APP_NAME),
        }
    }

    /// Every connection's forwards, from the forwarding engine.
    fn forward_view(&self) -> Vec<ConnView> {
        self.worker
            .as_ref()
            .map(|w| w.forwarder.view())
            .unwrap_or_default()
    }

    /// What the badge of pane `id`'s tab shows.
    fn forwarding(&self, views: &[ConnView], id: PaneId) -> Forwarding {
        let Some(connection) = self.pane_connection(id) else {
            return Forwarding::None;
        };
        match views.iter().find(|v| v.connection == connection) {
            Some(v) if v.inbound() => Forwarding::Inbound,
            Some(v) if !v.forwards.is_empty() => Forwarding::Outbound,
            _ => Forwarding::None,
        }
    }

    /// The X server forwarded X11 goes to: the X11Display setting, else
    /// `DISPLAY`, else display 0 on this PC.
    fn x11_display(&self) -> std::result::Result<Display, String> {
        let text = self
            .settings
            .x11_display
            .clone()
            .or_else(|| std::env::var("DISPLAY").ok().filter(|d| !d.is_empty()))
            .unwrap_or_else(|| "localhost:0".to_owned());
        Display::parse(&text).ok_or(text)
    }

    /// X11 forwarding for a new shell in pane `id`: the screen and the
    /// cookie the X server expects, from `XAUTHORITY` or `~/.Xauthority`
    /// (none, for an X server without access control).
    fn x11_auth(&mut self, id: PaneId) -> Option<X11Auth> {
        let display = match self.x11_display() {
            Ok(d) => d,
            Err(text) => {
                let text = format!(
                    "\x1b[0;31mX11Display \"{}\" is not a display (try localhost:0); X11 is \
                     not forwarded.\x1b[0m\r\n",
                    crate::ssh::sanitize(&text)
                );
                self.show(id, &text);
                return None;
            }
        };
        let file = std::env::var_os("XAUTHORITY")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".Xauthority"));
        let entries = std::fs::read(file)
            .map(|b| xauth_entries(&b))
            .unwrap_or_default();
        let (protocol, cookie) = find_cookie(&entries, &display).unwrap_or_default();
        Some(X11Auth {
            screen: display.screen,
            protocol,
            cookie,
        })
    }

    /// A fresh login in pane `id`: starts the connection's forwards (an
    /// empty list too, so forwards can be added) and says what runs.
    fn start_forwards(&mut self, id: PaneId) {
        let Some(Host::Ssh(s)) = self.panes.get(&id).map(|p| &p.host) else {
            return;
        };
        if s.connection != s.channel {
            return;
        }
        let target = s.target.clone();
        let connection = s.connection;
        let x11 = target
            .forward_x11
            .then(|| self.x11_display().ok())
            .flatten();
        let mut notes: Vec<String> = target.forwards.iter().map(ToString::to_string).collect();
        if let Some(d) = &x11 {
            notes.push(format!("X11 to {d}"));
        }
        let Some(w) = &self.worker else {
            return;
        };
        w.forwarder.start(
            connection,
            target.label(),
            ForwardSetup {
                forwards: target.forwards,
                gateway_ports: target.gateway_ports,
                x11,
            },
        );
        if !notes.is_empty() {
            let text = format!(
                "\x1b[0;90mForwarding {}. Ctrl+Shift+P lists the forwards.\x1b[0m\r\n",
                notes.join(", ")
            );
            self.show(id, &text);
        }
    }

    /// Stops the forwards of connections no logged-in pane and no remote
    /// desktop uses any more.
    fn prune_forwards(&mut self) {
        let Some(w) = &self.worker else {
            return;
        };
        for v in w.forwarder.view() {
            let used = self.panes.values().any(|p| match &p.host {
                Host::Ssh(s) => s.connection == v.connection && s.state() == State::Connected,
                Host::Desktop { connection, .. } => {
                    *connection == v.connection && p.desktop.is_some()
                }
                _ => false,
            });
            if !used {
                w.forwarder.stop(v.connection);
            }
        }
    }

    /// The SSH connection pane `id` is logged in on; a tmux pane's is its
    /// gateway's.
    /// Whether pane `id`'s connection (its gateway's, for a tmux pane)
    /// uses the opt-in legacy algorithms.
    fn legacy_in_use(&self, id: PaneId) -> bool {
        match self.panes.get(&id).map(|p| &p.host) {
            Some(Host::Ssh(s)) => s.legacy_in_use(),
            Some(Host::Tmux { gateway, .. }) => self.legacy_in_use(*gateway),
            _ => false,
        }
    }

    fn pane_connection(&self, id: PaneId) -> Option<ChannelId> {
        match self.panes.get(&id).map(|p| &p.host)? {
            Host::Ssh(s) if s.state() == State::Connected => Some(s.connection),
            Host::Tmux { gateway, .. } => self.pane_connection(*gateway),
            _ => None,
        }
    }

    /// Session > Open remote desktop: asks the forwarding engine for a
    /// tunnel to the host's RDP port (or, for the built-in viewer in a
    /// tab, its RDP or VNC port); [`Self::desktop_events`] opens the
    /// viewer once it listens.
    fn open_desktop(&mut self, pane: PaneId, tab: Option<Protocol>) -> Option<Command> {
        let host = self.host_pane(pane);
        let (Some(connection), Some(Host::Ssh(s))) = (
            self.pane_connection(pane),
            self.panes.get(&host).map(|p| &p.host),
        ) else {
            return Some(Command::Notice(NO_DESKTOP_HOST.into()));
        };
        let profile = self.settings.profile(&s.target.alias);
        let label = s.target.label();
        let rdp_port = profile
            .and_then(|p| p.desktop_port)
            .unwrap_or(DEFAULT_DESKTOP_PORT);
        let (port, local, want) = match tab {
            Some(protocol) => {
                let port = match protocol {
                    Protocol::Rdp => rdp_port,
                    Protocol::Vnc => profile.and_then(|p| p.vnc_port).unwrap_or(DEFAULT_VNC_PORT),
                };
                let user = s.target.user.clone();
                // Only the viewer connects; any free port will do.
                (
                    port,
                    0,
                    DesktopWant::Tab {
                        label,
                        user,
                        protocol,
                    },
                )
            }
            None => (
                rdp_port,
                desktop_port_for(&label),
                DesktopWant::Rdp { pane },
            ),
        };
        let w = self.worker.as_ref()?;
        w.forwarder
            .open_desktop(connection, "localhost", port, local);
        self.desktops
            .insert((connection, "localhost".to_owned(), port), want);
        self.show(
            pane,
            &format!(
                "\x1b[0;90mOpening the remote desktop on port {port} of the server...\x1b[0m\r\n"
            ),
        );
        self.refresh(pane);
        None
    }

    /// Opens a viewer on the desktop tunnels that are ready, and says why
    /// the others failed.
    fn desktop_events(&mut self) {
        let events = match &self.worker {
            Some(w) => w.forwarder.take_desktop_events(),
            None => return,
        };
        for event in events {
            match event {
                DesktopEvent::Ready {
                    connection,
                    host,
                    port,
                    addr,
                } => match self.desktops.remove(&(connection, host, port)) {
                    Some(DesktopWant::Rdp { pane }) => {
                        let note = match launch_rdp_client(&addr) {
                            Ok(()) => format!(
                                "\x1b[0;90mRemote desktop tunnel at {addr}; Remote Desktop \
                                 Connection opens on it. It closes with this connection.\x1b[0m\r\n"
                            ),
                            Err(e) => format!(
                                "\x1b[0;31mThe remote desktop tunnel listens at {addr}, but \
                                 Remote Desktop Connection (mstsc.exe) did not start: {e}\x1b[0m\r\n"
                            ),
                        };
                        self.show(pane, &note);
                        self.refresh(pane);
                    }
                    Some(DesktopWant::Tab {
                        label,
                        user,
                        protocol,
                    }) => self.desktop_tab(connection, label, user, protocol, &addr),
                    None => {}
                },
                DesktopEvent::Failed {
                    connection,
                    host,
                    port,
                    why,
                } => {
                    let advice = match self.desktops.remove(&(connection, host, port)) {
                        Some(DesktopWant::Tab {
                            protocol: Protocol::Vnc,
                            ..
                        }) => {
                            " Start a VNC server for your desktop there (x11vnc or TigerVNC \
                             for X11, wayvnc or GNOME's VNC mode for Wayland), or set VncPort \
                             in this host's profile if it listens elsewhere."
                        }
                        _ => {
                            " Install xrdp (sudo apt install xrdp) or turn on GNOME Remote \
                             Desktop, or set DesktopPort in this host's profile if it listens \
                             elsewhere."
                        }
                    };
                    self.queued.push(Command::Notice(format!("{why}{advice}")));
                }
            }
        }
    }

    /// Opens the built-in viewer on the tunnel at `addr` in a new tab.
    #[cfg(feature = "viewer")]
    fn desktop_tab(
        &mut self,
        connection: ChannelId,
        label: String,
        user: String,
        protocol: Protocol,
        addr: &str,
    ) {
        let id = self.new_tab_pane();
        // The server draws the desktop at the size of the tab, when it can.
        let (width, height) = self
            .panes
            .get(&id)
            .map(|p| desktop_size(p.rect))
            .unwrap_or((1024, 768));
        let session = crate::desktop::Session {
            protocol,
            width,
            height,
            layout: keyboard_layout(),
            user,
        };
        let started = sandbox::spawn_viewer().and_then(|v| {
            let to = v.to_worker.try_clone().map_err(io_error)?;
            let from = v.from_worker.try_clone().map_err(io_error)?;
            Desktop::start(addr, session, to, from, Box::new(v), self.notifier()).map_err(io_error)
        });
        let note = match started {
            Ok(desktop) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.host = Host::Desktop {
                        label: format!("desktop {label}"),
                        connection,
                    };
                    p.desktop = Some(Box::new(desktop));
                }
                let server = match protocol {
                    Protocol::Rdp => "RDP",
                    Protocol::Vnc => "VNC",
                };
                format!(
                    "\x1b[0;90mConnecting to the {server} server through the SSH tunnel...\x1b[0m\r\n"
                )
            }
            Err(e) => format!("\x1b[0;31mThe desktop viewer did not start: {e}\x1b[0m\r\n"),
        };
        self.show(id, &note);
        self.refresh(id);
        self.update_title();
    }

    #[cfg(not(feature = "viewer"))]
    fn desktop_tab(
        &mut self,
        _connection: ChannelId,
        _label: String,
        _user: String,
        _protocol: Protocol,
        _addr: &str,
    ) {
    }

    /// Gives each remote desktop its pane's size, after the layout
    /// settled.
    fn on_desktop_size_timer(&mut self) {
        // SAFETY: our window and timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_DESKTOP_SIZE);
        }
        for p in self.panes.values_mut() {
            let (w, h) = desktop_size(p.rect);
            if let Some(d) = p.desktop.as_mut() {
                d.resize(w, h);
            }
        }
    }

    /// Applies what the remote desktops sent.
    fn poll_desktops(&mut self) {
        let ids: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.desktop.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let updates = match self.panes.get_mut(&id).and_then(|p| p.desktop.as_mut()) {
                Some(d) => d.poll(),
                None => continue,
            };
            for update in updates {
                match update {
                    DesktopUpdate::Redraw => {
                        if let Some(p) = self.panes.get_mut(&id) {
                            p.grid.mark_all_dirty();
                        }
                        self.invalidate_dirty(id);
                    }
                    DesktopUpdate::Title(name) => {
                        if let Some(p) = self.panes.get_mut(&id) {
                            p.title = Some(name).filter(|n| !n.is_empty());
                        }
                        self.update_title();
                        self.refresh_chrome();
                    }
                    DesktopUpdate::Bell => {
                        if !self.is_visible(id)
                            && let Some(p) = self.panes.get_mut(&id)
                        {
                            p.bell = true;
                            self.refresh_chrome();
                        }
                    }
                    DesktopUpdate::Ask(prompt) => {
                        self.show(id, &prompt);
                        self.refresh(id);
                    }
                    DesktopUpdate::Ended(why) => {
                        if let Some(p) = self.panes.get_mut(&id) {
                            p.desktop = None;
                            p.grid.mark_all_dirty();
                        }
                        // The connection may have stayed open only for it.
                        self.prune_forwards();
                        let why = crate::ssh::sanitize(&why);
                        self.show(
                            id,
                            &format!("\r\n\x1b[0;31mThe remote desktop ended: {why}\x1b[0m\r\n"),
                        );
                        self.request_full_redraw();
                        self.refresh(id);
                    }
                }
            }
        }
    }

    /// Keyboard and mouse messages for a remote desktop pane; `true` when
    /// the desktop took the message.
    fn desktop_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
        if !self.panes.values().any(|p| p.desktop.is_some()) {
            return false;
        }
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
                let id = self.focused_id();
                if self.side.focused {
                    return false;
                }
                let vk = VIRTUAL_KEY(wparam.0 as u16);
                let down = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN);
                // FeroSSH's own shortcuts still work in the pane.
                let split = self.ws.tab().is_some_and(|t| t.root.panes().len() > 1);
                if down && shortcut(vk, key_mods(), self.ws.tabs.len() > 1, split).is_some() {
                    return false;
                }
                let extended = lparam.0 & (1 << 24) != 0;
                let scan = ((lparam.0 >> 16) & 0xff) as u16;
                let Some(d) = self.panes.get_mut(&id).and_then(|p| p.desktop.as_mut()) else {
                    return false;
                };
                // A prompt (user name, password) reads WM_CHAR, Enter and
                // Backspace included: keep the terminal from taking them.
                if d.asking() {
                    return true;
                }
                if !d.running() {
                    return false;
                }
                if d.key(vk.0, extended, scan, down) {
                    drop_pending_char(self.hwnd);
                }
                true
            }
            WM_CHAR | WM_SYSCHAR => {
                if self.side.focused {
                    return false;
                }
                let id = self.focused_id();
                let Some(d) = self.panes.get_mut(&id).and_then(|p| p.desktop.as_mut()) else {
                    return false;
                };
                if !d.asking() && !d.running() {
                    return false;
                }
                let Some(c) = char::from_u32(wparam.0 as u32) else {
                    return true;
                };
                if let Some(echo) = d.char(c) {
                    self.show(id, &echo);
                    self.refresh(id);
                }
                true
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
            | WM_MBUTTONUP | WM_MOUSEMOVE => {
                if self.drag.is_some() {
                    return false;
                }
                let (x, y) = lparam_point(lparam);
                let Spot::Pane(id) = self.spot(x, y) else {
                    return false;
                };
                let button = match msg {
                    WM_LBUTTONDOWN => Some((1, true)),
                    WM_LBUTTONUP => Some((1, false)),
                    WM_MBUTTONDOWN => Some((2, true)),
                    WM_MBUTTONUP => Some((2, false)),
                    WM_RBUTTONDOWN => Some((4, true)),
                    WM_RBUTTONUP => Some((4, false)),
                    _ => None,
                };
                let Some(p) = self.panes.get(&id) else {
                    return false;
                };
                if !p.desktop.as_ref().is_some_and(|d| d.running()) {
                    return false;
                }
                let area = (p.rect.x, p.rect.y, p.rect.right(), p.rect.bottom());
                // Ctrl+Shift+drag still moves panes.
                let mods = mouse_mods(wparam);
                if mods.ctrl && mods.shift {
                    return false;
                }
                if matches!(button, Some((_, true))) && self.focused_id() != id {
                    self.focus_pane(id);
                }
                if let Some(d) = self.panes.get_mut(&id).and_then(|p| p.desktop.as_mut()) {
                    d.mouse(area, x, y, button);
                }
                true
            }
            WM_MOUSEWHEEL => {
                let (x, y) = lparam_point(lparam);
                let mut pt = POINT { x, y };
                // SAFETY: our window and a local point.
                let _ = unsafe { ScreenToClient(self.hwnd, &mut pt) };
                let Spot::Pane(id) = self.spot(pt.x, pt.y) else {
                    return false;
                };
                let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
                let Some(p) = self.panes.get_mut(&id) else {
                    return false;
                };
                let area = (p.rect.x, p.rect.y, p.rect.right(), p.rect.bottom());
                match p.desktop.as_mut() {
                    Some(d) if d.running() => {
                        // Touchpads send part notches: each counts as one.
                        let notches = i32::from(delta) / WHEEL_DELTA_NOTCH;
                        let notches = if notches == 0 {
                            i32::from(delta).signum()
                        } else {
                            notches
                        };
                        d.wheel(area, pt.x, pt.y, notches);
                        true
                    }
                    _ => false,
                }
            }
            WM_KILLFOCUS => {
                let id = self.focused_id();
                if let Some(d) = self.panes.get_mut(&id).and_then(|p| p.desktop.as_mut()) {
                    d.focus_lost();
                }
                false
            }
            _ => false,
        }
    }

    /// Carries out what the forwards view asked for.
    fn forwards_action(&mut self, action: Option<FwdAction>) -> Option<Command> {
        match action {
            Some(FwdAction::Add { spec, connection }) => {
                // The connection picked in the list, else the focused
                // pane's, else the only one there is.
                let view = self.forward_view();
                let known = |c: &ChannelId| view.iter().any(|v| v.connection == *c);
                let connection = connection
                    .filter(known)
                    .or_else(|| self.pane_connection(self.focused_id()).filter(known))
                    .or_else(|| match &view[..] {
                        [only] => Some(only.connection),
                        _ => None,
                    });
                match (connection, &self.worker) {
                    (Some(c), Some(w)) => w.forwarder.add(c, spec),
                    _ if view.is_empty() => self
                        .forwards_view
                        .refuse(&spec, "Log in to a host first.".into()),
                    _ => self
                        .forwards_view
                        .refuse(&spec, "Click a connection below, then Enter.".into()),
                }
            }
            Some(FwdAction::Remove { connection, index }) => {
                if let Some(w) = &self.worker {
                    w.forwarder.remove(connection, index);
                }
            }
            Some(FwdAction::Leave) => self.focus_terminal(),
            None => {}
        }
        self.refresh_chrome();
        None
    }

    /// Starts the worker if it is not running.
    fn ensure_worker(&mut self, id: PaneId) -> bool {
        if self.worker.is_some() {
            return true;
        }
        let result = sandbox::spawn_worker(self.sandbox).and_then(|w| {
            let input = w.from_worker.try_clone().map_err(io_error)?;
            let output = w.to_worker.try_clone().map_err(io_error)?;
            let (forwarder, runner) = Forwarder::new();
            let link =
                Link::start(input, output, self.notifier(), forwarder.sink()).map_err(io_error)?;
            runner
                .spawn(link.sender(), self.notifier())
                .map_err(io_error)?;
            Ok(WorkerConn {
                link,
                forwarder,
                _process: w,
            })
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
        if let Some(target) = serial::parse_target(destination) {
            self.open_serial(id, target, None);
            return;
        }
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

    /// Opens serial console `target` in pane `id`, with the look of saved
    /// session `session` (or the port's, by theme rules).
    fn open_serial(&mut self, id: PaneId, target: SerialTarget, session: Option<&str>) {
        self.end_host(id);
        let alias = session.unwrap_or(&target.port).to_owned();
        let (font, theme) = look::for_host(
            &self.settings,
            &alias,
            &target.port,
            None,
            &FontSpec::default(),
            &self.default_theme,
        );
        let scrollback = self
            .settings
            .profile(&alias)
            .and_then(|p| p.scrollback)
            .or(self.settings.scrollback);
        if let Some(p) = self.panes.get_mut(&id) {
            p.term.set_settings(&term_settings(scrollback));
            p.term.scroll_to_bottom();
        }
        self.set_theme(id, theme);
        self.set_font(id, font);
        let notify = self.notifier();
        let opened = SerialPort::open(&target, notify);
        let text = match &opened {
            Ok(_) => format!(
                "\x1b[0;90mOpened {} at {} bits/s, {}, flow control {}. \
                 Session > Send break sends a break.\x1b[0m\r\n",
                target.port,
                target.settings.speed,
                target.settings.framing(),
                target.settings.flow.label()
            ),
            Err(e) => format!("\x1b[0;31m{e}\x1b[0m\r\nPress Enter to try again.\r\n"),
        };
        if let Some(p) = self.panes.get_mut(&id) {
            p.host = Host::Serial {
                port: opened.ok().map(Box::new),
                target,
            };
        }
        self.show(id, &text);
        self.start_log(id);
        self.update_title();
        self.refresh_chrome();
        self.refresh(id);
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
        let channel = self.new_channel();
        let x11 = target.forward_x11.then(|| self.x11_auth(id)).flatten();
        let legacy = self
            .settings
            .profile(&target.alias)
            .is_some_and(|p| p.legacy_algorithms);
        let source = self
            .settings
            .profile(&target.alias)
            .map(|p| p.password.clone())
            .unwrap_or_default();
        let mut session = SshSession::new(channel, target, env.known_hosts());
        session.x11 = x11;
        session.legacy = legacy;
        session.password_source = source != PasswordSource::Ask;
        session.remember_password = source == PasswordSource::Windows;
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
            Some(Host::Serial { target, .. }) => {
                let target = target.clone();
                self.open_serial(id, target, None);
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
                Effect::Output(bytes) => self.host_output(id, &bytes),
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
                Effect::FetchPassword => self.fetch_password(id),
                Effect::StorePassword(password) => self.store_password(id, &password),
                Effect::Connected => {
                    self.start_log(id);
                    self.sftp_connected(id);
                    self.start_forwards(id);
                    self.refresh_chrome();
                    self.tmux_after_login(id);
                    self.desktop_after_login(id);
                }
                Effect::Ended => {
                    self.pending_desktops.remove(&id);
                    self.tmux_lost(id);
                    self.stop_log(id);
                    self.prune_sftp();
                    self.prune_forwards();
                    self.refresh_chrome();
                }
            }
        }
    }

    /// Gets the login password of SSH pane `id` from its password source.
    fn fetch_password(&mut self, id: PaneId) {
        let Some(Host::Ssh(s)) = self.panes.get(&id).map(|p| &p.host) else {
            return;
        };
        let t = &s.target;
        let source = self
            .settings
            .profile(&t.alias)
            .map(|p| p.password.clone())
            .unwrap_or_default();
        match source {
            PasswordSource::Ask => {
                self.password_result(id, Err(String::new()), "");
            }
            PasswordSource::Windows => {
                let result = match credman::read(&credman::target(&t.user, &t.hostname, t.port)) {
                    Ok(Some(password)) => Ok(password),
                    Ok(None) => Err(
                        "No password saved in the Windows Credential Manager yet; type it \
                         and FeroSSH saves it once the login works."
                            .into(),
                    ),
                    Err(e) => Err(e),
                };
                self.password_result(id, result, "the Windows Credential Manager");
            }
            PasswordSource::Command(line) => {
                if command_approved(&line) {
                    self.run_password_command(id, &line);
                } else {
                    self.queued
                        .push(Command::ConfirmPasswordTool { pane: id, line });
                }
            }
        }
    }

    /// Runs the password manager command `line` for pane `id`'s login on
    /// a thread of its own; [`Self::poll_passwords`] takes the answer.
    fn run_password_command(&mut self, id: PaneId, line: &str) {
        let Some(Host::Ssh(s)) = self.panes.get(&id).map(|p| &p.host) else {
            return;
        };
        let t = &s.target;
        let tokens = pw_source::Tokens {
            name: &t.alias,
            host: &t.hostname,
            user: &t.user,
            port: t.port,
        };
        let Some((program, args)) = pw_source::command(line, &tokens) else {
            self.password_result(id, Err("The password command is empty.".into()), "");
            return;
        };
        let channel = s.channel;
        let name = Path::new(&program)
            .file_stem()
            .map_or_else(|| program.clone(), |n| n.to_string_lossy().into_owned());
        self.show(
            id,
            &format!(
                "\x1b[0;90mGetting the password from {}...\x1b[0m\r\n",
                crate::ssh::sanitize(&name)
            ),
        );
        self.refresh(id);
        let answers = self.passwords.clone();
        let notify = self.notifier();
        std::thread::spawn(move || {
            let result = pwcommand::run(&program, &args, pwcommand::TIMEOUT);
            if let Ok(mut a) = answers.lock() {
                a.push((id, channel, name, result));
            }
            notify();
        });
    }

    /// Hands finished password commands to their panes, if those still
    /// wait for them.
    fn poll_passwords(&mut self) {
        let done = match self.passwords.lock() {
            Ok(mut a) => std::mem::take(&mut *a),
            Err(_) => return,
        };
        for (id, channel, name, result) in done {
            let waiting = matches!(
                self.panes.get(&id).map(|p| &p.host),
                Some(Host::Ssh(s)) if s.channel == channel && s.fetching_password()
            );
            if waiting {
                self.password_result(id, result, &name);
            }
        }
    }

    /// Gives pane `id`'s session the password its source found, or why
    /// there is none (it then asks at the prompt).
    fn password_result(
        &mut self,
        id: PaneId,
        result: std::result::Result<Zeroizing<String>, String>,
        from: &str,
    ) {
        let Some(Host::Ssh(s)) = self.panes.get_mut(&id).map(|p| &mut p.host) else {
            return;
        };
        let effects = s.password_fetched(result, from);
        self.apply(id, effects);
        self.refresh(id);
    }

    /// Keeps the password that just worked for pane `id` in the Windows
    /// Credential Manager.
    fn store_password(&mut self, id: PaneId, password: &ferossh_ipc::Secrets) {
        let (Some(Host::Ssh(s)), Some(password)) =
            (self.panes.get(&id).map(|p| &p.host), password.0.first())
        else {
            return;
        };
        let t = &s.target;
        let note = match credman::write(
            &credman::target(&t.user, &t.hostname, t.port),
            &t.user,
            password,
        ) {
            Ok(()) => "Saved the password in the Windows Credential Manager.".to_owned(),
            Err(e) => e,
        };
        self.show(id, &format!("\x1b[0;90m{note}\x1b[0m\r\n"));
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
            Host::Tmux { label, .. } => label.clone(),
            Host::Serial {
                port: Some(_),
                target,
            } => target.port.clone(),
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
                let path = dir.join(&name);
                let new = |path: &Path| {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)
                };
                match new(&path) {
                    Ok(f) => {
                        // The chain beside it seals the log against edits.
                        let chain = new(&chain_path(&path))?;
                        return Ok((f, chain, path, name));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
                    Err(e) => return Err(e),
                }
            }
        });
        let header = format!("FeroSSH log of {label}, started {}", timestamp());
        let writer = opened.and_then(|(f, chain, path, name)| {
            Ok((LogWriter::sealed(f, chain, &name, raw, &header)?, path))
        });
        match writer {
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
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let Some(writer) = p.log.writer.take() else {
            return;
        };
        let footer = format!("Log ended {}", timestamp());
        if let (Ok(Some(sealed)), Some(name)) = (
            writer.finish_sealed(&footer),
            p.log.path.as_deref().and_then(Path::file_name),
        ) {
            let name = name.to_string_lossy();
            if let Err(e) = add_to_log_index(&name, &sealed) {
                self.show(
                    id,
                    &format!("\x1b[0;31mCould not add the log to the log index: {e}\x1b[0m\r\n"),
                );
            }
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
        if let Some(gateway) = self.panes.get(&id).and_then(Pane::gateway) {
            let actions = self
                .tmux
                .get_mut(&gateway)
                .map(|t| t.input(id, &bytes))
                .unwrap_or_default();
            self.run_tmux(gateway, actions);
            return;
        }
        let effects = match self.panes.get_mut(&id).map(|p| &mut p.host) {
            Some(Host::Local {
                shell: Some(shell),
                exited: false,
            }) => {
                shell.write(bytes);
                return;
            }
            Some(Host::Ssh(session)) if !session.prompt_active() => session.input(&bytes),
            Some(Host::Serial {
                port: Some(port), ..
            }) => {
                port.write(bytes);
                return;
            }
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
            // Typing reaches the desktop before this; the pane only shows
            // messages once it ended.
            Host::Desktop { .. } => {}
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
            Host::Serial { port: None, target } => {
                if enter {
                    let target = target.clone();
                    self.open_serial(id, target, None);
                }
            }
            Host::Ssh(_) | Host::Tmux { .. } | Host::Serial { .. } => self.send(id, bytes),
        }
    }

    /// Whether pane `id` passes typing straight to its host, so it may
    /// send or receive broadcast input. Prompts and ended sessions do not.
    fn takes_broadcast(&self, id: PaneId) -> bool {
        let Some(p) = self.panes.get(&id) else {
            return false;
        };
        if p.desktop.is_some() {
            return false;
        }
        match &p.host {
            Host::Local {
                shell: Some(_),
                exited: false,
            }
            | Host::Tmux { .. }
            | Host::Serial { port: Some(_), .. } => true,
            Host::Ssh(s) => s.state() == State::Connected && !s.prompt_active(),
            _ => false,
        }
    }

    /// The other panes that get what is typed in pane `id`.
    fn broadcast_targets(&self, id: PaneId) -> Vec<PaneId> {
        if self.broadcast == broadcast::Scope::Off {
            return Vec::new();
        }
        broadcast::targets(
            self.broadcast,
            id,
            &self.visible_ids(),
            &self.ws.all_panes(),
            |p| self.panes.get(&p).is_none_or(|p| p.no_broadcast),
            |p| self.takes_broadcast(p),
        )
    }

    /// Sends a key, character or paste to each of `targets`, encoded by
    /// that pane's own terminal: its cursor-key mode, bracketed paste and
    /// keypad mode can differ from the focused pane's.
    fn send_typed(&mut self, targets: &[PaneId], typed: &Typed) {
        for &id in targets {
            let Some(p) = self.panes.get_mut(&id) else {
                continue;
            };
            let bytes = match typed {
                Typed::Char(ch, alt) => Some(p.term.text(*ch, *alt)),
                Typed::Key(key, mods) => p.term.key(*key, *mods),
                Typed::Bytes(b) => Some(b.clone()),
                Typed::Paste(text) => Some(p.term.paste(text).bytes),
            };
            if let Some(bytes) = bytes {
                self.input(id, bytes);
            }
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
        // The serial consoles.
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in ids {
            while let Some(Pane {
                host: Host::Serial {
                    port: Some(port), ..
                },
                ..
            }) = self.panes.get_mut(&id)
            {
                let Some(out) = port.try_read() else {
                    break;
                };
                match out {
                    SerialOutput::Data(data) => {
                        fed += data.len();
                        if let Some(p) = self.panes.get_mut(&id) {
                            p.term.feed(&data);
                        }
                        self.log(id, &data);
                    }
                    SerialOutput::Closed(why) => {
                        self.stop_log(id);
                        if let Some(Host::Serial { port, .. }) =
                            self.panes.get_mut(&id).map(|p| &mut p.host)
                        {
                            *port = None;
                        }
                        let text = format!(
                            "\r\n\x1b[0;31m[The serial port stopped: {}. Press Enter to open it again.]\x1b[0m\r\n",
                            why.trim()
                        );
                        self.show(id, &text);
                        self.refresh_chrome();
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
        let mut transfers = false;
        while !more {
            let Some(event) = self.worker.as_ref().and_then(|w| w.link.try_recv()) else {
                break;
            };
            match event {
                LinkEvent::Message(msg) => {
                    match &msg {
                        Message::Data { bytes, .. } => fed += bytes.len(),
                        Message::SftpReply {
                            reply: SftpReply::Data(data),
                            ..
                        } => fed += data.len(),
                        _ => {}
                    }
                    if let Some(c) = msg.channel()
                        && let Some(remote) =
                            self.sftp.values_mut().find(|r| r.channel() == Some(c))
                    {
                        remote.on_message(msg);
                        transfers = true;
                        more = fed >= OUTPUT_BUDGET;
                        continue;
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
                    if let Some(w) = &self.worker {
                        w.forwarder.worker_gone();
                    }
                    self.worker = None;
                    self.broker_jobs.clear();
                    for remote in self.sftp.values_mut() {
                        remote.detach();
                    }
                    transfers = true;
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
        if transfers {
            self.flush_sftp();
            if self.files_visible() {
                self.refresh_chrome();
            }
        }
        self.desktop_events();
        self.poll_desktops();
        self.poll_passwords();
        // The forwards list and the tab badges.
        if self
            .worker
            .as_ref()
            .is_some_and(|w| w.forwarder.take_changed())
        {
            self.refresh_chrome();
            self.update_title();
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
                    // tmux answered its panes' queries already.
                    Action::Reply(_)
                        if self.panes.get(&id).is_some_and(|p| p.gateway().is_some()) => {}
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
                    Action::WorkingDirectory(dir) => {
                        if self.on_working_dir(id, dir) {
                            chrome = true;
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
        commands.append(&mut self.queued);
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
        // A moved or hidden mark: repaint the rows under where it was and
        // where it goes; the mark itself is drawn over them as a fill.
        let mark = pane.scroll_mark(self.dpi);
        if mark != pane.mark {
            let (_, oy) = pane.origin(self.dpi);
            let ch = pane.metrics.height.max(1) as i32;
            let last = pane.grid.rows().saturating_sub(1);
            for r in [pane.mark, mark].into_iter().flatten() {
                let first = ((r.y - oy) / ch).max(0) as usize;
                let end = ((r.bottom() - 1 - oy) / ch).max(0) as usize;
                for y in first.min(last)..=end.min(last) {
                    pane.grid.mark_dirty(y);
                }
            }
            pane.mark = mark;
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

    /// The tab strip's colours: the system menu bar's, so the strip reads
    /// as part of the window frame rather than of a terminal.
    fn strip_palette(&self) -> Palette {
        Palette::menu_bar(
            sys_color(COLOR_MENUBAR),
            sys_color(COLOR_WINDOW),
            sys_color(COLOR_MENUTEXT),
            sys_color(COLOR_GRAYTEXT),
            sys_color(COLOR_HIGHLIGHT),
        )
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
            let palette = self.strip_palette();
            let active = self.ws.active;
            let views = self.forward_view();
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
                        forwarding: self.forwarding(&views, t.focus),
                        legacy: self.legacy_in_use(t.focus),
                    }
                })
                .collect();
            self.strip_layout = tabstrip::draw(&mut self.strip_grid, &labels, active, &palette);
            // The strip is drawn whole (the active tab's background and
            // line reach past the text row).
            // SAFETY: valid window and rect.
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&win_rect(strip)), false);
            }
        }
        self.sync_files();
        if let Some(side) = self.areas.side {
            let focused = self.side.focused && self.focused;
            match self.side.view {
                SideView::Sessions => match (&mut self.side.form, &mut self.side.import) {
                    (Some(form), _) => form.draw(&mut self.side.grid, &palette, focused),
                    (None, Some(view)) => view.draw(&mut self.side.grid, &palette, focused),
                    (None, None) => self.side.list.draw(&mut self.side.grid, &palette, focused),
                },
                SideView::Forwards => {
                    let view = self.forward_view();
                    self.forwards_view
                        .draw(&mut self.side.grid, &palette, focused, &view);
                }
                SideView::Files => {
                    let remote = self.files_key.as_ref().and_then(|k| self.sftp.get(k));
                    self.files.draw(
                        &mut self.side.grid,
                        &palette,
                        focused,
                        remote,
                        self.settings.follow_directory,
                    );
                }
            }
            self.invalidate_grid(side);
        }
    }

    /// Invalidates the dirty rows of the side pane's grid in `area`.
    fn invalidate_grid(&self, area: Rect) {
        let grid = &self.side.grid;
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
            let sh = self.strip_metrics.height as i32 + 2 * scale(STRIP_PAD_DIP, self.dpi);
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
            let cols = ((s.w - 2 * cpad).max(0) / self.strip_metrics.width.max(1) as i32).max(1);
            self.strip_grid.resize(cols as usize, 1);
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
            if p.gateway().is_some() {
                // tmux sizes its panes; see sync_tmux.
                let m = p.metrics;
                p.term.set_cell_size(m.width, m.height);
                continue;
            }
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
        if self.panes.values().any(|p| p.desktop.is_some()) {
            // SAFETY: our window; replaces any earlier timer with this id.
            unsafe { SetTimer(Some(self.hwnd), TIMER_DESKTOP_SIZE, DESKTOP_SIZE_MS, None) };
        }
        self.sync_tmux();
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
        if let Some(s) = self.areas.strip {
            self.strip_fills(s, &mut out);
        }
        let Some(tab) = self.ws.tab() else {
            return out;
        };
        for d in tab.root.dividers(self.areas.panes, g) {
            out.push((pixel(d.rect), line));
        }
        let split = tab.root.panes().len() > 1;
        let receivers = self.broadcast_targets(tab.focus);
        if !receivers.is_empty() {
            // Every pane that gets the typing, the focused one too, is
            // framed in amber while broadcast is on.
            for id in std::iter::once(tab.focus).chain(receivers) {
                if let Some(p) = self.panes.get(&id)
                    && self.is_visible(id)
                {
                    frame(p.rect, g, BROADCAST_FRAME, &mut out);
                }
            }
        } else if split
            && !self.side.focused
            && let Some(p) = self.panes.get(&tab.focus)
        {
            frame(p.rect, g, p.theme.cursor, &mut out);
        }
        for id in self.visible_ids() {
            if let Some(p) = self.panes.get(&id)
                && let Some(m) = p.scroll_mark(self.dpi)
            {
                out.push((pixel(m), p.mark_colour()));
            }
        }
        if let Some(Drag::Pane { active: true, .. } | Drag::Tab { out: true, .. }) = &self.drag {
            let accent = Rgb::new(0xe5, 0xb5, 0x3b);
            if let Some(r) = self.drop_marker() {
                frame(r, 2 * g, accent, &mut out);
            }
        }
        out
    }

    /// The tab strip's shapes: the active tab's background above and below
    /// its text row, its accent line, and thin lines between the others.
    fn strip_fills(&self, s: Rect, out: &mut Vec<(PixelRect, Rgb)>) {
        let p = self.strip_palette();
        let cpad = scale(CHROME_PADDING_DIP, self.dpi);
        let spad = scale(STRIP_PAD_DIP, self.dpi);
        let cw = self.strip_metrics.width as i32;
        let ch = self.strip_metrics.height as i32;
        let g = gap(self.dpi);
        let x_of = |col: usize| s.x + cpad + col as i32 * cw;
        let active = self.ws.active;
        for i in 0..self.strip_layout.len() {
            let Some((start, end)) = self.strip_layout.span(i) else {
                continue;
            };
            let (x0, x1) = (x_of(start), x_of(end));
            if i == active {
                let top = Rect::new(x0, s.y, x1 - x0, spad);
                let bottom =
                    Rect::new(x0, s.y + spad + ch, x1 - x0, s.bottom() - (s.y + spad + ch));
                out.push((pixel(top), p.active));
                out.push((pixel(bottom), p.active));
                let accent = scale(ACCENT_DIP, self.dpi).max(1);
                out.push((pixel(Rect::new(x0, s.y, x1 - x0, accent)), p.accent));
            } else if i + 1 != active {
                // A short line after each inactive tab not next to the
                // active one.
                let line = Rect::new(x1 - g, s.y + spad, g, ch);
                out.push((pixel(line), tabstrip::mix(p.strip, p.text, 0.25)));
            }
        }
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
        let mut strip_theme = chrome_theme.clone();
        strip_theme.background = self.strip_palette().strip;
        let cpad = scale(CHROME_PADDING_DIP, self.dpi);
        let spad = scale(STRIP_PAD_DIP, self.dpi);
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
                image: p.image(),
            });
        }
        if let Some(s) = self.areas.strip {
            views.push(PaneView {
                grid: &self.strip_grid,
                font: &self.strip_font,
                theme: &strip_theme,
                rect: pixel(s),
                origin: (s.x + cpad, s.y + spad),
                cursor: None,
                full: true,
                image: None,
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
                image: None,
            });
        }
        let result = self.renderer.draw(&views, &fills, self.dpi);
        drop(views);
        match result {
            Ok(Frame::Presented) => {
                for id in &ids {
                    if let Some(p) = self.panes.get_mut(id) {
                        p.grid.clear_dirty();
                        if let Some(d) = p.desktop.as_mut() {
                            d.drawn();
                        }
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
        // Who else gets it is decided first: the key may end a prompt.
        let targets = self.broadcast_targets(id);
        self.input(id, bytes);
        self.send_typed(&targets, &Typed::Char(ch, alt));
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
            let targets = self.broadcast_targets(id);
            let Some(p) = self.panes.get_mut(&id) else {
                return KeyOutcome::Handled;
            };
            if let Some(bytes) = p.term.key(key, mods) {
                self.input(id, bytes);
            }
            self.send_typed(&targets, &Typed::Key(key, mods));
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
                let targets = self.broadcast_targets(id);
                self.input(id, vec![code]);
                self.send_typed(&targets, &Typed::Bytes(vec![code]));
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
        if self.side.view == SideView::Forwards {
            let view = self.forward_view();
            let rows = self.side.grid.rows();
            let action = self.forwards_view.key(key, &view, rows);
            return self.forwards_action(action);
        }
        if self.side.view == SideView::Files {
            if key == SideKey::Refresh {
                self.ensure_sftp(self.focused_id());
            }
            let rows = self.side.grid.rows();
            let downloads = self.download_dir();
            let remote = self.files_key.as_ref().and_then(|k| self.sftp.get_mut(k));
            let action = self.files.key(remote, key, &downloads, rows);
            return self.files_action(action);
        }
        if let Some(form) = &mut self.side.form {
            let action = form.key(key, self.side.grid.rows());
            return self.form_action(action);
        }
        if let Some(view) = &mut self.side.import {
            let action = view.key(key, self.side.grid.rows());
            return self.import_action(action);
        }
        let visible = SessionList::visible(self.side.grid.rows());
        let action = self.side.list.key(key, visible);
        self.side_action(action)
    }

    fn side_action(&mut self, action: Option<SideAction>) -> Option<Command> {
        let mut out = None;
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
            Some(SideAction::New) => self.open_form(None),
            Some(SideAction::Edit(host)) => self.open_form(Some(host)),
            Some(SideAction::Delete(host)) => {
                out = Some(
                    if load_saved()
                        .iter()
                        .any(|h| h.alias.eq_ignore_ascii_case(&host))
                    {
                        Command::DeleteSession(host)
                    } else {
                        Command::Notice(format!(
                            "{host} comes from {}, which FeroSSH does not change. Edit or \
                             remove it there.",
                            host_file(&host)
                        ))
                    },
                );
            }
            Some(SideAction::Leave) => self.focus_terminal(),
            None => {}
        }
        self.refresh_chrome();
        out
    }

    /// File > Import sessions: the import page, on what the clients
    /// on this account have saved.
    fn open_import(&mut self, sessions: Vec<ImportSource>, problems: Vec<String>) {
        let list = sessions
            .into_iter()
            .map(|(c, origin, s)| found(c, origin, s))
            .collect();
        self.side.form = None;
        self.side.import = Some(ImportView::new(list, problems));
        self.show_side(SideView::Sessions, true);
        self.refresh_chrome();
    }

    /// Sessions read from a file the user picked on the import page.
    fn add_import(&mut self, (client, origin, sessions): ImportSource) {
        if let Some(view) = &mut self.side.import {
            view.add(found(client, origin, sessions));
        }
        self.refresh_chrome();
    }

    /// Carries out what the import page asked for.
    fn import_action(&mut self, action: Option<ImportAction>) -> Option<Command> {
        let mut out = None;
        match action {
            Some(ImportAction::Close) => self.side.import = None,
            Some(ImportAction::FromFile) => out = Some(Command::PickImportFile),
            Some(ImportAction::Import { client, sessions }) => {
                out = Some(Command::Notice(self.write_import(client, &sessions)));
            }
            None => {}
        }
        self.refresh_chrome();
        out
    }

    /// Writes `client`'s import file (replacing an earlier import from
    /// it) and the fonts the sessions bring; says what happened.
    fn write_import(&mut self, client: Client, sessions: &[import::Session]) -> String {
        let path = imported_path(client);
        let all: Vec<&import::Session> = sessions.iter().collect();
        if let Err(e) = write_app_file(&path, &import::to_config(client, &all)) {
            return format!("Could not write {}: {e}", path.display());
        }
        // Fonts become profiles, unless the host already has one.
        for s in sessions {
            if let Some(p) = import::profile(s)
                && self.settings.profile(&p.host).is_none()
            {
                let host = p.host.clone();
                *self.settings.profile_mut(&host) = p;
            }
        }
        self.save_settings();
        self.side.import = None;
        self.show_side(SideView::Sessions, true);
        self.update_menu_checks();
        let n = sessions.len();
        let mut text = format!(
            "Imported {n} session{} from {} into the {} folder of the session manager.",
            plural(n),
            client.name(),
            client.name(),
        );
        let saved = sessions.iter().filter(|s| s.had_password).count();
        if saved > 0 {
            text.push_str(&format!(
                "\n\n{saved} of them had a password saved in {}, which FeroSSH does not import. \
                 To log in without typing it, pick a password source with F2 on the session.",
                client.name()
            ));
        }
        text
    }

    /// Opens the session form on `host`, or on a new session.
    fn open_form(&mut self, host: Option<String>) {
        let env = ssh_env();
        let items = host_items(&env);
        let names: Vec<String> = items.iter().map(|h| h.name.clone()).collect();
        let font = FontSpec::default();
        let hints = Hints {
            local_user: env.local_user.clone(),
            font: font.family,
            font_size: font.size_pt,
            scrollback: self
                .settings
                .scrollback
                .unwrap_or(TermSettings::DEFAULT_SCROLLBACK),
        };
        let themes: Vec<String> = Theme::builtin().iter().map(|t| t.name.to_owned()).collect();
        let form = match host {
            None => SessionForm::new(None, Source::New, Values::default(), hints, names, themes),
            Some(name) => {
                let profile = self.settings.profile(&name);
                let saved = load_saved();
                match saved.iter().find(|h| h.alias.eq_ignore_ascii_case(&name)) {
                    Some(h) => SessionForm::new(
                        Some(h.alias.clone()),
                        Source::Saved,
                        Values::from_saved(h).with_profile(profile),
                        hints,
                        names,
                        themes,
                    ),
                    None => {
                        let folder = items
                            .iter()
                            .find(|h| h.name == name)
                            .and_then(|h| h.folder.clone());
                        let values = match Target::resolve(&name, &env) {
                            Some(t) => {
                                let key = t
                                    .key_files
                                    .iter()
                                    .find(|k| k.explicit)
                                    .map(|k| k.path.display().to_string());
                                Values::from_resolved(
                                    &name,
                                    folder.as_deref(),
                                    &t.hostname,
                                    &t.user,
                                    t.port,
                                    key.as_deref(),
                                    &t.forwards,
                                )
                            }
                            None => Values {
                                name: name.clone(),
                                ..Values::default()
                            },
                        };
                        let source = Source::ReadOnly(host_file(&name));
                        SessionForm::new(
                            Some(name),
                            source,
                            values.with_profile(profile),
                            hints,
                            names,
                            themes,
                        )
                    }
                }
            }
        };
        self.side.form = Some(form);
        self.side.focused = true;
        self.refresh_chrome();
    }

    /// Carries out what the session form asked for.
    fn form_action(&mut self, action: Option<FormAction>) -> Option<Command> {
        let mut out = None;
        match action {
            Some(FormAction::Cancel) => self.side.form = None,
            Some(FormAction::Save { connect }) => {
                let form = self.side.form.as_ref()?;
                let mut hosts = load_saved();
                let mut settings = self.settings.clone();
                let name = form.apply(&mut hosts, &mut settings);
                // Off the Windows Credential Manager: the saved password goes. Its
                // name is resolved before the session's settings change.
                let was_windows = form.original.as_deref().is_some_and(|o| {
                    self.settings
                        .profile(o)
                        .is_some_and(|p| p.password == PasswordSource::Windows)
                });
                let still_windows = settings
                    .profile(&name)
                    .is_some_and(|p| p.password == PasswordSource::Windows);
                let forget = (was_windows && !still_windows)
                    .then(|| form.original.clone())
                    .flatten()
                    .and_then(|o| Target::resolve(&o, &ssh_env()));
                if let Err(e) = write_app_file(&sessions_path(), &saved::to_text(&hosts)) {
                    return Some(Command::Notice(format!(
                        "Could not save the session to {}: {e}",
                        sessions_path().display()
                    )));
                }
                if let Some(t) = forget {
                    credman::delete(&credman::target(&t.user, &t.hostname, t.port));
                }
                self.settings = settings;
                self.save_settings();
                self.side.form = None;
                self.reload_sessions(Some(&name));
                if connect {
                    self.open_host(&name, false);
                }
            }
            Some(FormAction::Delete) => {
                out = self
                    .side
                    .form
                    .as_ref()
                    .and_then(|f| f.original.clone())
                    .map(Command::DeleteSession);
            }
            Some(FormAction::BrowseKey) => out = Some(Command::PickKey),
            None => {}
        }
        self.refresh_chrome();
        out
    }

    /// Removes saved session `name` (after the user said yes).
    fn delete_session(&mut self, name: &str) {
        if self
            .settings
            .profile(name)
            .is_some_and(|p| p.password == PasswordSource::Windows)
            && let Some(t) = Target::resolve(name, &ssh_env())
        {
            credman::delete(&credman::target(&t.user, &t.hostname, t.port));
        }
        let mut hosts = load_saved();
        session_form::delete(name, &mut hosts, &mut self.settings);
        if let Err(e) = write_app_file(&sessions_path(), &saved::to_text(&hosts)) {
            self.queued.push(Command::Notice(format!(
                "Could not save {}: {e}",
                sessions_path().display()
            )));
            return;
        }
        self.save_settings();
        self.side.form = None;
        self.reload_sessions(None);
        self.refresh_chrome();
    }

    /// Reads the hosts again after a session was saved or deleted.
    fn reload_sessions(&mut self, select: Option<&str>) {
        let env = ssh_env();
        self.side
            .list
            .set_hosts(host_items(&env), self.settings.favourites.clone());
        if let Some(name) = select {
            self.side.list.select(name);
        }
    }

    /// A desktop session: Remote Desktop Connection straight to the
    /// machine, or a tunnel through the session named in Via once that
    /// SSH session has logged in.
    fn open_desktop_session(
        &mut self,
        name: &str,
        kind: SessionKind,
        via: Option<String>,
        split: bool,
    ) {
        let env = ssh_env();
        let includes = ssh_config::DirIncludes {
            home: env.home.clone(),
        };
        let cfg = ssh_config::resolve_in(&env.config, name, &includes);
        let port = cfg.port.unwrap_or(match kind {
            SessionKind::Vnc => DEFAULT_VNC_PORT,
            _ => DEFAULT_DESKTOP_PORT,
        });
        let Some(via) = via else {
            if kind != SessionKind::Rdp {
                self.queued.push(Command::Notice(format!(
                    "A VNC desktop goes through an SSH session. Choose one under Via SSH in \
                     the session form (F2 on {name})."
                )));
                return;
            }
            let host = cfg.hostname.unwrap_or_else(|| name.to_owned());
            let addr = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            if let Err(e) = launch_rdp_client(&addr) {
                self.queued.push(Command::Notice(format!(
                    "Remote Desktop Connection (mstsc.exe) did not start: {e}"
                )));
            }
            return;
        };
        if kind == SessionKind::Vnc && !cfg!(feature = "vnc-viewer") {
            self.queued.push(Command::Notice(
                "This FeroSSH has no built-in VNC viewer. VNC sessions open in the desktop \
                 build (ferossh-desktop.exe)."
                    .into(),
            ));
            return;
        }
        // Without a host of its own, the desktop is on the SSH host.
        let host = cfg.hostname.unwrap_or_else(|| "localhost".to_owned());
        let pending = PendingDesktop {
            label: name.to_owned(),
            host,
            port,
            kind,
            user: cfg.user,
        };
        // The SSH session's terminal shows the login and any problem.
        let Some(id) = self.host_pane_for(split) else {
            return;
        };
        self.pending_desktops.insert(id, pending);
        self.focus_terminal();
        self.connect(id, &via);
    }

    /// After a login on SSH pane `id`: opens the desktop a desktop session
    /// waited for.
    fn desktop_after_login(&mut self, id: PaneId) {
        let Some(p) = self.pending_desktops.remove(&id) else {
            return;
        };
        let (Some(connection), Some(Host::Ssh(s))) = (
            self.pane_connection(id),
            self.panes.get(&id).map(|p| &p.host),
        ) else {
            return;
        };
        let (local, want) = match p.kind {
            SessionKind::Vnc => (
                0,
                DesktopWant::Tab {
                    label: p.label.clone(),
                    user: p.user.clone().unwrap_or_else(|| s.target.user.clone()),
                    protocol: Protocol::Vnc,
                },
            ),
            _ => (desktop_port_for(&p.label), DesktopWant::Rdp { pane: id }),
        };
        let Some(w) = self.worker.as_ref() else {
            return;
        };
        w.forwarder.open_desktop(connection, &p.host, p.port, local);
        self.desktops
            .insert((connection, p.host.clone(), p.port), want);
        self.show(
            id,
            &format!(
                "\x1b[0;90mOpening the desktop {} on {} port {}...\x1b[0m\r\n",
                crate::ssh::sanitize(&p.label),
                crate::ssh::sanitize(&p.host),
                p.port
            ),
        );
        self.refresh(id);
    }

    /// The pane a session opens in: the focused one if it waits at the
    /// connect prompt, else a new tab (or, with `split`, a split).
    fn host_pane_for(&mut self, split: bool) -> Option<PaneId> {
        let focused = self.focused_id();
        let idle = matches!(
            self.panes.get(&focused).map(|p| &p.host),
            Some(Host::Ask(t)) if t.is_empty()
        );
        if split {
            self.split(Dir::Right)
        } else if idle {
            Some(focused)
        } else {
            Some(self.new_tab_pane())
        }
    }

    /// Opens `host` from the session manager: in the focused pane if it is
    /// waiting at the connect prompt, else in a new tab (or a split).
    fn open_host(&mut self, host: &str, split: bool) {
        if let Some(p) = self.settings.profile(host)
            && p.kind == SessionKind::Serial
        {
            let line = p.serial.unwrap_or_default();
            let hostname = Target::resolve(host, &ssh_env()).map(|t| t.hostname);
            let port = hostname.as_deref().and_then(serial::port_name);
            match port.or_else(|| serial::port_name(host)) {
                Some(port) => {
                    if let Some(id) = self.host_pane_for(split) {
                        self.focus_terminal();
                        let target = SerialTarget {
                            port,
                            settings: line,
                        };
                        self.open_serial(id, target, Some(host));
                    }
                }
                None => self.queued.push(Command::Notice(format!(
                    "The session {host} has no COM port: press F2 on it and fill one in."
                ))),
            }
            return;
        }
        if let Some(p) = self.settings.profile(host)
            && p.kind != SessionKind::Ssh
        {
            let (kind, via) = (p.kind, p.via.clone());
            self.open_desktop_session(host, kind, via, split);
            return;
        }
        if let Some(id) = self.host_pane_for(split) {
            self.focus_terminal();
            self.connect(id, host);
        }
    }

    /// Connect to...: asks for a destination in `pane` if it is already
    /// waiting at the connect prompt, else in a new tab, so the shells
    /// already open keep running.
    fn connect_in_new_tab(&mut self, pane: PaneId) {
        let idle = matches!(
            self.panes.get(&pane).map(|p| &p.host),
            Some(Host::Ask(t)) if t.is_empty()
        );
        let id = if idle { pane } else { self.new_tab_pane() };
        self.focus_terminal();
        self.ask_destination(id);
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

    /// Shows the side pane in `view`; the session manager re-reads
    /// ssh_config, file transfer opens SFTP if it is not open.
    fn show_side(&mut self, view: SideView, focus: bool) {
        self.side.view = view;
        match view {
            SideView::Sessions => {
                let env = ssh_env();
                self.side
                    .list
                    .set_hosts(host_items(&env), self.settings.favourites.clone());
            }
            SideView::Files => self.ensure_sftp(self.focused_id()),
            SideView::Forwards => {}
        }
        self.side.visible = true;
        self.side.focused = focus;
        self.layout();
        self.update_menu_checks();
    }

    /// View > Session manager, File transfer or Port forwards (`id`). The
    /// shortcut first moves the focus to a pane that is open but not
    /// focused, then hides it; a menu click (`from_menu`) hides it at once,
    /// since its checkmark says the pane is open.
    fn toggle_side(&mut self, id: u32, from_menu: bool) {
        let view = match id {
            cmd::FILES => SideView::Files,
            cmd::FORWARDS => SideView::Forwards,
            _ => SideView::Sessions,
        };
        let open = self.side.visible && self.side.view == view;
        match side_toggle(open, self.side.focused, from_menu) {
            SideToggle::Show => self.show_side(view, true),
            SideToggle::Hide => self.hide_side(),
            SideToggle::Focus => {
                self.side.focused = true;
                self.refresh_chrome();
                self.request_full_redraw();
                self.refresh(self.focused_id());
            }
        }
        self.update_menu_checks();
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
        let from = self.host_pane(from);
        // A COM port opens once, so a split of a serial console asks.
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

    /// Closes a pane; the last one closes the window. A tmux pane is
    /// closed in tmux, and goes when tmux says it went.
    fn close_pane(&mut self, id: PaneId) -> Option<Command> {
        if let Some(gateway) = self.panes.get(&id).and_then(Pane::gateway)
            && let Some(t) = self.tmux.get_mut(&gateway).filter(|t| t.active())
        {
            let actions = t.close(id);
            self.run_tmux(gateway, actions);
            return None;
        }
        self.reattach.remove(&id);
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
            let col = ((x - s.x - cpad).max(0) / self.chrome_metrics.width.max(1) as i32) as usize;
            let row = ((y - s.y - cpad).max(0) / self.chrome_metrics.height.max(1) as i32) as usize;
            return Spot::Side(col, row);
        }
        if let Some(s) = self.areas.strip
            && s.contains(x, y)
        {
            let cpad = scale(CHROME_PADDING_DIP, self.dpi);
            let col = ((x - s.x - cpad).max(0) / self.strip_metrics.width.max(1) as i32) as usize;
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
            Spot::Side(col, row) => {
                if !self.side.focused {
                    self.side.focused = true;
                    self.request_full_redraw();
                }
                if button == MouseButton::Right
                    && self.side.view == SideView::Sessions
                    && self.side.form.is_none()
                    && self.side.import.is_none()
                {
                    let visible = SessionList::visible(self.side.grid.rows());
                    self.side.list.select_at(row, visible);
                    self.refresh_chrome();
                    return Some(Command::SessionMenu(self.side.list.selected_host()));
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
                if self.side.view == SideView::Forwards {
                    let view = self.forward_view();
                    let rows = self.side.grid.rows();
                    self.forwards_view.click(row, &view, rows);
                    self.refresh_chrome();
                    return None;
                }
                if self.side.view == SideView::Files {
                    let rows = self.side.grid.rows();
                    let downloads = self.download_dir();
                    let remote = self.files_key.as_ref().and_then(|k| self.sftp.get_mut(k));
                    let action = self.files.click(remote, col, row, double, &downloads, rows);
                    return self.files_action(action);
                }
                if let Some(form) = &mut self.side.form {
                    let action = form.click(col, row, self.side.grid.rows());
                    return self.form_action(action);
                }
                if let Some(view) = &mut self.side.import {
                    let action = view.click(col, row, self.side.grid.rows());
                    return self.import_action(action);
                }
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
                Spot::Pane(target)
                    if self.ws.tab_of(target) != Some(index)
                        && !self.locked_tab(index)
                        && !self.locked(target) =>
                {
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
            && !self.locked(id)
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
                // Back to the pane's own tab, if the drag showed another.
                let shown = self.ws.active;
                self.ws.focus(id);
                if self.ws.active != shown {
                    self.after_structure_change();
                }
                self.request_full_redraw();
            }
        }
        None
    }

    /// The tab under client x in the strip, if any.
    fn strip_tab_at(&self, x: i32) -> Option<usize> {
        let s = self.areas.strip?;
        let cpad = scale(CHROME_PADDING_DIP, self.dpi);
        let col = ((x - s.x - cpad).max(0) / self.strip_metrics.width.max(1) as i32) as usize;
        match self.strip_layout.hit(col)? {
            Hit::Tab(i) | Hit::Close(i) => Some(i),
            Hit::NewTab => None,
        }
    }

    fn on_mouse_move(&mut self, (x, y): (i32, i32), mods: Mods) {
        self.mouse.at = (x, y);
        let hover = self.strip_tab_at(x);
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
                    let in_strip = y < s.bottom();
                    if *out && in_strip {
                        // Back over the strip: another tab shows that tab
                        // to drop into; the dragged one goes back to
                        // moving it along the strip.
                        match hover {
                            Some(i) if i != *index => {
                                let focus = self.ws.tabs.get(i).map(|t| t.focus);
                                if focus.is_some() && focus != *home {
                                    *home = focus;
                                    if let Some(f) = focus {
                                        self.ws.focus(f);
                                    }
                                    self.after_structure_change();
                                }
                            }
                            _ => {
                                *out = false;
                                if let Some(f) = self.ws.tabs.get(*index).map(|t| t.focus) {
                                    self.ws.focus(f);
                                }
                                self.after_structure_change();
                            }
                        }
                        self.request_full_redraw();
                        return;
                    }
                    let below = home.is_some() && !in_strip;
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
                    let col =
                        ((x - s.x - cpad).max(0) / self.strip_metrics.width.max(1) as i32) as usize;
                    let to = self.strip_layout.slot(col);
                    if to != *index {
                        let from = *index;
                        *index = to;
                        self.ws.move_tab(from, to);
                        self.refresh_chrome();
                        self.update_menu_checks();
                    }
                }
                Drag::Pane { id, start, active } => {
                    if !*active
                        && ((x - start.0).abs() > DRAG_START_PX
                            || (y - start.1).abs() > DRAG_START_PX)
                    {
                        *active = true;
                    }
                    if *active {
                        // Over a tab in the strip: show it, to drop the
                        // pane into one of its panes.
                        let id = *id;
                        let over_strip = self.areas.strip.is_some_and(|s| s.contains(x, y));
                        if let Some(i) = hover.filter(|_| over_strip)
                            && i != self.ws.active
                            && let Some(f) = self.ws.tabs.get(i).map(|t| t.focus)
                        {
                            let f = if self.ws.tab_of(id) == Some(i) { id } else { f };
                            self.ws.focus(f);
                            self.after_structure_change();
                        }
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
            Spot::Side(..) => {
                if self.side.view == SideView::Forwards {
                    let view = self.forward_view();
                    let rows = self.side.grid.rows();
                    self.forwards_view.scroll_by(lines, &view, rows);
                } else if self.side.view == SideView::Files {
                    let remote = self.files_key.as_ref().and_then(|k| self.sftp.get(k));
                    self.files.scroll_by(remote, lines, self.side.grid.rows());
                } else if let Some(form) = &mut self.side.form {
                    form.scroll_by(lines, self.side.grid.rows());
                } else if let Some(view) = &mut self.side.import {
                    view.scroll_by(lines, self.side.grid.rows());
                } else {
                    let visible = SessionList::visible(self.side.grid.rows());
                    self.side.list.scroll_by(lines, visible);
                }
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
            cmd::IMPORT => return Some(Command::ImportSessions),
            cmd::RESTART_SHELL => self.start_shell(pane),
            cmd::CONNECT => self.connect_in_new_tab(pane),
            cmd::RECONNECT => self.reconnect(self.host_pane(pane)),
            cmd::NEW_TAB => {
                // A tmux pane's new tab is a new tmux window.
                if let Some(gateway) = self.panes.get(&pane).and_then(Pane::gateway) {
                    let actions = self
                        .tmux
                        .get_mut(&gateway)
                        .map(|t| t.new_tab(pane))
                        .unwrap_or_default();
                    self.run_tmux(gateway, actions);
                    return None;
                }
                let id = self.new_tab_pane();
                self.fill_like(id, pane);
            }
            cmd::SPLIT_RIGHT | cmd::SPLIT_DOWN => {
                let dir = if id == cmd::SPLIT_RIGHT {
                    Dir::Right
                } else {
                    Dir::Down
                };
                if let Some(gateway) = self.panes.get(&pane).and_then(Pane::gateway) {
                    let actions = self
                        .tmux
                        .get_mut(&gateway)
                        .map(|t| t.split(pane, dir))
                        .unwrap_or_default();
                    self.run_tmux(gateway, actions);
                    return None;
                }
                if let Some(new) = self.split(dir) {
                    self.fill_like(new, pane);
                }
            }
            cmd::CLOSE_PANE => return self.close_pane(pane),
            cmd::PANE_TO_TAB => {
                if !self.locked(pane) && self.ws.pane_to_tab(pane) {
                    self.after_structure_change();
                }
            }
            cmd::JOIN_TAB => {
                let next = if self.ws.active == 0 {
                    1
                } else {
                    self.ws.active - 1
                };
                if !self.locked_tab(self.ws.active)
                    && !self.locked_tab(next)
                    && self.ws.join_neighbour()
                {
                    self.after_structure_change();
                }
            }
            cmd::TMUX_ATTACH => {
                let host = self.host_pane(pane);
                if host != pane {
                    // Already in tmux: offer the other sessions.
                    let actions = self
                        .tmux
                        .get_mut(&host)
                        .map(Tmux::pick_session)
                        .unwrap_or_default();
                    self.run_tmux(host, actions);
                    return None;
                }
                let name = match self.panes.get(&pane).map(|p| &p.host) {
                    Some(Host::Ssh(s)) if s.state() == State::Connected && !s.prompt_active() => {
                        self.settings
                            .profile(&s.target.alias)
                            .and_then(|p| p.tmux.clone())
                            .unwrap_or_else(|| "main".to_owned())
                    }
                    _ => return Some(Command::Notice(NOT_IN_SHELL.into())),
                };
                self.attach_tmux(pane, &name, true);
            }
            cmd::DESKTOP => return self.open_desktop(pane, None),
            cmd::DESKTOP_TAB => return self.open_desktop(pane, Some(Protocol::Vnc)),
            cmd::DESKTOP_TAB_RDP => return self.open_desktop(pane, Some(Protocol::Rdp)),
            cmd::TMUX_DETACH | cmd::TMUX_PICK | cmd::TMUX_LAYOUT => {
                let host = self.host_pane(pane);
                let Some(t) = self.tmux.get_mut(&host).filter(|t| t.active()) else {
                    return Some(Command::Notice(
                        "This pane is not showing a tmux session. Session > Attach tmux \
                         starts one."
                            .into(),
                    ));
                };
                let actions = match id {
                    cmd::TMUX_DETACH => t.detach(),
                    cmd::TMUX_PICK => t.pick_session(),
                    _ => {
                        let mode = match t.mode() {
                            TmuxMode::Ferossh => TmuxMode::Tmux,
                            TmuxMode::Tmux => TmuxMode::Ferossh,
                        };
                        t.set_mode(mode)
                    }
                };
                self.run_tmux(host, actions);
                self.update_menu_checks();
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
            cmd::SESSIONS | cmd::FILES | cmd::FORWARDS => self.toggle_side(id, false),
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
            cmd::UPLOAD => {
                self.ensure_sftp(pane);
                let Some(key) = self.files_target(pane) else {
                    return Some(Command::Notice(NOT_CONNECTED.into()));
                };
                let listed = self.files_visible() && self.files_key.as_deref() == Some(&key);
                let dir = self.upload_dir(pane, listed);
                return Some(Command::PickUpload { key, dir });
            }
            cmd::DOWNLOADS => return Some(Command::OpenFolder(self.download_dir())),
            cmd::VERIFY => {
                self.settings.verify_transfers = !self.settings.verify_transfers;
                for remote in self.sftp.values_mut() {
                    remote.verify = self.settings.verify_transfers;
                }
                self.save_settings();
                self.update_menu_checks();
                self.refresh_chrome();
            }
            cmd::FOLLOW_CD => self.toggle_follow(),
            cmd::DISCONNECT => {
                let pane = self.host_pane(pane);
                self.tmux_lost(pane);
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
            cmd::LOG_VERIFY => return Some(Command::VerifyLog),
            cmd::SERIAL_BREAK => {
                let pane = self.focused_id();
                match self.panes.get(&pane).map(|p| &p.host) {
                    Some(Host::Serial {
                        port: Some(port), ..
                    }) => {
                        port.send_break();
                        self.show(pane, "\x1b[0;90m[break]\x1b[0m");
                    }
                    _ => {
                        return Some(Command::Notice(
                            "Send break is for serial consoles: open a COM port first \
                             (type COM3 at the connect prompt, or make a Serial console \
                             session)."
                                .into(),
                        ));
                    }
                }
            }
            cmd::BROADCAST_TAB | cmd::BROADCAST_ALL => {
                let wanted = if id == cmd::BROADCAST_TAB {
                    broadcast::Scope::Tab
                } else {
                    broadcast::Scope::All
                };
                self.broadcast = self.broadcast.toggle(wanted);
                self.update_menu_checks();
                self.update_title();
                self.refresh_chrome();
            }
            cmd::BROADCAST_SKIP => {
                let pane = self.focused_id();
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.no_broadcast = !p.no_broadcast;
                }
                self.update_menu_checks();
                self.refresh_chrome();
            }
            cmd::ZOOM_IN | cmd::ZOOM_OUT | cmd::ZOOM_RESET => {
                let font = self.panes.get(&pane)?.font.clone();
                let font = match id {
                    cmd::ZOOM_IN => font.zoomed(1),
                    cmd::ZOOM_OUT => font.zoomed(-1),
                    _ => FontSpec::new(&font.family, FontSpec::DEFAULT_PT),
                };
                self.set_font(pane, font);
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
                }
            }
            id if (cmd::THEME..cmd::THEME + Theme::builtin().len() as u32).contains(&id) => {
                if let Some(theme) = Theme::builtin().get((id - cmd::THEME) as usize) {
                    self.set_theme(pane, theme.clone());
                    // A tmux pane's theme is kept in the session.
                    self.sync_tmux();
                    self.refresh_chrome();
                    self.request_full_redraw();
                }
            }
            _ => {}
        }
        None
    }

    /// Changes a pane's font; the window keeps its size and the grid gains
    /// or loses cells right away, so a shell started next (the pty-req of
    /// a new connection) gets the rows and columns that fit.
    fn set_font(&mut self, id: PaneId, font: FontSpec) {
        let Ok(metrics) = self.renderer.metrics(&font, self.dpi) else {
            return;
        };
        if let Some(p) = self.panes.get_mut(&id) {
            p.font = font;
            p.metrics = metrics;
        }
        self.layout();
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
        let host = self.panes.get(&self.host_pane(pane)).map(|g| &g.host);
        let Some(Host::Ssh(s)) = host else {
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
        let own_layout = self
            .tmux
            .get(&self.host_pane(self.focused_id()))
            .is_some_and(|t| t.active() && t.mode() == TmuxMode::Ferossh);
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
                (
                    cmd::SESSIONS,
                    self.side.visible && self.side.view == SideView::Sessions,
                ),
                (
                    cmd::FILES,
                    self.side.visible && self.side.view == SideView::Files,
                ),
                (
                    cmd::FORWARDS,
                    self.side.visible && self.side.view == SideView::Forwards,
                ),
                (cmd::FOLLOW_CD, s.follow_directory),
                (cmd::VERIFY, s.verify_transfers),
                (cmd::SIDE_RIGHT, s.side_pane_right),
                (cmd::TAB_STRIP, s.tab_strip),
                (cmd::TMUX_LAYOUT, own_layout),
                (cmd::BROADCAST_TAB, self.broadcast == broadcast::Scope::Tab),
                (cmd::BROADCAST_ALL, self.broadcast == broadcast::Scope::All),
                (cmd::BROADCAST_SKIP, p.is_some_and(|p| p.no_broadcast)),
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
        if let Ok(m) = self.renderer.metrics(&self.strip_font, dpi) {
            self.strip_metrics = m;
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

    /// Where downloads go: the setting, else the user's Downloads folder.
    fn download_dir(&self) -> PathBuf {
        self.settings
            .download_folder
            .as_ref()
            .map_or_else(|| home().join("Downloads"), PathBuf::from)
    }

    fn files_visible(&self) -> bool {
        self.side.visible && self.side.view == SideView::Files
    }

    /// The transfer queue's key for pane `id`, if it is an SSH pane.
    fn pane_sftp_key(&self, id: PaneId) -> Option<String> {
        match &self.panes.get(&self.host_pane(id))?.host {
            Host::Ssh(s) => Some(sftp_key(&s.target)),
            _ => None,
        }
    }

    /// The queue key for pane `id` when it is logged in and its host's
    /// queue exists.
    fn files_target(&self, id: PaneId) -> Option<String> {
        let p = self.panes.get(&id)?;
        if !p.connected() {
            return None;
        }
        let key = self.pane_sftp_key(id)?;
        self.sftp.contains_key(&key).then_some(key)
    }

    /// Opens SFTP on pane `id`'s connection, if the pane is logged in and
    /// its host has no SFTP channel yet. The queue starts in the pane's
    /// directory when it reported one.
    fn ensure_sftp(&mut self, id: PaneId) {
        let Some(p) = self.panes.get(&id) else {
            return;
        };
        let Some(Host::Ssh(s)) = self.panes.get(&self.host_pane(id)).map(|g| &g.host) else {
            return;
        };
        if s.state() != State::Connected || self.worker.is_none() {
            return;
        }
        let key = sftp_key(&s.target);
        let label = s.target.label();
        let connection = s.connection;
        let cwd = p.cwd.clone().filter(|_| self.settings.follow_directory);
        let (verify, parallel) = (
            self.settings.verify_transfers,
            self.settings.parallel_transfers,
        );
        let remote = self.sftp.entry(key).or_insert_with(|| {
            let mut r = Remote::new(&label);
            r.verify = verify;
            if let Some(n) = parallel {
                r.parallel = n;
            }
            r
        });
        if remote.channel().is_some() {
            return;
        }
        if let Some(cwd) = cwd
            && remote.listing.path.is_empty()
        {
            remote.browse(&cwd);
        }
        // `self.sftp` is borrowed: the same as `new_channel`, inline.
        let channel = self.next_channel;
        self.next_channel = ((channel + 1) % FORWARD_IDS).max(1);
        remote.attach(channel, connection);
        self.flush_sftp();
    }

    /// A pane logged in: transfers waiting for its host carry on, and the
    /// file view opens SFTP if it shows that host.
    fn sftp_connected(&mut self, id: PaneId) {
        let Some(key) = self.pane_sftp_key(id) else {
            return;
        };
        let shown = self.files_visible() && self.files_key.as_deref() == Some(&key);
        let waiting = self.sftp.get(&key).is_some_and(Remote::wants_connection);
        if shown || waiting {
            self.ensure_sftp(id);
        }
    }

    /// Closes SFTP channels whose connection no logged-in pane uses any
    /// more, so they do not keep it open. Their transfers wait.
    fn prune_sftp(&mut self) {
        let live: Vec<(String, ChannelId)> = self
            .panes
            .values()
            .filter_map(|p| match &p.host {
                Host::Ssh(s) if s.state() == State::Connected => {
                    Some((sftp_key(&s.target), s.connection))
                }
                _ => None,
            })
            .collect();
        let mut closed = false;
        for (key, remote) in &mut self.sftp {
            if let Some(conn) = remote.connection()
                && !live.iter().any(|(k, c)| k == key && *c == conn)
            {
                remote.close();
                closed = true;
            }
        }
        if closed {
            self.flush_sftp();
        }
    }

    /// Sends what the queues asked for, and runs the progress timer while
    /// anything moves.
    fn flush_sftp(&mut self) {
        for remote in self.sftp.values_mut() {
            let out = remote.take_outbox();
            if let Some(w) = &self.worker {
                for msg in out {
                    w.link.send(msg);
                }
            }
        }
        let busy = self.sftp.values().any(Remote::busy);
        if busy != self.transfer_timer {
            self.transfer_timer = busy;
            // SAFETY: our window and timer.
            unsafe {
                if busy {
                    SetTimer(Some(self.hwnd), TIMER_TRANSFER, TRANSFER_TICK_MS, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_TRANSFER);
                }
            }
        }
    }

    fn on_transfer_timer(&mut self) {
        for remote in self.sftp.values_mut() {
            remote.poll();
        }
        self.flush_sftp();
        if self.files_visible() {
            self.refresh_chrome();
        }
    }

    /// Points the file view at the focused pane's host when focus moved
    /// to a pane on another host.
    fn sync_files(&mut self) {
        if !self.files_visible() {
            return;
        }
        let id = self.focused_id();
        let key = self.pane_sftp_key(id);
        if key == self.files_key {
            return;
        }
        self.files.reset();
        self.files_key.clone_from(&key);
        self.ensure_sftp(id);
        self.follow_pane(id);
    }

    /// Lists pane `id`'s reported directory, when following is on.
    fn follow_pane(&mut self, id: PaneId) {
        if !self.settings.follow_directory {
            return;
        }
        let Some(cwd) = self.panes.get(&id).and_then(|p| p.cwd.clone()) else {
            return;
        };
        let Some(key) = self.pane_sftp_key(id) else {
            return;
        };
        if let Some(remote) = self.sftp.get_mut(&key)
            && remote.listing.path != cwd
        {
            remote.browse(&cwd);
            self.flush_sftp();
        }
    }

    /// The shell reported its directory (OSC 7). Returns whether the file
    /// view changed.
    fn on_working_dir(&mut self, id: PaneId, dir: WorkingDir) -> bool {
        let Some(p) = self.panes.get_mut(&id) else {
            return false;
        };
        if !matches!(p.host, Host::Ssh(_) | Host::Tmux { .. }) {
            return false;
        }
        let host = dir.host.to_ascii_lowercase();
        let first = p.cwd_host.get_or_insert_with(|| host.clone());
        if *first != host {
            // A nested ssh on another host: its paths are not ours.
            p.cwd = None;
            return false;
        }
        p.cwd = Some(dir.path);
        if !self.files_visible() || id != self.focused_id() {
            return false;
        }
        self.follow_pane(id);
        true
    }

    fn toggle_follow(&mut self) {
        self.settings.follow_directory = !self.settings.follow_directory;
        self.save_settings();
        self.update_menu_checks();
        self.follow_pane(self.focused_id());
        self.refresh_chrome();
    }

    /// Carries out what the file view picked.
    fn files_action(&mut self, action: Option<FilesAction>) -> Option<Command> {
        let command = match action {
            Some(FilesAction::Leave) => {
                self.focus_terminal();
                None
            }
            Some(FilesAction::Delete(entry)) => self
                .files_key
                .clone()
                .map(|key| Command::ConfirmDelete { key, entry }),
            Some(FilesAction::PickUpload(dir)) => self
                .files_key
                .clone()
                .filter(|k| self.sftp.contains_key(k))
                .map(|key| Command::PickUpload { key, dir }),
            Some(FilesAction::OpenDownloads) => Some(Command::OpenFolder(self.download_dir())),
            Some(FilesAction::ToggleFollow) => {
                self.toggle_follow();
                None
            }
            None => None,
        };
        self.flush_sftp();
        self.refresh_chrome();
        command
    }

    /// Where an upload from pane `id` goes: the listed folder when
    /// `listed`, else the shell's directory, else the home folder, else
    /// wherever the server starts SFTP.
    fn upload_dir(&self, id: PaneId, listed: bool) -> Vec<u8> {
        let remote = self.pane_sftp_key(id).and_then(|k| self.sftp.get(&k));
        if listed
            && let Some(r) = remote
            && !r.listing.path.is_empty()
        {
            return r.listing.path.clone();
        }
        if let Some(cwd) = self.panes.get(&id).and_then(|p| p.cwd.clone()) {
            return cwd;
        }
        remote
            .and_then(|r| r.home.clone())
            .unwrap_or_else(|| b".".to_vec())
    }

    /// Queues uploads and shows the queue.
    fn upload_files(&mut self, key: &str, paths: &[PathBuf], dir: &[u8]) {
        let Some(remote) = self.sftp.get_mut(key) else {
            return;
        };
        for path in paths {
            if let Err(e) = remote.upload(path, dir) {
                remote.notice = Some(e);
            }
        }
        self.flush_sftp();
        if !self.files_visible() {
            self.show_side(SideView::Files, false);
        }
        self.refresh_chrome();
    }

    /// Where files dropped at client point (`x`, `y`) go: the queue key,
    /// a label for the prompt, and the remote folder.
    fn drop_target(&mut self, x: i32, y: i32) -> Option<(String, Vec<u8>)> {
        let (id, listed) = match self.spot(x, y) {
            Spot::Pane(id) => (id, false),
            Spot::Side(..) if self.files_visible() => (self.focused_id(), true),
            _ => return None,
        };
        self.ensure_sftp(id);
        let key = self.files_target(id)?;
        let listed = listed && self.files_key.as_deref() == Some(&key);
        Some((key, self.upload_dir(id, listed)))
    }

    /// Transfers not yet done, failed, skipped or cancelled.
    fn unfinished_transfers(&self) -> usize {
        self.sftp
            .values()
            .flat_map(Remote::jobs)
            .filter(|j| j.parent.is_none() && !j.state.finished())
            .count()
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
        // Leaving the window only detaches tmux; closing a tab closes its
        // tmux windows.
        ids.iter()
            .filter_map(|id| self.panes.get(id))
            .filter(|p| {
                let open = p.connected() || matches!(p.host, Host::Serial { port: Some(_), .. });
                open && (tab.is_some() || p.gateway().is_none())
            })
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
            v if v == key(b'E') && m.shift => Some(cmd::FILES),
            v if v == key(b'P') && m.shift => Some(cmd::FORWARDS),
            v if v == key(b'D') && m.shift => Some(cmd::DESKTOP),
            v if v == key(b'U') && m.shift => Some(cmd::UPLOAD),
            v if v == key(b'B') && m.shift => Some(cmd::BROADCAST_TAB),
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
    /// File > Import sessions: the side pane's import page.
    ImportSessions,
    /// Session > Check a log: the file picker, then the chain check.
    VerifyLog,
    /// The import page's file picker.
    PickImportFile,
    SaveLook,
    /// A message box with this text.
    Notice(String),
    /// Ask, then delete `entry` in the listed folder of `key`'s host.
    ConfirmDelete {
        key: String,
        entry: DirEntry,
    },
    /// The file picker, then upload into `dir` on `key`'s host.
    PickUpload {
        key: String,
        dir: Vec<u8>,
    },
    /// A menu of tmux sessions to switch gateway `gateway`'s client to.
    TmuxPicker {
        gateway: PaneId,
        names: Vec<String>,
    },
    /// The session manager's right-click menu, on this host if any.
    SessionMenu(Option<String>),
    /// Ask, then remove this saved session.
    DeleteSession(String),
    /// The file picker for the session form's key file.
    PickKey,
    /// Ask before a password manager command runs the first time.
    ConfirmPasswordTool {
        pane: PaneId,
        line: String,
    },
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

/// Session > Attach tmux on a pane that is not at a logged-in shell.
const NOT_IN_SHELL: &str = "Log in to an SSH host in this pane first; Attach tmux types \
                            `tmux -CC new-session -A -s main` at its shell (the session name \
                            comes from the host's Tmux setting in ferossh.conf).";

/// The size, in pixels, a remote desktop in a pane at `rect` should have.
fn desktop_size(rect: Rect) -> (u16, u16) {
    let side = |v: i32| u16::try_from(v.clamp(200, 8192)).unwrap_or(1024);
    (side(rect.w), side(rect.h))
}

/// Session > Open remote desktop on a pane that is not logged in.
const NO_DESKTOP_HOST: &str = "Log in to an SSH host in this pane first. Open remote desktop \
                               tunnels the host's Remote Desktop (RDP) port over that \
                               connection and opens Remote Desktop Connection on it.";

/// The message for file transfer without a logged-in pane.
const NOT_CONNECTED: &str = "Log in to an SSH host in this pane first; files go to and from \
                             the host of the focused pane.";

/// Starts Windows' Remote Desktop Connection on `addr`, from System32 so
/// nothing earlier on PATH stands in for it.
/// The keyboard layout's language id (0x409 for US English), which an
/// RDP server needs to read the scan codes the viewer sends.
#[cfg(feature = "viewer")]
fn keyboard_layout() -> u32 {
    // SAFETY: no arguments to get wrong; 0 is this thread.
    let hkl = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout(0) };
    (hkl.0 as usize & 0xffff) as u32
}

fn launch_rdp_client(addr: &str) -> std::io::Result<()> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let mstsc = PathBuf::from(root).join("System32").join("mstsc.exe");
    std::process::Command::new(mstsc)
        .arg(format!("/v:{addr}"))
        .spawn()
        .map(drop)
}

/// The key of a host's transfer queue: one per user, host name and port.
fn sftp_key(t: &Target) -> String {
    format!("{}@{}:{}", t.user, t.hostname.to_ascii_lowercase(), t.port)
}

/// The paths of files dropped from Explorer, and where they were dropped.
fn dropped_files(drop: HDROP) -> (Vec<PathBuf>, POINT) {
    let mut paths = Vec::new();
    let mut at = POINT::default();
    // SAFETY: `drop` is the handle WM_DROPFILES carried; each buffer is
    // sized from the length the first call reports, and DragFinish frees
    // the handle once.
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            if let Some(name) = buf.get(..got) {
                paths.push(PathBuf::from(String::from_utf16_lossy(name)));
            }
        }
        let _ = DragQueryPoint(drop, &mut at);
        DragFinish(drop);
    }
    (paths, at)
}

/// Files dropped on the window: asks, then uploads them to the host of
/// the pane they landed on.
fn drop_files(hwnd: HWND, drop: HDROP) {
    let (paths, at) = dropped_files(drop);
    if paths.is_empty() {
        return;
    }
    let Some((key, dir)) = with_window(|w| w.drop_target(at.x, at.y)).flatten() else {
        dialog(
            hwnd,
            "Drop files on a pane that is logged in to an SSH host to upload them there.",
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    };
    let what = match paths.as_slice() {
        [one] => format!(
            "\"{}\"",
            one.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        many => format!("{} items", many.len()),
    };
    let host = key.rsplit_once(':').map_or(key.as_str(), |(h, _)| h);
    let text = format!("Upload {what} to {} on {host}?", names::display(&dir));
    if dialog(hwnd, &text, MB_YESNO | MB_ICONQUESTION) == Some(IDYES) {
        with_window(|w| w.upload_files(&key, &paths, &dir));
    }
}

/// File > Upload files: the system file picker, several files at once.
fn pick_files(hwnd: HWND) -> Vec<PathBuf> {
    if DIALOG_OPEN.get() {
        return Vec::new();
    }
    let mut buf = vec![0u16; 64 * 1024];
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: w!("Upload files"),
        Flags: OFN_ALLOWMULTISELECT
            | OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    DIALOG_OPEN.set(true);
    // SAFETY: the struct is filled in as documented and `buf` outlives the
    // call; the dialog's modal loop holds no state borrow.
    let ok = unsafe { GetOpenFileNameW(&mut ofn) }.as_bool();
    DIALOG_OPEN.set(false);
    if !ok {
        return Vec::new();
    }
    // A folder, then the names, each NUL-terminated; one file comes as its
    // full path alone.
    let parts: Vec<String> = buf
        .split(|&c| c == 0)
        .take_while(|p| !p.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    match parts.as_slice() {
        [] => Vec::new(),
        [one] => vec![PathBuf::from(one)],
        [dir, names @ ..] => names.iter().map(|n| Path::new(dir).join(n)).collect(),
    }
}

/// The session form's Browse: one key file, starting in `~/.ssh`.
fn pick_key_file(hwnd: HWND) -> Option<PathBuf> {
    pick_file(hwnd, w!("Key file for this session"), &home().join(".ssh"))
}

/// The open-file dialog, starting in `dir`.
fn pick_file(hwnd: HWND, title: PCWSTR, dir: &std::path::Path) -> Option<PathBuf> {
    if DIALOG_OPEN.get() {
        return None;
    }
    let mut buf = vec![0u16; 4096];
    let dir = HSTRING::from(dir.as_os_str());
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrInitialDir: PCWSTR(dir.as_ptr()),
        lpstrTitle: title,
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    DIALOG_OPEN.set(true);
    // SAFETY: the struct is filled in as documented; `buf` and `dir`
    // outlive the call; the dialog's modal loop holds no state borrow.
    let ok = unsafe { GetOpenFileNameW(&mut ofn) }.as_bool();
    DIALOG_OPEN.set(false);
    if !ok {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    buf.get(..len)
        .map(|name| PathBuf::from(String::from_utf16_lossy(name)))
}

/// Which file a host the session form cannot change comes from, as the
/// user knows it.
fn host_file(host: &str) -> String {
    let imported = Client::ALL.into_iter().find(|&c| {
        std::fs::read_to_string(imported_path(c)).is_ok_and(|t| {
            ssh_config::hosts(&t, &ssh_config::NoIncludes)
                .iter()
                .any(|h| h.name.eq_ignore_ascii_case(host))
        })
    });
    match imported {
        Some(c) => format!("the {} import", c.name()),
        None => "~/.ssh/config".to_owned(),
    }
}

/// The session manager's right-click menu.
fn session_menu(hwnd: HWND, host: Option<String>) {
    const CONNECT: u32 = 1;
    const SPLIT: u32 = 2;
    const EDIT: u32 = 3;
    const DELETE: u32 = 4;
    const NEW: u32 = 5;
    // SAFETY: a popup menu of our own on our window, destroyed after use.
    // TPM_RETURNCMD returns the picked id, 0 for none.
    let picked = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        if host.is_some() {
            let _ = AppendMenuW(menu, MF_STRING, CONNECT as usize, w!("&Connect\tEnter"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                SPLIT as usize,
                w!("Open in a &split\tShift+Enter"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, EDIT as usize, w!("&Edit session...\tF2"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                DELETE as usize,
                w!("&Delete session...\tDel"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        }
        let _ = AppendMenuW(menu, MF_STRING, NEW as usize, w!("&New session...\tIns"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        u32::try_from(chosen.0).unwrap_or(0)
    };
    let action = match (picked, host) {
        (CONNECT, Some(host)) => SideAction::Open { host, split: false },
        (SPLIT, Some(host)) => SideAction::Open { host, split: true },
        (EDIT, Some(host)) => SideAction::Edit(host),
        (DELETE, Some(host)) => SideAction::Delete(host),
        (NEW, _) => SideAction::New,
        _ => return,
    };
    if let Some(Some(command)) = with_window(|w| w.side_action(Some(action))) {
        run_command(hwnd, command);
    }
}

/// What a side pane command does.
#[derive(Debug, PartialEq)]
enum SideToggle {
    Show,
    Hide,
    Focus,
}

/// `open`: the pane shows this view; `focused`: it has the keyboard.
fn side_toggle(open: bool, focused: bool, from_menu: bool) -> SideToggle {
    match (open, focused || from_menu) {
        (false, _) => SideToggle::Show,
        (true, true) => SideToggle::Hide,
        (true, false) => SideToggle::Focus,
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
            let moving = with_window(|w| w.unfinished_transfers()).unwrap_or(0);
            let live = with_window(|w| w.live_sessions(None)).unwrap_or(0);
            if moving > 0 {
                let text = format!(
                    "{moving} file transfer{s} not finished. Exit anyway?\n\n\
                     Partly downloaded files stay as .part files next to where they were going.",
                    s = if moving == 1 { " is" } else { "s are" },
                );
                if dialog(hwnd, &text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != Some(IDYES) {
                    return;
                }
            } else if live > 1 {
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
            let targets = with_window(|w| w.broadcast_targets(id)).unwrap_or_default();
            let others = if targets.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\nBroadcast input is on: it goes to {} other pane{} too.",
                    targets.len(),
                    plural(targets.len())
                )
            };
            if paste.needs_confirmation() || (!targets.is_empty() && paste.line_breaks > 0) {
                let lines = paste.line_breaks;
                let text = format!(
                    "The text you are pasting contains {lines} line break{s}. Each one acts as \
                     Enter, so a shell runs every line as soon as it arrives.{others}\n\n{}\n\n\
                     Paste it?",
                    clipboard_preview(&text, PREVIEW_CHARS),
                    s = plural(lines),
                );
                let answer = dialog(hwnd, &text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2);
                if answer != Some(IDYES) {
                    return;
                }
            }
            with_window(|w| {
                w.input(id, paste.bytes);
                w.send_typed(&targets, &Typed::Paste(text));
            });
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
        Command::Notice(text) => {
            dialog(hwnd, &text, MB_OK | MB_ICONINFORMATION);
        }
        Command::ConfirmDelete { key, entry } => {
            let what = if entry.attrs.is_dir() {
                "the folder"
            } else {
                "the file"
            };
            let path = with_window(|w| {
                w.sftp
                    .get(&key)
                    .map(|r| names::display(&names::join(&r.listing.path, &entry.name)))
            })
            .flatten()
            .unwrap_or_default();
            let note = if entry.attrs.is_dir() {
                " Only an empty folder can be deleted."
            } else {
                ""
            };
            let text = format!(
                "Delete {what} {path} on {key}?{note}\n\nThis cannot be undone.",
                key = key.rsplit_once(':').map_or(key.as_str(), |(h, _)| h)
            );
            if dialog(hwnd, &text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == Some(IDYES) {
                with_window(|w| {
                    if let Some(r) = w.sftp.get_mut(&key) {
                        r.delete(&entry);
                    }
                    w.flush_sftp();
                    w.refresh_chrome();
                });
            }
        }
        Command::TmuxPicker { gateway, names } => {
            let new = cmd::TMUX_SESSION + names.len() as u32;
            // SAFETY: a popup menu of our own on our window, destroyed
            // after use. TPM_RETURNCMD returns the picked id, 0 for none.
            let picked = unsafe {
                let Ok(menu) = CreatePopupMenu() else {
                    return;
                };
                for (id, name) in (cmd::TMUX_SESSION..).zip(&names) {
                    let text = crate::ssh::sanitize(name).replace('&', "&&");
                    let _ = AppendMenuW(menu, MF_STRING, id as usize, &HSTRING::from(text));
                }
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                let _ = AppendMenuW(menu, MF_STRING, new as usize, w!("&New tmux session"));
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let chosen = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_NONOTIFY,
                    pt.x,
                    pt.y,
                    None,
                    hwnd,
                    None,
                );
                let _ = DestroyMenu(menu);
                u32::try_from(chosen.0).unwrap_or(0)
            };
            if (cmd::TMUX_SESSION..=new).contains(&picked) {
                let name = names.get((picked - cmd::TMUX_SESSION) as usize).cloned();
                with_window(|w| {
                    let actions = w
                        .tmux
                        .get_mut(&gateway)
                        .map(|t| t.switch_session(name.as_deref()))
                        .unwrap_or_default();
                    w.run_tmux(gateway, actions);
                });
            }
        }
        Command::PickUpload { key, dir } => {
            let picked = pick_files(hwnd);
            if !picked.is_empty() {
                with_window(|w| w.upload_files(&key, &picked, &dir));
            }
        }
        Command::ImportSessions => {
            let (found, problems) = known_imports();
            with_window(|w| w.open_import(found, problems));
        }
        Command::PickImportFile => {
            let start = std::env::var_os("APPDATA").map_or_else(home, PathBuf::from);
            if let Some(path) = pick_file(hwnd, w!("Sessions file of another SSH client"), &start) {
                match read_import_file(&path) {
                    Ok(found) => {
                        with_window(|w| w.add_import(found));
                    }
                    Err(e) => {
                        dialog(hwnd, &e, MB_OK | MB_ICONWARNING);
                    }
                }
            }
        }
        Command::SessionMenu(host) => session_menu(hwnd, host),
        Command::VerifyLog => {
            if let Some(path) = pick_file(hwnd, w!("Check a log for changes"), &log_dir()) {
                let (text, ok) = check_log(&path);
                let icon = if ok {
                    MB_ICONINFORMATION
                } else {
                    MB_ICONWARNING
                };
                dialog(hwnd, &text, MB_OK | icon);
            }
        }
        Command::DeleteSession(name) => {
            let text = format!(
                "Delete the session {name}? Its look and extras in FeroSSH go with it.\n\n\
                 This cannot be undone."
            );
            if dialog(hwnd, &text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == Some(IDYES) {
                with_window(|w| w.delete_session(&name));
            }
        }
        Command::ConfirmPasswordTool { pane, line } => {
            let text = format!(
                "FeroSSH will run this command to get the login password, and type the first \
                 line it prints as the password:\n\n{}\n\nIt runs without a shell, a window \
                 or any input. Run it now and from now on?",
                line.chars().filter(|c| !c.is_control()).collect::<String>()
            );
            let yes =
                dialog(hwnd, &text, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == Some(IDYES);
            with_window(|w| {
                if yes {
                    approve_command(&line);
                    w.run_password_command(pane, &line);
                } else {
                    w.password_result(pane, Err("The password command was not run.".into()), "");
                }
            });
        }
        Command::PickKey => {
            if let Some(path) = pick_key_file(hwnd) {
                with_window(|w| {
                    if let Some(form) = &mut w.side.form {
                        form.set_key(&path.display().to_string());
                    }
                    w.refresh_chrome();
                });
            }
        }
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

/// Host names already taken outside `client`'s own import: in the user's
/// ssh_config, the saved sessions and the other imports. Lowercase.
fn taken_aliases(client: Client) -> Vec<String> {
    let home = home();
    let config = std::fs::read_to_string(home.join(".ssh").join("config")).unwrap_or_default();
    let includes = ssh_config::DirIncludes { home };
    let mut taken: Vec<String> = ssh_config::hosts(&config, &includes)
        .into_iter()
        .map(|h| h.name.to_ascii_lowercase())
        .collect();
    taken.extend(
        load_saved()
            .into_iter()
            .map(|h| h.alias.to_ascii_lowercase()),
    );
    for other in Client::ALL.into_iter().filter(|&c| c != client) {
        if let Ok(text) = std::fs::read_to_string(imported_path(other)) {
            taken.extend(
                ssh_config::hosts(&text, &ssh_config::NoIncludes)
                    .into_iter()
                    .map(|h| h.name.to_ascii_lowercase()),
            );
        }
    }
    taken
}

/// What the import page shows for sessions read from `origin`: distinct
/// names, and the ones a host elsewhere already has marked.
fn found(client: Client, origin: String, mut sessions: Vec<import::Session>) -> Found {
    import::distinct(&mut sessions);
    let taken = taken_aliases(client);
    let taken = sessions
        .iter()
        .map(|s| taken.contains(&s.host.alias.to_ascii_lowercase()))
        .collect();
    Found {
        client,
        origin,
        sessions,
        taken,
    }
}

/// Sessions of one client, with where they were read: "registry" or a
/// path.
type ImportSource = (Client, String, Vec<import::Session>);

/// The sessions of other clients on this account: PuTTY, KiTTY and WinSCP
/// in the registry, and the files where WinSCP, MobaXterm, mRemoteNG and
/// SecureCRT keep theirs. Files that could not be read come back as
/// problems, for the import page to show.
fn known_imports() -> (Vec<ImportSource>, Vec<String>) {
    let mut found: Vec<_> = Client::ALL
        .into_iter()
        .map(|c| (c, "registry".to_owned(), import_reg::sessions(c)))
        .filter(|(_, _, s)| !s.is_empty())
        .collect();
    let mut problems = Vec::new();
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let home = home();
    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(a) = &appdata {
        // WinSCP's sites when it is set to keep them in an INI file.
        files.push(a.join("WinSCP.ini"));
        files.push(a.join("MobaXterm").join("MobaXterm.ini"));
        files.push(a.join("mRemoteNG").join("confCons.xml"));
    }
    files.push(
        home.join("Documents")
            .join("MobaXterm")
            .join("MobaXterm.ini"),
    );
    for file in files.into_iter().filter(|f| f.is_file()) {
        match read_import_file(&file) {
            Ok(f) if !f.2.is_empty() => found.push(f),
            Ok(_) => {}
            Err(e) => problems.push(e),
        }
    }
    let crt = import_reg::securecrt_config()
        .map(PathBuf::from)
        .or_else(|| appdata.map(|a| a.join("VanDyke").join("Config")))
        .map(|c| c.join("Sessions"));
    if let Some(dir) = crt.filter(|d| d.is_dir()) {
        let sessions = securecrt_sessions(&dir);
        if !sessions.is_empty() {
            found.push((Client::SecureCrt, dir.display().to_string(), sessions));
        }
    }
    (found, problems)
}

/// Reads a file for an import, up to `max` bytes.
fn read_text(p: &std::path::Path, max: u64) -> std::result::Result<String, String> {
    let meta = std::fs::metadata(p).map_err(|e| format!("Could not read {}: {e}", p.display()))?;
    if meta.len() > max {
        return Err(format!("{} is too big to be a session file.", p.display()));
    }
    std::fs::read(p)
        .map(|b| {
            String::from_utf8_lossy(&b)
                .trim_start_matches('\u{feff}')
                .to_owned()
        })
        .map_err(|e| format!("Could not read {}: {e}", p.display()))
}

/// SecureCRT's sessions in its `Sessions` folder and the folders in it.
fn securecrt_sessions(root: &std::path::Path) -> Vec<import::Session> {
    let mut out = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), String::new(), 0)];
    let mut seen = 0;
    while let Some((dir, prefix, depth)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > 20_000 {
                return out;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth < 8 {
                dirs.push((entry.path(), format!("{prefix}{name}/"), depth + 1));
            } else if kind.is_file()
                && securecrt::is_session_file(&name)
                && let Ok(text) = read_text(&entry.path(), 1 << 20)
                && let Some(stem) = name.get(..name.len() - 4)
                && let Some(s) = securecrt::session(&format!("{prefix}{stem}"), &text)
            {
                out.push(s);
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Reads a file the user picked for an import: a `WinSCP.ini`, a
/// `MobaXterm.ini` (or `.mxtsessions`), mRemoteNG's `confCons.xml`, one
/// of SecureCRT's session files (which brings the `Sessions` folder it is
/// in), or one of a portable KiTTY's session files (which brings the
/// folder it is in).
fn read_import_file(path: &std::path::Path) -> std::result::Result<ImportSource, String> {
    let text = read_text(path, 16 << 20)?;
    let origin = path.display().to_string();
    let none = |what: &str| format!("{} has no {what}.", path.display());
    let line_starts = |prefix: &str| text.lines().any(|l| l.trim_start().starts_with(prefix));
    if line_starts("[Sessions\\") {
        let sessions = winscp::from_ini(&text);
        if sessions.is_empty() {
            return Err(none("SFTP or SCP sites"));
        }
        return Ok((Client::WinScp, origin, sessions));
    }
    if line_starts("[Bookmarks") {
        let sessions = mobaxterm::from_ini(&text);
        if sessions.is_empty() {
            return Err(none("SSH, SFTP, RDP or VNC sessions"));
        }
        return Ok((Client::MobaXterm, origin, sessions));
    }
    if text.contains("Connections") && text.trim_start().starts_with('<') {
        let sessions = mremoteng::from_xml(&text)?;
        if sessions.is_empty() {
            return Err(none("SSH, RDP or VNC connections"));
        }
        return Ok((Client::MRemoteNg, origin, sessions));
    }
    let file_name = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    let name = file_name(path).unwrap_or_default();
    if line_starts("S:\"Hostname\"=") || line_starts("S:\"Protocol Name\"=") {
        // The whole Sessions folder: the nearest one up, else this folder.
        let root = path
            .ancestors()
            .skip(1)
            .find(|a| {
                a.file_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case("Sessions"))
            })
            .or_else(|| path.parent())
            .ok_or_else(|| none("sessions"))?;
        let sessions = securecrt_sessions(root);
        if sessions.is_empty() {
            return Err(format!("{} has no SSH sessions.", root.display()));
        }
        return Ok((Client::SecureCrt, root.display().to_string(), sessions));
    }
    let unknown = || {
        format!(
            "{} is not a sessions file FeroSSH knows. It reads WinSCP.ini, MobaXterm.ini, \
             mRemoteNG's confCons.xml, and the session files of SecureCRT and of a portable \
             KiTTY.",
            path.display()
        )
    };
    if text.len() > 1 << 20 || putty::file_session(&name, &text).is_none() {
        return Err(unknown());
    }
    let dir = path.parent().ok_or_else(unknown)?;
    let mut sessions = Vec::new();
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("Could not read {}: {e}", dir.display()))?;
    for entry in entries.flatten().take(10_000) {
        let p = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if let (Some(name), Ok(text)) = (file_name(&p), read_text(&p, 1 << 20))
            && let Some(s) = putty::file_session(&name, &text)
        {
            sessions.push(s);
        }
    }
    sessions.sort_by_key(|a| a.name.to_lowercase());
    Ok((Client::Kitty, dir.display().to_string(), sessions))
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
        if with_window(|w| w.desktop_message(msg, wparam, lparam)) == Some(true) {
            return LRESULT(0);
        }
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
            WM_TIMER if wparam.0 == TIMER_TRANSFER => {
                with_window(Window::on_transfer_timer);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_DESKTOP_SIZE => {
                with_window(Window::on_desktop_size_timer);
                LRESULT(0)
            }
            WM_DROPFILES => {
                drop_files(hwnd, HDROP(wparam.0 as *mut c_void));
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
                let id = loword(wparam.0);
                if matches!(id, cmd::SESSIONS | cmd::FILES | cmd::FORWARDS) {
                    with_window(|w| w.toggle_side(id, true));
                } else {
                    run_command(hwnd, Command::Id(id));
                }
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
                    // tmux keeps its sessions; leave them detached.
                    let gateways: Vec<PaneId> = w.tmux.keys().copied().collect();
                    for g in gateways {
                        let actions = w
                            .tmux
                            .get_mut(&g)
                            .filter(|t| t.active())
                            .map(Tmux::detach)
                            .unwrap_or_default();
                        w.run_tmux(g, actions);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_click_hides_an_open_side_pane_at_once() {
        assert_eq!(side_toggle(true, false, true), SideToggle::Hide);
        assert_eq!(side_toggle(true, true, true), SideToggle::Hide);
        assert_eq!(side_toggle(false, false, true), SideToggle::Show);
    }

    #[test]
    fn shortcut_focuses_an_open_side_pane_before_hiding_it() {
        assert_eq!(side_toggle(true, false, false), SideToggle::Focus);
        assert_eq!(side_toggle(true, true, false), SideToggle::Hide);
        assert_eq!(side_toggle(false, true, false), SideToggle::Show);
    }
}
