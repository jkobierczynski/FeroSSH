//! The file transfer pane as native Windows controls: a Local list and a
//! Remote list of files with icons, size and date, side by side or one
//! above the other, with arrow buttons in the gap between them that move
//! the selection across; above them a filter for both, and below them the
//! transfer queue (with the overwrite question when one is asked).
//!
//! The remote listing and the queue come from the main window as a
//! [`View`]; the Local list's folder is read here. What the user does goes
//! back as an [`Event`]. Files are dragged from one list to the other (or
//! onto a folder in it), and dropped from Explorer onto the Remote list
//! (the main window gets those, and asks [`drop_at`] where they landed).
//! Keys work as in the other native tools: [`pre_translate`] passes
//! Ctrl+Shift shortcuts on to the main window and gives Tab, Enter and
//! Escape their dialog meaning.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use ferossh_sftp::{Choice, JobId, State as JobState, names};
use windows::Win32::Foundation::{COLORREF, FILETIME, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, ClientToScreen, CreateSolidBrush,
    DT_CALCRECT, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
    EndPaint, FillRect, GetDC, HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, ReleaseDC,
    ScreenToClient, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDDS_SUBITEM, CDRF_DODEFAULT,
    CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYSUBITEMDRAW, CDRF_SKIPDEFAULT, EM_SETCUEBANNER, EM_SETSEL,
    LIST_VIEW_ITEM_STATE_FLAGS, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCFMT_LEFT, LVCFMT_RIGHT,
    LVCOLUMNW, LVHITTESTINFO, LVIF_IMAGE, LVIF_PARAM, LVIF_TEXT, LVIR_BOUNDS, LVIS_DROPHILITED,
    LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE, LVM_GETITEMCOUNT,
    LVM_GETITEMRECT, LVM_GETNEXTITEM, LVM_GETSUBITEMRECT, LVM_HITTEST, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETIMAGELIST,
    LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_BEGINDRAG, LVN_KEYDOWN, LVNI_SELECTED,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHAREIMAGELISTS, LVS_SHOWSELALWAYS,
    LVS_SINGLESEL, LVSIL_SMALL, NM_CUSTOMDRAW, NM_DBLCLK, NM_SETFOCUS, NMHDR, NMITEMACTIVATE,
    NMLVCUSTOMDRAW, NMLVKEYDOWN, SetWindowTheme, TOOLTIPS_CLASSW, TTF_IDISHWND, TTF_SUBCLASS,
    TTM_ADDTOOLW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW, WC_BUTTONW, WC_EDITW, WC_LISTVIEWW,
    WC_STATICW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_CONTROL, VK_DELETE, VK_F5, VK_SHIFT,
};
use windows::Win32::UI::Shell::{
    SHFILEINFOW, SHGFI_SMALLICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_CHECKBOX, BS_PUSHBUTTON,
    BS_PUSHLIKE, CS_HREDRAW, CS_VREDRAW, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, EN_CHANGE, EN_SETFOCUS, ES_AUTOHSCROLL, GetClientRect, GetCursorPos, HMENU,
    IDC_ARROW, IDC_HAND, IDC_NO, IDC_SIZENS, IDC_SIZEWE, IDCANCEL, IDOK, IsChild, IsDialogMessageW,
    IsWindowVisible, LoadCursorW, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
    MoveWindow, RegisterClassW, SW_HIDE, SW_SHOWNA, SendMessageW, SetCursor, SetWindowTextW,
    ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CAPTURECHANGED, WM_COMMAND, WM_CONTEXTMENU, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NOTIFY, WM_PAINT, WM_SETCURSOR, WM_SYSCHAR, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_EX_TOPMOST, WS_POPUP, WS_TABSTOP, WS_VISIBLE, WindowFromPoint,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::files::{
    self, Arrange, BackStep, FileRow, Follow, LocalPane, QueueRow, SyncLocal, SyncPending,
    SyncStep, filtered, local_rows,
};
use crate::layout::Rect;
use crate::native::{fonts, key_down, pass_edge, scale, search_text, set_font, wide};
use ferossh_config::Bookmark;

/// What the main window shows in the pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// The host the Remote list is on; `None` when the focused pane is not
    /// logged in to an SSH host.
    pub host: Option<String>,
    /// The transfer queue the Remote list is on (`host` is only its
    /// label, which two queues can share).
    pub key: Option<String>,
    /// The listed remote folder; empty until one is listed.
    pub remote_dir: Vec<u8>,
    /// A remote folder is being listed.
    pub remote_loading: bool,
    /// All of its rows, unfiltered.
    pub remote_rows: Vec<FileRow>,
    /// A line under the lists, and whether it is a problem.
    pub status: Option<(String, bool)>,
    pub queue: Vec<QueueRow>,
    /// A file that exists where it goes: what to do?
    pub question: Option<(JobId, String)>,
    pub follow: Follow,
    /// The download folder: the Local list moves there when it changes.
    pub base: PathBuf,
    /// Downloads finished so far, to read the Local list again.
    pub landed: usize,
    pub arrange: Arrange,
    /// Every bookmark; the menu leaves out those for other hosts.
    pub bookmarks: Vec<Bookmark>,
    /// The connection the Remote list shows, to match bookmarks to.
    pub alias: Option<String>,
    /// The Local folder the host's connection names, opened when the
    /// pane turns to that host.
    pub local_start: Option<PathBuf>,
}

/// What a queued transfer should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOp {
    Pause,
    Resume,
    Cancel,
    Retry,
}

/// What the user did in the pane.
#[derive(Debug)]
pub enum Event {
    /// List this remote folder.
    Browse(Vec<u8>),
    /// Sync's go-back: list its remote folder, but only while the view
    /// still shows its queue and connection.
    SyncBack(files::SyncBack),
    Home,
    Refresh,
    /// Download these entries of the listed remote folder into `into`.
    Download {
        keys: Vec<Vec<u8>>,
        into: PathBuf,
    },
    /// Upload these local files and folders into remote folder `dir`.
    Upload {
        paths: Vec<PathBuf>,
        dir: Vec<u8>,
    },
    /// Local files and folders dragged out of the pane and let go at
    /// `at` (main window client coordinates), on a terminal pane say:
    /// handled like files dropped from Explorer.
    DropOut {
        paths: Vec<PathBuf>,
        at: POINT,
    },
    /// Ask, then delete these entries of the listed remote folder.
    Delete(Vec<Vec<u8>>),
    /// The Local list moved here: downloads go here from now on.
    SetLocal(PathBuf),
    PickLocal,
    PickUpload,
    /// Open this folder in Explorer.
    OpenLocal(PathBuf),
    ToggleFollow,
    Arrange(Arrange),
    /// The bookmarks as they are now (one was added, renamed or deleted).
    Bookmarks(Vec<Bookmark>),
    Job(JobId, JobOp),
    ClearFinished,
    Answer(JobId, Choice, bool),
    /// One of the controls got the keyboard.
    Focused,
    /// Escape with no filter: back to the terminal.
    Leave,
}

/// Carries out an [`Event`]; `HWND` is the main window.
pub type Handler = fn(HWND, Event);

/// Where files dropped from Explorer landed.
#[derive(Debug, PartialEq, Eq)]
pub enum DropAt {
    /// On the Remote list: into its folder, or into the folder row named
    /// this.
    Remote(Option<Vec<u8>>),
    /// On the Local list, which takes no drops.
    Local,
    /// Elsewhere in the pane.
    Pane,
}

const ID_FOLLOW: usize = 300;
const ID_LAYOUT: usize = 301;
const ID_FILTER: usize = 302;
const ID_BOOKMARKS: usize = 303;
const ID_SYNC: usize = 304;
const ID_REFRESH: usize = 305;
const ID_NAME: usize = 370;
const ID_NAME_SAVE: usize = 371;
const ID_NAME_CANCEL: usize = 372;
const ID_NAME_HOST: usize = 373;
const ID_LOCAL_UP: usize = 310;
const ID_LOCAL_PATH: usize = 311;
const ID_LOCAL_PICK: usize = 312;
const ID_LOCAL_LIST: usize = 313;
const ID_REMOTE_UP: usize = 320;
const ID_REMOTE_PATH: usize = 321;
const ID_REMOTE_HOME: usize = 322;
const ID_REMOTE_LIST: usize = 323;
const ID_UPLOAD: usize = 330;
const ID_DOWNLOAD: usize = 331;
const ID_STATUS: usize = 340;
const ID_ASK: usize = 350;
const ID_OVERWRITE: usize = 351;
const ID_SKIP: usize = 352;
const ID_KEEP: usize = 353;
const ID_ALL: usize = 354;
const ID_QUEUE: usize = 360;

/// Columns: title, width in DIPs, right-aligned.
const FILE_COLUMNS: [(&str, i32, bool); 4] = [
    ("Name", 190, false),
    ("Size", 70, true),
    ("Modified", 116, false),
    ("Rights", 82, false),
];
const QUEUE_COLUMNS: [(&str, i32, bool); 4] = [
    ("Transfer", 200, false),
    ("Direction", 80, false),
    ("Progress", 110, false),
    ("Status", 180, false),
];
/// The queue's tallest, in rows.
const QUEUE_ROWS: i32 = 5;

#[derive(Clone, Copy)]
struct Handles {
    main: HWND,
    panel: HWND,
    follow: HWND,
    layout: HWND,
    bookmarks: HWND,
    sync: HWND,
    /// Reads both lists again.
    refresh: HWND,
    filter: HWND,
    /// The bookmark name row: the name, Save, Cancel, only this host.
    name: [HWND; 4],
    local: Side,
    remote: Side,
    upload: HWND,
    download: HWND,
    status: HWND,
    ask: HWND,
    answers: [HWND; 3],
    all: HWND,
    queue: HWND,
}

#[derive(Clone, Copy)]
struct Side {
    up: HWND,
    path: HWND,
    extra: HWND,
    list: HWND,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drag {
    /// The gap between the lists, to share the room differently.
    Split,
    /// Files from the Remote list (`true`) or the Local list.
    Files { remote: bool },
}

struct State {
    h: Handles,
    handler: Handler,
    font: HFONT,
    title_font: HFONT,
    dpi: u32,
    view: Option<View>,
    local: LocalPane,
    /// Rows as shown in each list, filtered.
    local_shown: Vec<FileRow>,
    remote_shown: Vec<FileRow>,
    queue_shown: Vec<QueueRow>,
    /// The folders the lists show, to start a new folder at the top.
    local_dir: Option<PathBuf>,
    remote_dir: Vec<u8>,
    /// The transfer queue the Remote list shows.
    remote_key: Option<String>,
    /// A row to select once its folder is listed (the folder just left).
    select_local: Option<Vec<u8>>,
    select_remote: Option<Vec<u8>>,
    filter: String,
    arrange: Arrange,
    /// The panel's size as laid out, the lists' area and the gap.
    size: (i32, i32),
    /// How much of the panel's width the terminal leaves uncovered: the
    /// buttons at the right edge move left to stay in it.
    visible: i32,
    lists: Rect,
    gap: Rect,
    /// Where the "Local" and "Remote" labels and the title go.
    labels: [Rect; 2],
    drag: Option<Drag>,
    /// The row lit up as the folder a drag would drop into.
    lit: Option<(HWND, i32)>,
    /// A problem with the Local list (a folder that cannot be read).
    local_error: Option<String>,
    icons: HashMap<String, i32>,
    /// Sync browsing: opening a folder, or going up, in one list does the
    /// same in the other.
    sync: bool,
    /// A bookmark name being typed: for a new bookmark, or for renaming
    /// bookmark `Some(i)`.
    naming: Option<Option<usize>>,
    /// The host the pane showed, to open its connection's Local folder
    /// when it turns to another.
    shown_host: Option<(Option<String>, Option<String>)>,
    /// A Sync browsing move waiting for its remote folder to list.
    sync_pending: Option<SyncPending>,
    /// Where the title row's buttons start: the host label ends before.
    title_buttons_x: i32,
    /// Inside `set_view`: the main window's state is borrowed, so events
    /// wait in `deferred` and go out from a posted message.
    in_view: bool,
    deferred: Vec<Event>,
    /// A Sync go-back waiting for the Remote list to settle.
    going_back: Option<files::SyncBack>,
}

/// Posted to the main window when events wait in `deferred`: it hands
/// them to [`take_deferred`].
pub const WM_FILES_DEFERRED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 41;

/// The events that waited while the view was being set.
pub fn take_deferred() -> Vec<Event> {
    with(|s| std::mem::take(&mut s.deferred)).unwrap_or_default()
}

/// Ends `set_view`'s deferral, posting for any waiting events.
struct ViewGuard;

impl Drop for ViewGuard {
    fn drop(&mut self) {
        let post = with(|s| {
            s.in_view = false;
            (!s.deferred.is_empty()).then_some(s.h.main)
        })
        .flatten();
        if let Some(main) = post {
            // SAFETY: posting to our own main window.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(main),
                    WM_FILES_DEFERRED,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn handles() -> Option<Handles> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|s| s.h))
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// The system image list's small icon for a folder or for a file named
/// like `name` (by its extension; nothing is read from disk).
fn icon(icons: &mut HashMap<String, i32>, name: &str, dir: bool) -> i32 {
    let key = if dir {
        "<dir>".to_owned()
    } else {
        match name.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() => format!(".{}", ext.to_lowercase()),
            _ => "<file>".to_owned(),
        }
    };
    if let Some(&i) = icons.get(&key) {
        return i;
    }
    let probe = if dir {
        wide("folder")
    } else {
        wide(&format!("file{key}"))
    };
    let mut info = SHFILEINFOW::default();
    // SAFETY: a valid string and a correctly sized struct for the call to
    // fill; with SHGFI_USEFILEATTRIBUTES nothing is read from disk.
    unsafe {
        SHGetFileInfoW(
            PCWSTR(probe.as_ptr()),
            if dir {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            },
            Some(&raw mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
        );
    }
    icons.insert(key, info.iIcon);
    info.iIcon
}

/// The system image list of small icons.
fn system_icons() -> isize {
    let probe = wide("folder");
    let mut info = SHFILEINFOW::default();
    // SAFETY: as in `icon`; the list belongs to the system and is shared.
    unsafe {
        SHGetFileInfoW(
            PCWSTR(probe.as_ptr()),
            FILE_ATTRIBUTE_DIRECTORY,
            Some(&raw mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
        ) as isize
    }
}

/// `secs` since 1970 as local date and time, `2026-11-09 08:15`.
fn when(secs: Option<i64>) -> String {
    use windows::Win32::Foundation::SYSTEMTIME;
    let Some(secs) = secs else {
        return String::new();
    };
    let ticks = (secs + 11_644_473_600).max(0) as u64 * 10_000_000;
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: plain conversions between structs on the stack.
    let ok = unsafe {
        FileTimeToSystemTime(&ft, &mut utc).is_ok()
            && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
    };
    if !ok {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
    )
}

fn add_columns(list: HWND, columns: &[(&str, i32, bool)], dpi: u32) {
    for (i, (title, width, right)) in columns.iter().enumerate() {
        let mut text = wide(title);
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT,
            fmt: if *right { LVCFMT_RIGHT } else { LVCFMT_LEFT },
            cx: scale(*width, dpi),
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        // SAFETY: our own control; the struct and its text live through
        // the call, which copies them.
        unsafe {
            SendMessageW(
                list,
                LVM_INSERTCOLUMNW,
                Some(WPARAM(i)),
                Some(LPARAM(&raw const col as isize)),
            );
        }
    }
}

fn set_widths(list: HWND, columns: &[(&str, i32, bool)], dpi: u32) {
    for (i, (_, w, _)) in columns.iter().enumerate() {
        // SAFETY: our own control.
        unsafe {
            SendMessageW(
                list,
                LVM_SETCOLUMNWIDTH,
                Some(WPARAM(i)),
                Some(LPARAM(scale(*w, dpi) as isize)),
            );
        }
    }
}

/// Creates the pane, hidden, as a child of `main`.
pub fn create(main: HWND, handler: Handler, dpi: u32) -> windows::core::Result<()> {
    // SAFETY: registering a class and creating child windows with valid
    // static strings; they are destroyed with `main`.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("FeroSSHFiles");
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
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
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
        let tab = WS_TABSTOP.0;
        let button =
            |text: PCWSTR, id: usize| child(WC_BUTTONW, text, tab | BS_PUSHBUTTON as u32, id);
        let edit = |id: usize| {
            child(
                WC_EDITW,
                w!(""),
                tab | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                id,
            )
        };
        let list = |id: usize, single: bool| {
            let style = LVS_REPORT
                | LVS_SHOWSELALWAYS
                | LVS_SHAREIMAGELISTS
                | if single { LVS_SINGLESEL } else { 0 };
            let l = child(WC_LISTVIEWW, w!(""), tab | style, id)?;
            let _ = SetWindowTheme(l, w!("Explorer"), PCWSTR::null());
            let ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER;
            SendMessageW(
                l,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                Some(WPARAM(ex as usize)),
                Some(LPARAM(ex as isize)),
            );
            Ok::<HWND, windows::core::Error>(l)
        };
        let follow = child(
            WC_BUTTONW,
            w!("Follow terminal"),
            tab | (BS_CHECKBOX | BS_PUSHLIKE) as u32,
            ID_FOLLOW,
        )?;
        let layout = button(w!("Layout ▾"), ID_LAYOUT)?;
        let bookmarks = button(w!("★ Bookmarks ▾"), ID_BOOKMARKS)?;
        let refresh = button(w!("↻ Refresh"), ID_REFRESH)?;
        let sync = child(
            WC_BUTTONW,
            w!("⇆ Sync"),
            tab | (BS_CHECKBOX | BS_PUSHLIKE) as u32,
            ID_SYNC,
        )?;
        let filter = edit(ID_FILTER)?;
        let name = [
            edit(ID_NAME)?,
            button(w!("Save"), ID_NAME_SAVE)?,
            button(w!("Cancel"), ID_NAME_CANCEL)?,
            child(
                WC_BUTTONW,
                w!("Only for this host"),
                tab | BS_AUTOCHECKBOX as u32,
                ID_NAME_HOST,
            )?,
        ];
        let name_cue = wide("Bookmark name");
        SendMessageW(
            name[0],
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(name_cue.as_ptr() as isize)),
        );
        for c in name {
            let _ = ShowWindow(c, SW_HIDE);
        }
        let cue = wide("Filter by name (both lists)");
        SendMessageW(
            filter,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        let icons = system_icons();
        let side = |up: usize, path: usize, extra: PCWSTR, extra_id: usize, list_id: usize| {
            let s = Side {
                up: button(w!("↑"), up)?,
                path: edit(path)?,
                extra: button(extra, extra_id)?,
                list: list(list_id, false)?,
            };
            SendMessageW(
                s.list,
                LVM_SETIMAGELIST,
                Some(WPARAM(LVSIL_SMALL as usize)),
                Some(LPARAM(icons)),
            );
            Ok::<Side, windows::core::Error>(s)
        };
        let local = side(
            ID_LOCAL_UP,
            ID_LOCAL_PATH,
            w!("..."),
            ID_LOCAL_PICK,
            ID_LOCAL_LIST,
        )?;
        let remote = side(
            ID_REMOTE_UP,
            ID_REMOTE_PATH,
            w!("⌂"),
            ID_REMOTE_HOME,
            ID_REMOTE_LIST,
        )?;
        add_columns(local.list, FILE_COLUMNS.get(..3).unwrap_or_default(), dpi);
        add_columns(remote.list, &FILE_COLUMNS, dpi);
        let upload = button(w!("→"), ID_UPLOAD)?;
        let download = button(w!("←"), ID_DOWNLOAD)?;
        let status = child(WC_STATICW, w!(""), 0x80 /* SS_NOPREFIX */, ID_STATUS)?;
        let ask = child(WC_STATICW, w!(""), 0x80, ID_ASK)?;
        let answers = [
            button(w!("&Overwrite"), ID_OVERWRITE)?,
            button(w!("&Skip"), ID_SKIP)?,
            button(w!("&Keep both"), ID_KEEP)?,
        ];
        let all = child(
            WC_BUTTONW,
            w!("For &all"),
            tab | BS_AUTOCHECKBOX as u32,
            ID_ALL,
        )?;
        let queue = list(ID_QUEUE, true)?;
        add_columns(queue, &QUEUE_COLUMNS, dpi);
        let (font, title_font) = fonts(dpi);
        let h = Handles {
            main,
            panel,
            follow,
            layout,
            bookmarks,
            sync,
            refresh,
            filter,
            name,
            local,
            remote,
            upload,
            download,
            status,
            ask,
            answers,
            all,
            queue,
        };
        for c in controls(&h) {
            set_font(c, font);
        }
        add_tips(panel, instance.into(), &h);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                h,
                handler,
                font,
                title_font,
                dpi,
                view: None,
                local: LocalPane::default(),
                local_shown: Vec::new(),
                remote_shown: Vec::new(),
                queue_shown: Vec::new(),
                local_dir: None,
                remote_dir: Vec::new(),
                remote_key: None,
                select_local: None,
                select_remote: None,
                filter: String::new(),
                arrange: Arrange::default(),
                size: (0, 0),
                visible: 0,
                lists: Rect::default(),
                gap: Rect::default(),
                labels: [Rect::default(); 2],
                drag: None,
                lit: None,
                local_error: None,
                icons: HashMap::new(),
                sync: false,
                naming: None,
                sync_pending: None,
                title_buttons_x: i32::MAX,
                in_view: false,
                deferred: Vec::new(),
                going_back: None,
                shown_host: None,
            });
        });
        pass_edge(panel);
    }
    Ok(())
}

fn controls(h: &Handles) -> Vec<HWND> {
    let mut v = vec![h.follow, h.layout, h.bookmarks, h.sync, h.refresh, h.filter];
    v.extend(h.name);
    for s in [h.local, h.remote] {
        v.extend([s.up, s.path, s.extra, s.list]);
    }
    v.extend([h.upload, h.download, h.status, h.ask, h.all, h.queue]);
    v.extend(h.answers);
    v
}

/// Tooltips for the buttons whose label is a sign.
fn add_tips(panel: HWND, instance: windows::Win32::Foundation::HINSTANCE, h: &Handles) {
    // SAFETY: a tooltip window of our own; each struct and its text live
    // through the call, which copies them.
    unsafe {
        let Ok(tip) = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
            0,
            0,
            0,
            0,
            Some(panel),
            None,
            Some(instance),
            None,
        ) else {
            return;
        };
        for (control, text) in [
            (
                h.upload,
                "Upload the selected local files to the Remote folder",
            ),
            (
                h.download,
                "Download the selected remote files to the Local folder",
            ),
            (h.local.up, "Up one folder (Backspace)"),
            (h.remote.up, "Up one folder (Backspace)"),
            (h.local.extra, "Choose the Local folder"),
            (h.remote.extra, "The home folder"),
            (h.follow, "The Remote list follows the terminal's folder"),
            (
                h.sync,
                "Sync browsing: opening a folder or going up in one list does the same in the other",
            ),
        ] {
            let mut text = wide(text);
            let info = TTTOOLINFOW {
                cbSize: size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd: panel,
                uId: control.0 as usize,
                lpszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                tip,
                TTM_ADDTOOLW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const info as isize)),
            );
        }
    }
}

/// Puts the pane over `at`, laid out for the tool's whole `width`; `None`
/// hides it.
pub fn place(at: Option<Rect>, width: i32, dpi: u32) {
    let Some(h) = handles() else {
        return;
    };
    let Some(at) = at else {
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(h.panel, SW_HIDE);
        }
        return;
    };
    let refont = with(|s| {
        if s.dpi == dpi {
            return None;
        }
        s.dpi = dpi;
        let (font, title) = fonts(dpi);
        Some((
            font,
            std::mem::replace(&mut s.font, font),
            std::mem::replace(&mut s.title_font, title),
        ))
    })
    .flatten();
    if let Some((font, old, old_title)) = &refont {
        let (font, old, old_title) = (*font, *old, *old_title);
        for c in controls(&h) {
            set_font(c, font);
        }
        set_widths(h.local.list, FILE_COLUMNS.get(..3).unwrap_or_default(), dpi);
        set_widths(h.remote.list, &FILE_COLUMNS, dpi);
        set_widths(h.queue, &QUEUE_COLUMNS, dpi);
        // SAFETY: no control uses the old fonts any more.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(old.0));
            let _ = DeleteObject(HGDIOBJ(old_title.0));
        }
    }
    // SAFETY: moving our own window.
    unsafe {
        let _ = MoveWindow(h.panel, at.x, at.y, at.w, at.h, true);
    }
    let resized = with(|s| {
        let sized = std::mem::replace(&mut s.size, (width, at.h)) != (width, at.h);
        let uncovered = std::mem::replace(&mut s.visible, at.w) != at.w;
        sized || uncovered
    })
    .unwrap_or(false);
    if resized || refont.is_some() {
        layout(&h);
    }
    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(h.panel, SW_SHOWNA);
    }
}

/// Where the title ("File transfer" and the host) ends, as `paint` draws it.
fn title_end(h: &Handles, dpi: u32) -> i32 {
    let Some((title_font, font, host)) = with(|s| {
        (
            s.title_font,
            s.font,
            s.view.as_ref().and_then(|v| v.host.clone()),
        )
    }) else {
        return 0;
    };
    let host = host.unwrap_or_else(|| "no SSH host".into());
    let width = |dc, font: HFONT, text: &str| {
        let mut t: Vec<u16> = text.encode_utf16().collect();
        let mut r = RECT::default();
        // SAFETY: measuring on a DC we hold; the font is selected back
        // out by the caller.
        unsafe {
            SelectObject(dc, HGDIOBJ(font.0));
            DrawTextW(
                dc,
                &mut t,
                &mut r,
                DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        r.right - r.left
    };
    // SAFETY: our own window's DC, released below with its font restored.
    unsafe {
        let dc = GetDC(Some(h.panel));
        if dc.is_invalid() {
            return 0;
        }
        let old = SelectObject(dc, HGDIOBJ(title_font.0));
        let end = scale(10, dpi)
            + width(dc, title_font, "File transfer")
            + scale(10, dpi)
            + width(dc, font, &host);
        SelectObject(dc, old);
        ReleaseDC(Some(h.panel), dc);
        end
    }
}

/// Places every control for the panel's size, arrangement, queue and
/// question.
fn layout(h: &Handles) {
    let Some((dpi, (width, height), visible, arrange, queued, asking, status, naming)) =
        with(|s| {
            (
                s.dpi,
                s.size,
                s.visible,
                s.arrange,
                s.queue_shown.len() as i32,
                s.view.as_ref().is_some_and(|v| v.question.is_some()),
                s.local_error.is_some() || s.view.as_ref().is_some_and(|v| v.status.is_some()),
                s.naming.is_some(),
            )
        })
    else {
        return;
    };
    let px = |d: i32| scale(d, dpi);
    let pad = px(8);
    let row = px(26);
    let gap6 = px(6);
    let inner = (width - 2 * pad).max(0);
    let title = px(36);
    // The buttons at the right edge keep to the part the terminal leaves
    // uncovered, but never move over what is to their left.
    let full_right = pad + inner;
    let shown_right = (visible - pad).min(full_right);
    let keep_right = |least: i32| shown_right.max(least).min(full_right);
    // The title row's buttons, against the right edge.
    let layout_w = px(78);
    let follow_w = px(112);
    let filter_y = title + px(2);
    let mut bottom = height - pad;
    let queue_h = if queued > 0 {
        px(24) * (queued.min(QUEUE_ROWS) + 1) + px(6)
    } else {
        0
    };
    let queue_y = bottom - queue_h;
    if queued > 0 {
        bottom = queue_y - gap6;
    }
    let ask_h = px(28);
    let ask_y = bottom - ask_h;
    if asking {
        bottom = ask_y - gap6;
    }
    let status_h = px(20);
    let status_y = bottom - status_h;
    if status {
        bottom = status_y - px(2);
    }
    let name_y = filter_y + row + gap6;
    let lists_y = if naming {
        name_y + row + gap6
    } else {
        filter_y + row + gap6
    };
    let lists = Rect::new(pad, lists_y, inner, (bottom - lists_y).max(0));
    let gap_size = if arrange.stacked { px(38) } else { px(44) };
    let (local, gap, remote) = crate::files::arrange(arrange, lists, gap_size);
    let label_w = px(56);
    let place_side = |side: Side, r: Rect| -> Rect {
        let b = px(28);
        let path_x = r.x + label_w + b + px(4);
        // The path box shrinks, down to a short one, to keep "..." in view.
        let right = keep_right(path_x + px(80) + px(4) + b).min(r.x + r.w);
        let path_w = (right - path_x - b - px(4)).max(0);
        // SAFETY: moving our own windows.
        unsafe {
            let _ = MoveWindow(side.up, r.x + label_w, r.y, b, row, true);
            let _ = MoveWindow(side.path, path_x, r.y, path_w, row, true);
            let _ = MoveWindow(side.extra, path_x + path_w + px(4), r.y, b, row, true);
            let _ = MoveWindow(
                side.list,
                r.x,
                r.y + row + px(4),
                r.w,
                (r.h - row - px(4)).max(0),
                true,
            );
        }
        Rect::new(r.x, r.y, label_w, row)
    };
    let local_label = place_side(h.local, local);
    let remote_label = place_side(h.remote, remote);
    let (up_arrow, down_arrow) = arrange.arrows();
    let bw = px(32);
    let bh = px(28);
    let sync_w = px(66);
    let marks_w = px(108);
    let buttons_w = marks_w + sync_w + follow_w + layout_w + 3 * gap6;
    let title_right = keep_right(title_end(h, dpi) + px(6) + buttons_w);
    // SAFETY: moving and labelling our own windows; the texts live through
    // each call.
    unsafe {
        let _ = MoveWindow(
            h.layout,
            title_right - layout_w,
            (title - row) / 2,
            layout_w,
            row,
            true,
        );
        let follow_x = title_right - layout_w - gap6 - follow_w;
        let _ = MoveWindow(h.follow, follow_x, (title - row) / 2, follow_w, row, true);
        let sync_x = follow_x - gap6 - sync_w;
        let _ = MoveWindow(h.sync, sync_x, (title - row) / 2, sync_w, row, true);
        let marks_x = sync_x - gap6 - marks_w;
        with(|s| s.title_buttons_x = marks_x);
        let _ = MoveWindow(h.bookmarks, marks_x, (title - row) / 2, marks_w, row, true);
        let check_w = px(160);
        let b = px(70);
        let name_w = (inner - check_w - 2 * b - 3 * gap6).max(0);
        let mut x = pad;
        for (c, w) in h.name.iter().zip([name_w, b, b, check_w]) {
            let _ = MoveWindow(*c, x, name_y, w, row, true);
            let _ = ShowWindow(*c, if naming { SW_SHOWNA } else { SW_HIDE });
            x += w + gap6;
        }
        // Refresh ends the filter's row: the title row is full on a
        // narrow pane.
        let refresh_w = px(76).min(inner / 3);
        let filter_right = keep_right(pad + px(120) + gap6 + refresh_w);
        let filter_w = (filter_right - pad - gap6 - refresh_w).max(0);
        let _ = MoveWindow(h.filter, pad, filter_y, filter_w, row, true);
        let _ = MoveWindow(
            h.refresh,
            pad + filter_w + gap6,
            filter_y,
            refresh_w,
            row,
            true,
        );
        let (u, d) = if arrange.stacked {
            // Centred in the part of the gap the terminal leaves uncovered,
            // but never squeezed past room for both buttons.
            let shown_w = (shown_right - gap.x).max(2 * bw + px(8)).min(gap.w);
            let cx = gap.x + shown_w / 2;
            let y = gap.y + (gap.h - bh) / 2;
            ((cx - bw - px(4), y), (cx + px(4), y))
        } else {
            let cy = gap.y + gap.h / 2;
            let x = gap.x + (gap.w - bw) / 2;
            ((x, cy - bh - px(4)), (x, cy + px(4)))
        };
        let _ = MoveWindow(h.upload, u.0, u.1, bw, bh, true);
        let _ = MoveWindow(h.download, d.0, d.1, bw, bh, true);
        let _ = SetWindowTextW(h.upload, &windows::core::HSTRING::from(up_arrow));
        let _ = SetWindowTextW(h.download, &windows::core::HSTRING::from(down_arrow));
        let show = |c: HWND, on: bool| {
            let _ = ShowWindow(c, if on { SW_SHOWNA } else { SW_HIDE });
        };
        show(h.status, status);
        let _ = MoveWindow(h.status, pad, status_y, inner, status_h, true);
        show(h.ask, asking);
        show(h.all, asking);
        let buttons_w = px(84) * 3 + px(80) + gap6 * 3;
        let _ = MoveWindow(
            h.ask,
            pad,
            ask_y + px(5),
            (inner - buttons_w - gap6).max(0),
            ask_h - px(6),
            true,
        );
        let mut x = pad + inner - buttons_w;
        for b in h.answers {
            show(b, asking);
            let _ = MoveWindow(b, x, ask_y, px(84), ask_h, true);
            x += px(84) + gap6;
        }
        let _ = MoveWindow(h.all, x, ask_y, px(80), ask_h, true);
        show(h.queue, queued > 0);
        let _ = MoveWindow(h.queue, pad, queue_y, inner, queue_h, true);
        let _ = InvalidateRect(Some(h.panel), None, true);
    }
    with(|s| {
        s.lists = lists;
        s.gap = gap;
        s.labels = [local_label, remote_label];
    });
}

/// Whether one of the pane's controls has the keyboard.
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

/// Gives the keyboard to the Remote list, or the Local list when no host
/// is listed.
pub fn focus_first() {
    let Some((h, remote)) = with(|s| (s.h, s.view.as_ref().is_some_and(|v| v.host.is_some())))
    else {
        return;
    };
    let target = if remote { h.remote.list } else { h.local.list };
    // SAFETY: our own control.
    unsafe {
        let _ = SetFocus(Some(target));
    }
}

fn set_state(list: HWND, index: i32, flags: LIST_VIEW_ITEM_STATE_FLAGS, on: bool) {
    let item = LVITEMW {
        stateMask: flags,
        state: if on { flags } else { Default::default() },
        ..Default::default()
    };
    // SAFETY: our own control; the struct lives through the call. Index
    // -1 is every row.
    unsafe {
        SendMessageW(
            list,
            LVM_SETITEMSTATE,
            Some(WPARAM(index as usize)),
            Some(LPARAM(&raw const item as isize)),
        );
    }
}

fn select(list: HWND, index: i32) {
    let both = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
    set_state(list, -1, both, false);
    set_state(list, index, both, true);
    // SAFETY: our own control.
    unsafe {
        SendMessageW(
            list,
            LVM_ENSUREVISIBLE,
            Some(WPARAM(index as usize)),
            Some(LPARAM(0)),
        );
    }
}

/// The selected rows' indexes.
fn selected(list: HWND) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = usize::MAX;
    loop {
        // SAFETY: plain message to our own control.
        let i = unsafe {
            SendMessageW(
                list,
                LVM_GETNEXTITEM,
                Some(WPARAM(from)),
                Some(LPARAM(LVNI_SELECTED as isize)),
            )
        }
        .0;
        if i < 0 {
            return out;
        }
        out.push(i as usize);
        from = i as usize;
    }
}

/// Fills `list` with `rows`, keeping the selection on the same names, or
/// on `pick` when it is listed, or on the first row in a new folder.
fn fill(
    list: HWND,
    rows: &[FileRow],
    old: &[FileRow],
    new_folder: bool,
    pick: Option<&[u8]>,
    icons: &mut HashMap<String, i32>,
    remote: bool,
) {
    let keep: Vec<Vec<u8>> = if new_folder {
        Vec::new()
    } else {
        selected(list)
            .into_iter()
            .filter_map(|i| old.get(i).map(|r| r.key.clone()))
            .collect()
    };
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        SendMessageW(list, LVM_DELETEALLITEMS, None, None);
        for (i, r) in rows.iter().enumerate() {
            let mut name = wide(&r.name);
            let item = LVITEMW {
                mask: LVIF_TEXT | LVIF_PARAM | LVIF_IMAGE,
                iItem: i as i32,
                pszText: PWSTR(name.as_mut_ptr()),
                iImage: icon(icons, &r.name, r.dir),
                lParam: LPARAM(i as isize),
                ..Default::default()
            };
            let at = SendMessageW(
                list,
                LVM_INSERTITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const item as isize)),
            )
            .0;
            let size = r.size.map(names::size).unwrap_or_default();
            let mut subs = vec![size, when(r.modified)];
            if remote {
                subs.push(r.rights.clone());
            }
            for (sub, text) in subs.iter().enumerate() {
                let mut text = wide(text);
                let item = LVITEMW {
                    iSubItem: sub as i32 + 1,
                    pszText: PWSTR(text.as_mut_ptr()),
                    ..Default::default()
                };
                SendMessageW(
                    list,
                    LVM_SETITEMTEXTW,
                    Some(WPARAM(at as usize)),
                    Some(LPARAM(&raw const item as isize)),
                );
            }
        }
    }
    let state = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0);
    let mut first = None;
    for (i, r) in rows.iter().enumerate() {
        let want = match pick {
            Some(p) => r.key == p,
            None => keep.contains(&r.key),
        };
        if want && !r.up {
            set_state(list, i as i32, state, true);
            first.get_or_insert(i);
        }
    }
    match first {
        Some(i) => {
            set_state(
                list,
                i as i32,
                LIST_VIEW_ITEM_STATE_FLAGS(LVIS_FOCUSED.0),
                true,
            );
            // SAFETY: our own control.
            unsafe {
                SendMessageW(list, LVM_ENSUREVISIBLE, Some(WPARAM(i)), Some(LPARAM(0)));
            }
        }
        None if !rows.is_empty() => {
            set_state(list, 0, LIST_VIEW_ITEM_STATE_FLAGS(LVIS_FOCUSED.0), true);
        }
        None => {}
    }
}

fn set_text_if(control: HWND, text: &str) {
    // SAFETY: plain query and message to our own control; the text lives
    // through the call.
    unsafe {
        if GetFocus() == control {
            // Not while the user types in it.
            return;
        }
        if search_text(control) != text {
            let t = wide(text);
            let _ = SetWindowTextW(control, PCWSTR(t.as_ptr()));
        }
    }
}

/// Shows what the main window gives, changing only what changed.
pub fn set_view(view: View) {
    let Some(h) = handles() else {
        return;
    };
    // Called with the main window's state borrowed: what this emits is
    // carried out once that is released.
    with(|s| s.in_view = true);
    let _guard = ViewGuard;
    let changes = with(|s| {
        let old = s.view.as_ref();
        let host = old.map(|v| &v.host) != Some(&view.host);
        // A bookmark name being typed belongs to the connection it was
        // started on: another connection cancels it.
        let renaming_lost =
            s.naming.is_some() && old.is_some_and(|v| v.alias != view.alias || v.key != view.key);
        if renaming_lost {
            s.naming = None;
        }
        // A Sync move: the Local list follows once the remote folder has
        // listed, and stays if it did not.
        // On another queue or connection it is dropped: neither moves.
        let alias_changed = old.is_some_and(|v| v.alias != view.alias);
        let synced = match files::sync_step(
            s.sync_pending.take(),
            view.key.as_deref(),
            alias_changed,
            view.remote_loading,
            &view.remote_dir,
        ) {
            SyncStep::Wait(p) => {
                s.sync_pending = Some(p);
                SyncStep::None
            }
            step => step,
        };
        // A go-back settles: say how it went.
        if let Some(back) = s.going_back.take() {
            let remote_error = view
                .status
                .as_ref()
                .filter(|st| st.1)
                .map(|st| st.0.as_str());
            match files::back_step(
                &back,
                view.key.as_deref(),
                alias_changed,
                view.remote_loading,
                &view.remote_dir,
                remote_error,
            ) {
                BackStep::Wait => s.going_back = Some(back),
                BackStep::Dropped => s.local_error = None,
                BackStep::Done(line) => s.local_error = Some(line),
            }
        }
        let queue = s.queue_shown != view.queue;
        let before = (
            s.queue_shown.len(),
            old.is_some_and(|v| v.question.is_some()),
            old.is_some_and(|v| v.status.is_some()),
            s.arrange,
        );
        if old.is_none() {
            s.arrange = view.arrange;
        }
        // The pane turned to another host, or another connection to the
        // same host: its connection's Local folder, else the download
        // folder (not the last connection's Local folder).
        let shown = (view.host.clone(), view.alias.clone());
        let turned = s.shown_host.is_some() && s.shown_host.as_ref() != Some(&shown);
        let first = s.shown_host.is_none();
        let start = if turned {
            Some(
                view.local_start
                    .clone()
                    .unwrap_or_else(|| view.base.clone()),
            )
            .filter(|d| s.local.dir() != Some(d.as_path()))
        } else if first {
            view.local_start.clone()
        } else {
            None
        };
        s.shown_host = Some(shown);
        let local_changed = s.local.sync(&view.base, view.landed);
        if local_changed {
            s.local_error = None;
        }
        s.view = Some(view.clone());
        s.queue_shown.clone_from(&view.queue);
        let after = (
            s.queue_shown.len(),
            view.question.is_some(),
            view.status.is_some(),
            s.arrange,
        );
        (before != after || renaming_lost, host, queue, start, synced)
    });
    let Some((relayout, host_changed, queue_changed, start, synced)) = changes else {
        return;
    };
    if let Some(dir) = start {
        // It does not become the download folder.
        local_go(&h, dir, None, false);
    }
    match synced {
        SyncStep::Move(p) => {
            let moved = match p.local {
                SyncLocal::Enter(dir) => local_go(&h, dir, None, true),
                SyncLocal::Up => local_up(&h),
            };
            if !moved {
                // Both or neither: the Remote list goes back, on the same
                // queue; the line says how that went once it has.
                let back = files::SyncBack {
                    key: p.key,
                    alias: view.alias.clone(),
                    path: p.back,
                };
                with(|s| s.going_back = Some(back.clone()));
                emit(Event::SyncBack(back));
                local_problem(
                    "Sync browsing: the Local folder did not open, so Remote is going back to where it was.".into(),
                );
            }
        }
        SyncStep::Failed => local_problem(
            "Sync browsing: the Remote folder did not open, so Local stayed where it was.".into(),
        ),
        SyncStep::None | SyncStep::Wait(_) => {}
    }
    refresh_local(&h, false);
    refresh_remote(&h);
    if queue_changed {
        refresh_queue(&h, &view);
    }
    // SAFETY: our own controls.
    unsafe {
        SendMessageW(
            h.follow,
            BM_SETCHECK,
            Some(WPARAM(if view.follow == Follow::Off {
                BST_UNCHECKED.0 as usize
            } else {
                BST_CHECKED.0 as usize
            })),
            None,
        );
        let enabled = view.host.is_some();
        for c in [
            h.remote.up,
            h.remote.extra,
            h.remote.path,
            h.upload,
            h.download,
        ] {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(c, enabled);
        }
        if let Some((_, text)) = &view.question {
            set_text_if(h.ask, &format!("⚠ {text}"));
        }
    }
    refresh_status(&h);
    if relayout || host_changed {
        // The title row's buttons stop at the host's name.
        layout(&h);
    }
    if host_changed {
        // The title shows the host.
        // SAFETY: our own window.
        unsafe {
            let _ = InvalidateRect(Some(h.panel), None, false);
        }
    }
}

fn refresh_status(h: &Handles) {
    let Some((text, show)) = with(|s| {
        let t = s
            .local_error
            .clone()
            .or_else(|| s.view.as_ref().and_then(|v| v.status.clone()).map(|v| v.0));
        let show = t.is_some();
        (t.unwrap_or_default(), show)
    }) else {
        return;
    };
    set_text_if(h.status, &text);
    // SAFETY: our own window.
    let shown = unsafe { IsWindowVisible(h.status).as_bool() };
    if shown != show {
        layout(h);
    }
}

/// Shows the Local list again; `reread` reads its folder from disk.
fn refresh_local(h: &Handles, reread: bool) {
    let Some((rows, old, new_folder, pick, mut icons, dir)) = with(|s| {
        if reread {
            s.local.reload();
        }
        let rows = local_rows(&s.local.listing, &s.filter);
        let dir = s.local.dir().map(PathBuf::from);
        let new_folder = dir != s.local_dir;
        if rows == s.local_shown && !new_folder {
            return None;
        }
        s.local_dir.clone_from(&dir);
        let old = std::mem::replace(&mut s.local_shown, rows.clone());
        let pick = s.select_local.take();
        Some((
            rows,
            old,
            new_folder,
            pick,
            std::mem::take(&mut s.icons),
            dir,
        ))
    })
    .flatten() else {
        return;
    };
    fill(
        h.local.list,
        &rows,
        &old,
        new_folder,
        pick.as_deref(),
        &mut icons,
        false,
    );
    with(|s| s.icons = icons);
    let label = dir.map_or_else(|| "This PC".to_owned(), |d| d.display().to_string());
    set_text_if(h.local.path, &label);
}

fn refresh_remote(h: &Handles) {
    let Some((rows, old, new_folder, pick, mut icons, dir)) = with(|s| {
        let v = s.view.as_ref()?;
        let rows = filtered(&v.remote_rows, &s.filter);
        // Another queue is another host: nothing selected carries over,
        // even when its folder and names are the same.
        let new_queue = v.key != s.remote_key;
        if new_queue {
            s.remote_key.clone_from(&v.key);
            s.select_remote = None;
        }
        let new_folder = new_queue || v.remote_dir != s.remote_dir;
        if rows == s.remote_shown && !new_folder {
            return None;
        }
        s.remote_dir.clone_from(&v.remote_dir);
        let dir = v.remote_dir.clone();
        let old = std::mem::replace(&mut s.remote_shown, rows.clone());
        let pick = s.select_remote.take();
        Some((
            rows,
            old,
            new_folder,
            pick,
            std::mem::take(&mut s.icons),
            dir,
        ))
    })
    .flatten() else {
        return;
    };
    fill(
        h.remote.list,
        &rows,
        &old,
        new_folder,
        pick.as_deref(),
        &mut icons,
        true,
    );
    with(|s| s.icons = icons);
    set_text_if(h.remote.path, &names::display(&dir));
}

fn refresh_queue(h: &Handles, view: &View) {
    // SAFETY: our own control; each struct and its text live through the
    // call, which copies them.
    unsafe {
        let n = SendMessageW(h.queue, LVM_GETITEMCOUNT, None, None).0 as usize;
        let keep = selected(h.queue).first().copied();
        if n != view.queue.len() {
            SendMessageW(h.queue, LVM_DELETEALLITEMS, None, None);
        }
        for (i, r) in view.queue.iter().enumerate() {
            let mut name = wide(&r.name);
            let item = LVITEMW {
                mask: LVIF_TEXT,
                iItem: i as i32,
                pszText: PWSTR(name.as_mut_ptr()),
                ..Default::default()
            };
            if n != view.queue.len() {
                SendMessageW(
                    h.queue,
                    LVM_INSERTITEMW,
                    Some(WPARAM(0)),
                    Some(LPARAM(&raw const item as isize)),
                );
            } else {
                SendMessageW(
                    h.queue,
                    LVM_SETITEMTEXTW,
                    Some(WPARAM(i)),
                    Some(LPARAM(&raw const item as isize)),
                );
            }
            let direction = if r.upload {
                "↑ Upload"
            } else {
                "↓ Download"
            };
            for (sub, text) in [direction, "", r.status.as_str()].iter().enumerate() {
                let mut text = wide(text);
                let item = LVITEMW {
                    iSubItem: sub as i32 + 1,
                    pszText: PWSTR(text.as_mut_ptr()),
                    ..Default::default()
                };
                SendMessageW(
                    h.queue,
                    LVM_SETITEMTEXTW,
                    Some(WPARAM(i)),
                    Some(LPARAM(&raw const item as isize)),
                );
            }
        }
        if n != view.queue.len()
            && let Some(k) = keep.filter(|k| *k < view.queue.len())
        {
            select(h.queue, k as i32);
        }
        // The progress bars.
        let _ = InvalidateRect(Some(h.queue), None, false);
    }
}

fn emit(e: Event) {
    let mut e = Some(e);
    let Some((main, handler)) = with(|s| {
        if s.in_view {
            s.deferred.extend(e.take());
            None
        } else {
            Some((s.h.main, s.handler))
        }
    })
    .flatten() else {
        return;
    };
    if let Some(e) = e {
        handler(main, e);
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
        match msg.message {
            WM_KEYDOWN if key_down(VK_CONTROL.0) && key_down(VK_SHIFT.0) => {
                let mut m = *msg;
                m.hwnd = h.main;
                let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&m);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&m);
                return true;
            }
            // F5 outside the lists (they refresh only themselves): both.
            WM_KEYDOWN
                if msg.wParam.0 == usize::from(VK_F5.0)
                    && msg.hwnd != h.local.list
                    && msg.hwnd != h.remote.list =>
            {
                refresh_both(&h);
                return true;
            }
            WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR => return false,
            _ => {}
        }
        IsDialogMessageW(h.panel, msg).as_bool()
    }
}

/// The rows selected in the Local (`remote` false) or Remote list; the
/// ".." row only when nothing else is.
fn picked(h: &Handles, remote: bool) -> Vec<FileRow> {
    let list = if remote { h.remote.list } else { h.local.list };
    let idx = selected(list);
    with(|s| {
        let rows = if remote {
            &s.remote_shown
        } else {
            &s.local_shown
        };
        idx.iter()
            .filter_map(|&i| rows.get(i))
            .filter(|r| !r.up)
            .cloned()
            .collect()
    })
    .unwrap_or_default()
}

/// The listed remote folder, unless none is listed yet or there is no
/// host.
fn remote_dir() -> Option<Vec<u8>> {
    with(|s| {
        s.view
            .as_ref()
            .filter(|v| v.host.is_some() && !v.remote_dir.is_empty())
            .map(|v| v.remote_dir.clone())
    })
    .flatten()
}

fn local_problem(text: String) {
    if let Some(h) = handles() {
        with(|s| s.local_error = Some(text));
        refresh_status(&h);
    }
}

/// Uploads the selected local rows into the Remote list's folder, or its
/// folder `into` (a name in it).
fn upload(h: &Handles, into: Option<Vec<u8>>) {
    let paths: Vec<PathBuf> = picked(h, false)
        .into_iter()
        .filter_map(|r| r.local)
        .collect();
    if paths.is_empty() {
        return;
    }
    let Some(dir) = remote_dir() else {
        local_problem("Wait for the remote folder to be listed.".into());
        return;
    };
    let dir = match into {
        Some(name) => names::join(&dir, &name),
        None => dir,
    };
    emit(Event::Upload { paths, dir });
}

/// Downloads the selected remote rows into the Local list's folder, or
/// `into`.
fn download(h: &Handles, into: Option<PathBuf>) {
    let keys: Vec<Vec<u8>> = picked(h, true).into_iter().map(|r| r.key).collect();
    if keys.is_empty() {
        return;
    }
    let Some(into) = into.or_else(|| with(|s| s.local.dir().map(PathBuf::from)).flatten()) else {
        local_problem("Choose a folder in the Local list to download into.".into());
        return;
    };
    emit(Event::Download { keys, into });
}

fn local_enter(h: &Handles, dir: PathBuf, select_name: Option<Vec<u8>>) {
    local_go(h, dir, select_name, true);
}

/// Lists local folder `dir`; `remember` makes it the download folder.
/// Returns whether the Local list moved.
fn local_go(h: &Handles, dir: PathBuf, select_name: Option<Vec<u8>>, remember: bool) -> bool {
    let done = with(|s| {
        let r = s.local.enter(&dir);
        if r.is_ok() {
            s.local_error = None;
            s.select_local = select_name;
        }
        r
    });
    match done {
        Some(Ok(())) => {
            refresh_local(h, false);
            refresh_status(h);
            if remember {
                emit(Event::SetLocal(dir));
            }
            true
        }
        Some(Err(e)) => {
            local_problem(e);
            false
        }
        None => false,
    }
}

/// Returns whether the Local list moved.
fn local_up(h: &Handles) -> bool {
    let done = with(|s| s.local.up());
    match done {
        Some(Ok(left)) => {
            let name = left
                .as_ref()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .or_else(|| left.as_ref().map(|p| p.display().to_string()));
            with(|s| {
                s.select_local = name.map(String::into_bytes);
                s.local_error = None;
            });
            refresh_local(h, false);
            refresh_status(h);
            if let Some(dir) = with(|s| s.local.dir().map(PathBuf::from)).flatten() {
                emit(Event::SetLocal(dir));
            }
            true
        }
        Some(Err(e)) => {
            local_problem(e);
            false
        }
        None => false,
    }
}

fn remote_up() {
    let Some(dir) = remote_dir() else {
        return;
    };
    if dir.as_slice() == b"/" {
        return;
    }
    let name = names::file_name(&dir).to_vec();
    with(|s| s.select_remote = Some(name));
    emit(Event::Browse(names::parent(&dir)));
}

/// Enter or a double click on row `index` of a list: a folder opens, a
/// file goes across.
fn open(h: &Handles, remote: bool, index: Option<usize>) {
    let row = with(|s| {
        let rows = if remote {
            &s.remote_shown
        } else {
            &s.local_shown
        };
        index.and_then(|i| rows.get(i)).cloned()
    })
    .flatten();
    let Some(row) = row else {
        return;
    };
    match (row.up, row.dir) {
        (true, _) => go_up(h, remote),
        (false, true) => go_into(h, remote, &row),
        (false, false) if remote => download(h, None),
        (false, false) => upload(h, None),
    }
}

/// Whether Sync browsing is on.
fn synced() -> bool {
    with(|s| s.sync).unwrap_or(false)
}

/// One folder up in the Remote (`remote`) or Local list, and in the
/// other one too with Sync browsing on: both move, or neither.
fn go_up(h: &Handles, remote: bool) {
    if !synced() {
        if remote {
            remote_up();
        } else {
            local_up(h);
        }
        return;
    }
    let Some(dir) = remote_dir() else {
        // No Remote list to keep in step.
        if !remote {
            local_up(h);
        }
        return;
    };
    if dir.as_slice() == b"/" {
        local_problem("Sync browsing: Remote is at / already.".into());
        return;
    }
    if with(|s| s.local.dir().is_none()).unwrap_or(true) {
        remote_up();
        return;
    }
    sync_browse(names::parent(&dir), SyncLocal::Up);
    with(|s| s.select_remote = Some(names::file_name(&dir).to_vec()));
}

/// Opens folder `row` of the Remote (`remote`) or Local list, and the
/// folder of the same name in the other list with Sync browsing on: both
/// open, or neither.
fn go_into(h: &Handles, remote: bool, row: &FileRow) {
    let open_remote = || {
        if let Some(dir) = remote_dir() {
            emit(Event::Browse(names::join(&dir, &row.key)));
        }
    };
    if !synced() {
        if remote {
            open_remote();
        } else if let Some(p) = &row.local {
            local_enter(h, p.clone(), None);
        }
        return;
    }
    let Some(dir) = remote_dir() else {
        if let Some(p) = row.local.as_ref().filter(|_| !remote) {
            local_enter(h, p.clone(), None);
        }
        return;
    };
    let target = names::join(&dir, &row.key);
    if !remote {
        if let Some(p) = &row.local {
            sync_browse(target, SyncLocal::Enter(p.clone()));
        }
        return;
    }
    let Some(here) = with(|s| s.local.dir().map(PathBuf::from)).flatten() else {
        open_remote();
        return;
    };
    // The server picks the name: map it to one plain local name before
    // anything touches the file system (no `\\host\share`, drive or `..`).
    match files::sync_local(&here, &row.key) {
        Some(there) if there.is_dir() => sync_browse(target, SyncLocal::Enter(there)),
        _ => local_problem(format!(
            "Sync browsing: {} has no folder {}, so neither list moved.",
            here.display(),
            crate::ssh::sanitize_line(&String::from_utf8_lossy(&row.key))
        )),
    }
}

/// Asks for remote folder `target`; the Local list does `local` once it
/// has listed.
fn sync_browse(target: Vec<u8>, local: SyncLocal) {
    let Some(back) = remote_dir() else {
        return;
    };
    with(|s| {
        s.sync_pending = Some(SyncPending {
            key: s.view.as_ref().and_then(|v| v.key.clone()),
            target: target.clone(),
            back,
            local,
        });
    });
    emit(Event::Browse(target));
}

fn focused_index(list: HWND) -> Option<usize> {
    use windows::Win32::UI::Controls::LVNI_FOCUSED;
    // SAFETY: plain message to our own control.
    let i = unsafe {
        SendMessageW(
            list,
            LVM_GETNEXTITEM,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(LVNI_FOCUSED as isize)),
        )
    }
    .0;
    (i >= 0).then_some(i as usize)
}

fn command(h: &Handles, id: usize, code: u32) {
    // SAFETY: a plain query.
    let focus = unsafe { GetFocus() };
    match (id, code) {
        (ID_FILTER, EN_CHANGE) => {
            let text: String = search_text(h.filter).chars().take(60).collect();
            with(|s| s.filter = text);
            refresh_local(h, false);
            refresh_remote(h);
        }
        (ID_FILTER | ID_LOCAL_PATH | ID_REMOTE_PATH | ID_NAME, EN_SETFOCUS) => emit(Event::Focused),
        (id, _) if id == IDOK.0 as usize => {
            if focus == h.name[0] {
                save_name(h);
            } else if focus == h.local.list || focus == h.remote.list {
                let remote = focus == h.remote.list;
                if key_down(VK_CONTROL.0) {
                    if remote {
                        download(h, None);
                    } else {
                        upload(h, None);
                    }
                } else {
                    let row = selected(focus).first().copied().or(focused_index(focus));
                    open(h, remote, row);
                }
            } else if focus == h.local.path {
                let text = search_text(h.local.path);
                local_enter(h, PathBuf::from(text.trim()), None);
                // SAFETY: our own control.
                unsafe {
                    let _ = SetFocus(Some(h.local.list));
                }
            } else if focus == h.remote.path {
                let text = search_text(h.remote.path);
                if !text.trim().is_empty() {
                    emit(Event::Browse(text.trim().as_bytes().to_vec()));
                }
                // SAFETY: our own control.
                unsafe {
                    let _ = SetFocus(Some(h.remote.list));
                }
            } else if focus == h.filter {
                focus_first();
            }
        }
        (id, _) if id == IDCANCEL.0 as usize => {
            if with(|s| s.naming.is_some()).unwrap_or(false) && h.name.contains(&focus) {
                end_naming(h);
            } else if with(|s| s.drag.is_some()).unwrap_or(false) {
                // SAFETY: we hold the capture.
                unsafe {
                    let _ = ReleaseCapture();
                }
            } else if search_text(h.filter).is_empty() {
                emit(Event::Leave);
            } else {
                // SAFETY: our own control; EN_CHANGE clears the filter.
                unsafe {
                    let _ = SetWindowTextW(h.filter, w!(""));
                }
            }
        }
        (ID_FOLLOW, BN_CLICKED) => emit(Event::ToggleFollow),
        (ID_LAYOUT, BN_CLICKED) => layout_menu(h),
        (ID_BOOKMARKS, BN_CLICKED) => bookmarks_menu(h),
        (ID_REFRESH, BN_CLICKED) => refresh_both(h),
        (ID_SYNC, BN_CLICKED) => {
            let on = with(|s| {
                s.sync = !s.sync;
                s.sync_pending = None;
                s.sync
            })
            .unwrap_or(false);
            // SAFETY: our own control.
            unsafe {
                SendMessageW(
                    h.sync,
                    BM_SETCHECK,
                    Some(WPARAM(if on {
                        BST_CHECKED.0 as usize
                    } else {
                        BST_UNCHECKED.0 as usize
                    })),
                    None,
                );
            }
        }
        (ID_NAME_SAVE, BN_CLICKED) => save_name(h),
        (ID_NAME_CANCEL, BN_CLICKED) => end_naming(h),
        (ID_LOCAL_UP, BN_CLICKED) => go_up(h, false),
        (ID_LOCAL_PICK, BN_CLICKED) => emit(Event::PickLocal),
        (ID_REMOTE_UP, BN_CLICKED) => go_up(h, true),
        (ID_REMOTE_HOME, BN_CLICKED) => emit(Event::Home),
        (ID_UPLOAD, BN_CLICKED) => upload(h, None),
        (ID_DOWNLOAD, BN_CLICKED) => download(h, None),
        (ID_OVERWRITE | ID_SKIP | ID_KEEP, BN_CLICKED) => {
            let Some(job) = with(|s| s.view.as_ref()?.question.as_ref().map(|q| q.0)).flatten()
            else {
                return;
            };
            let choice = match id {
                ID_OVERWRITE => Choice::Overwrite,
                ID_SKIP => Choice::Skip,
                _ => Choice::KeepBoth,
            };
            // SAFETY: our own control.
            let all =
                unsafe { SendMessageW(h.all, BM_GETCHECK, None, None) }.0 == BST_CHECKED.0 as isize;
            emit(Event::Answer(job, choice, all));
        }
        _ => {}
    }
}

/// The Layout button's menu.
fn layout_menu(h: &Handles) {
    const SIDE: u32 = 1;
    const STACKED: u32 = 2;
    const SWAP: u32 = 3;
    const PICK: u32 = 4;
    const UPLOAD: u32 = 5;
    const EXPLORER: u32 = 6;
    let Some((arrange, host, local)) = with(|s| {
        (
            s.arrange,
            s.view.as_ref().is_some_and(|v| v.host.is_some()),
            s.local.dir().map(PathBuf::from),
        )
    }) else {
        return;
    };
    let chosen = popup(
        h.layout,
        &[
            (
                SIDE,
                "&Side by side",
                arrange.stacked.then_some(0).is_none(),
            ),
            (STACKED, "S&tacked", arrange.stacked),
            (0, "", false),
            (SWAP, "S&wap Local and Remote", arrange.swapped),
            (0, "", false),
            (PICK, "&Choose local folder...", false),
            (
                if host { UPLOAD } else { u32::MAX },
                "&Upload files...",
                false,
            ),
            (
                if local.is_some() { EXPLORER } else { u32::MAX },
                "&Open local folder in Explorer",
                false,
            ),
        ],
        None,
    );
    let set = |a: Arrange| {
        with(|s| s.arrange = a);
        layout(h);
        emit(Event::Arrange(a));
    };
    match chosen {
        SIDE => set(Arrange {
            stacked: false,
            ..arrange
        }),
        STACKED => set(Arrange {
            stacked: true,
            ..arrange
        }),
        SWAP => set(Arrange {
            swapped: !arrange.swapped,
            ..arrange
        }),
        PICK => emit(Event::PickLocal),
        UPLOAD => emit(Event::PickUpload),
        EXPLORER => {
            if let Some(dir) = local {
                emit(Event::OpenLocal(dir));
            }
        }
        _ => {}
    }
}

/// Reads the Local folder from disk again and asks for the Remote one.
fn refresh_both(h: &Handles) {
    refresh_local(h, true);
    if with(|s| s.view.as_ref().is_some_and(|v| v.host.is_some())).unwrap_or(false) {
        emit(Event::Refresh);
    }
}

/// The Bookmarks button's menu: open one, add one for the folders shown,
/// rename or delete one.
fn bookmarks_menu(h: &Handles) {
    const ADD: u32 = 1;
    const OPEN: u32 = 1000;
    const RENAME: u32 = 2000;
    const DELETE: u32 = 3000;
    let Some((all, alias, can_add)) = with(|s| {
        let v = s.view.as_ref();
        let remote = v.is_some_and(|v| v.host.is_some() && !v.remote_dir.is_empty());
        (
            v.map(|v| v.bookmarks.clone()).unwrap_or_default(),
            v.and_then(|v| v.alias.clone()),
            remote || s.local.dir().is_some(),
        )
    }) else {
        return;
    };
    let shown = files::bookmarks_for(&all, alias.as_deref());
    let item = |b: &Bookmark| {
        let place = b
            .remote
            .as_deref()
            .or(b.local.as_deref())
            .map(|p| short_path(p, 40).replace('&', "&&"))
            .unwrap_or_default();
        windows::core::HSTRING::from(format!("{}\t{place}", b.name.replace('&', "&&")))
    };
    let named = |b: &Bookmark| windows::core::HSTRING::from(b.name.replace('&', "&&"));
    // SAFETY: popup menus of our own, destroyed (with their submenus)
    // after use; the texts live through each call.
    let chosen = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        if shown.is_empty() {
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("No bookmarks yet"));
        }
        for (n, (_, b)) in shown.iter().enumerate() {
            let _ = AppendMenuW(menu, MF_STRING, (OPEN as usize) + n, &item(b));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let add = if can_add {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        };
        let _ = AppendMenuW(menu, add, ADD as usize, w!("&Add bookmark here..."));
        if !shown.is_empty()
            && let (Ok(rename), Ok(delete)) = (CreatePopupMenu(), CreatePopupMenu())
        {
            for (n, (_, b)) in shown.iter().enumerate() {
                let _ = AppendMenuW(rename, MF_STRING, (RENAME as usize) + n, &named(b));
                let _ = AppendMenuW(delete, MF_STRING, (DELETE as usize) + n, &named(b));
            }
            let _ = AppendMenuW(menu, MF_STRING | MF_POPUP, rename.0 as usize, w!("&Rename"));
            let _ = AppendMenuW(menu, MF_STRING | MF_POPUP, delete.0 as usize, w!("&Delete"));
        }
        let mut rc = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(h.bookmarks, &mut rc);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            rc.left,
            rc.bottom,
            None,
            h.bookmarks,
            None,
        );
        let _ = DestroyMenu(menu);
        u32::try_from(chosen.0).unwrap_or(0)
    };
    let pick = |base: u32| {
        chosen
            .checked_sub(base)
            .and_then(|n| shown.get(n as usize))
            .map(|(i, b)| (*i, (*b).clone()))
    };
    if chosen == ADD {
        start_naming(h, None);
    } else if let Some((i, _)) = (chosen >= DELETE).then(|| pick(DELETE)).flatten() {
        let out = all
            .iter()
            .enumerate()
            .filter(|(n, _)| *n != i)
            .map(|(_, b)| b.clone())
            .collect();
        set_bookmarks(out);
    } else if let Some((i, _)) = (chosen >= RENAME).then(|| pick(RENAME)).flatten() {
        start_naming(h, Some(i));
    } else if let Some((_, b)) = (chosen >= OPEN).then(|| pick(OPEN)).flatten() {
        open_bookmark(h, &b);
    }
}

/// `path` cut to its last `max` characters.
fn short_path(path: &str, max: usize) -> String {
    let n = path.chars().count();
    if n <= max {
        return path.to_owned();
    }
    let tail: String = path.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

/// Keeps `all` as the bookmarks, here and in the settings.
fn set_bookmarks(all: Vec<Bookmark>) {
    with(|s| {
        if let Some(v) = s.view.as_mut() {
            v.bookmarks.clone_from(&all);
        }
    });
    emit(Event::Bookmarks(all));
}

/// Opens bookmark `b`'s folders: the local one in the Local list, the
/// remote one in the Remote list.
fn open_bookmark(h: &Handles, b: &Bookmark) {
    let host = with(|s| s.view.as_ref().is_some_and(|v| v.host.is_some())).unwrap_or(false);
    if let Some(local) = &b.local {
        local_enter(h, PathBuf::from(local), None);
    }
    if let Some(remote) = &b.remote {
        if host {
            emit(Event::Browse(remote.as_bytes().to_vec()));
        } else if b.local.is_none() {
            local_problem(format!("Connect to a host to open {remote}."));
        }
    }
}

/// Shows the name row, to name a new bookmark (`which` `None`) for the
/// folders shown, or to rename bookmark `which`.
fn start_naming(h: &Handles, which: Option<usize>) {
    let Some((name, only_host, has_alias)) = with(|s| {
        let v = s.view.as_ref()?;
        let remote = v
            .host
            .is_some()
            .then_some(v.remote_dir.as_slice())
            .filter(|d| !d.is_empty());
        let (name, only_host) = match which {
            Some(i) => {
                let b = v.bookmarks.get(i)?;
                (b.name.clone(), b.host.is_some())
            }
            // Remote folders are the host's own.
            None => (
                files::bookmark_suggestion(s.local.dir(), remote),
                remote.is_some(),
            ),
        };
        let has_alias = v.alias.is_some();
        s.naming = Some(which);
        Some((name, only_host && has_alias, has_alias))
    })
    .flatten() else {
        return;
    };
    layout(h);
    let text = wide(&name);
    // SAFETY: our own controls; the text lives through the call.
    unsafe {
        let _ = SetWindowTextW(h.name[0], PCWSTR(text.as_ptr()));
        let _ = SetWindowTextW(
            h.name[1],
            if which.is_some() {
                w!("Rename")
            } else {
                w!("Add")
            },
        );
        SendMessageW(
            h.name[3],
            BM_SETCHECK,
            Some(WPARAM(if only_host {
                BST_CHECKED.0 as usize
            } else {
                BST_UNCHECKED.0 as usize
            })),
            None,
        );
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(h.name[3], has_alias);
        let _ = SetFocus(Some(h.name[0]));
        SendMessageW(h.name[0], EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
}

/// Hides the name row.
fn end_naming(h: &Handles) {
    with(|s| s.naming = None);
    layout(h);
    // SAFETY: our own control.
    unsafe {
        let _ = SetFocus(Some(h.bookmarks));
    }
}

/// Saves the bookmark named in the name row.
fn save_name(h: &Handles) {
    let Some(name) = files::bookmark_name(&search_text(h.name[0])) else {
        local_problem("Type a name for the bookmark.".into());
        return;
    };
    // SAFETY: our own control.
    let only_host =
        unsafe { SendMessageW(h.name[3], BM_GETCHECK, None, None) }.0 == BST_CHECKED.0 as isize;
    let Some(Some((all, which, new))) = with(|s| {
        let which = s.naming?;
        let v = s.view.as_ref()?;
        let host = |old: Option<&String>| {
            only_host
                .then(|| v.alias.clone().or_else(|| old.cloned()))
                .flatten()
        };
        let new = match which {
            Some(i) => {
                let old = v.bookmarks.get(i)?;
                Bookmark {
                    name,
                    host: host(old.host.as_ref()),
                    ..old.clone()
                }
            }
            None => Bookmark {
                name,
                local: s.local.dir().map(|d| d.display().to_string()),
                remote: v
                    .host
                    .is_some()
                    .then(|| String::from_utf8(v.remote_dir.clone()).ok())
                    .flatten()
                    .filter(|r| {
                        !r.is_empty() && !r.contains('"') && !r.chars().any(char::is_control)
                    }),
                host: host(None),
            },
        };
        Some((v.bookmarks.clone(), which, new))
    }) else {
        return;
    };
    if new.local.is_none() && new.remote.is_none() {
        local_problem("Open a folder to bookmark first.".into());
        return;
    }
    end_naming(h);
    set_bookmarks(files::put_bookmark(&all, new, which));
}

/// A popup menu of `items` (id, text, checked; id 0 is a line, `u32::MAX`
/// greyed out) below `under`, or at `at` (screen coordinates); the id
/// picked, 0 for none.
fn popup(under: HWND, items: &[(u32, &str, bool)], at: Option<POINT>) -> u32 {
    // SAFETY: a popup menu of our own, destroyed after use; the texts live
    // through each call.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        for (id, text, checked) in items {
            let text = windows::core::HSTRING::from(*text);
            match *id {
                0 => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                }
                u32::MAX => {
                    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &text);
                }
                id => {
                    let flags = if *checked {
                        MF_STRING | MF_CHECKED
                    } else {
                        MF_STRING
                    };
                    let _ = AppendMenuW(menu, flags, id as usize, &text);
                }
            }
        }
        let at = at.unwrap_or_else(|| {
            let mut rc = RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(under, &mut rc);
            POINT {
                x: rc.left,
                y: rc.bottom,
            }
        });
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            at.x,
            at.y,
            None,
            under,
            None,
        );
        let _ = DestroyMenu(menu);
        u32::try_from(chosen.0).unwrap_or(0)
    }
}

/// Where a right click (or the menu key, `lparam` -1) on `list` opens its
/// menu, and the row it is on.
fn menu_spot(list: HWND, lparam: LPARAM) -> (POINT, Option<usize>) {
    // SAFETY: our own control; each struct lives through its call.
    unsafe {
        if lparam.0 as i32 == -1 {
            let i = focused_index(list).filter(|i| selected(list).contains(i));
            let mut rc = RECT {
                left: LVIR_BOUNDS as i32,
                ..Default::default()
            };
            let mut at = POINT::default();
            if let Some(i) = i
                && SendMessageW(
                    list,
                    LVM_GETITEMRECT,
                    Some(WPARAM(i)),
                    Some(LPARAM(&raw mut rc as isize)),
                )
                .0 != 0
            {
                at = POINT {
                    x: rc.left + 16,
                    y: rc.bottom,
                };
            }
            let _ = ClientToScreen(list, &mut at);
            return (at, i);
        }
        let at = POINT {
            x: i32::from(lparam.0 as u16 as i16),
            y: i32::from((lparam.0 >> 16) as u16 as i16),
        };
        let mut hit = LVHITTESTINFO {
            pt: at,
            ..Default::default()
        };
        let _ = ScreenToClient(list, &mut hit.pt);
        let i = SendMessageW(
            list,
            LVM_HITTEST,
            Some(WPARAM(0)),
            Some(LPARAM(&raw mut hit as isize)),
        )
        .0;
        let i = (i >= 0).then_some(i as usize);
        // A right click on a row not selected selects it alone.
        if let Some(i) = i
            && !selected(list).contains(&i)
        {
            select(list, i as i32);
        }
        (at, i)
    }
}

fn context_menu(h: &Handles, list: HWND, lparam: LPARAM) {
    const OPEN: u32 = 1;
    const SEND: u32 = 2;
    const DELETE: u32 = 3;
    const REFRESH: u32 = 4;
    const HOME: u32 = 5;
    const PICK: u32 = 6;
    const EXPLORER: u32 = 7;
    const UPLOAD: u32 = 8;
    let (at, row) = menu_spot(list, lparam);
    let remote = list == h.remote.list;
    let any = !picked(h, remote).is_empty();
    let host = remote_dir().is_some();
    let can = |id: u32, on: bool| if on { id } else { u32::MAX };
    let items: Vec<(u32, &str, bool)> = if remote {
        vec![
            (can(OPEN, row.is_some()), "&Open\tEnter", false),
            (can(SEND, any), "&Download\tCtrl+Enter", false),
            (can(DELETE, any), "D&elete...\tDel", false),
            (0, "", false),
            (can(REFRESH, host), "&Refresh\tF5", false),
            (can(HOME, host), "&Home folder", false),
            (can(UPLOAD, host), "&Upload files...", false),
        ]
    } else {
        let local = with(|s| s.local.dir().is_some()).unwrap_or(false);
        vec![
            (can(OPEN, row.is_some()), "&Open\tEnter", false),
            (can(SEND, any && host), "&Upload\tCtrl+Enter", false),
            (0, "", false),
            (REFRESH, "&Refresh\tF5", false),
            (PICK, "&Choose local folder...", false),
            (can(EXPLORER, local), "Open in &Explorer", false),
        ]
    };
    // The menu runs its own message loop, in which the list can be
    // refreshed: the row is found again by its name afterwards.
    let clicked = row.and_then(|i| shown_row(remote, i));
    let queue = shown_queue();
    let chosen = popup(list, &items, Some(at));
    if shown_queue() != queue {
        // The pane turned to another host meanwhile: its rows are not the
        // ones the menu was for.
        return;
    }
    match chosen {
        OPEN => open(h, remote, clicked.and_then(|c| find_row(remote, &c))),
        SEND if remote => download(h, None),
        SEND => upload(h, None),
        DELETE => delete(h),
        REFRESH if remote => emit(Event::Refresh),
        REFRESH => refresh_local(h, true),
        HOME => emit(Event::Home),
        UPLOAD => emit(Event::PickUpload),
        PICK => emit(Event::PickLocal),
        EXPLORER => {
            if let Some(dir) = with(|s| s.local.dir().map(PathBuf::from)).flatten() {
                emit(Event::OpenLocal(dir));
            }
        }
        _ => {}
    }
}

/// The transfer queue the Remote list is on.
fn shown_queue() -> Option<String> {
    with(|s| s.view.as_ref().and_then(|v| v.key.clone())).flatten()
}

/// A row of a list by what it is, not where: its folder, name and
/// whether it is the ".." row.
#[derive(Clone, PartialEq, Eq)]
struct RowId {
    folder: (Option<PathBuf>, Vec<u8>),
    key: Vec<u8>,
    up: bool,
}

/// Row `i` of the Remote (`remote`) or Local list as shown.
fn shown_row(remote: bool, i: usize) -> Option<RowId> {
    with(|s| {
        let (rows, folder) = if remote {
            (&s.remote_shown, (None, s.remote_dir.clone()))
        } else {
            (&s.local_shown, (s.local_dir.clone(), Vec::new()))
        };
        rows.get(i).map(|r| RowId {
            folder,
            key: r.key.clone(),
            up: r.up,
        })
    })
    .flatten()
}

/// Where row `id` is in its list now; `None` when the list shows another
/// folder or the row went.
fn find_row(remote: bool, id: &RowId) -> Option<usize> {
    with(|s| {
        let (rows, folder) = if remote {
            (&s.remote_shown, (None, s.remote_dir.clone()))
        } else {
            (&s.local_shown, (s.local_dir.clone(), Vec::new()))
        };
        if folder != id.folder {
            return None;
        }
        rows.iter().position(|r| r.up == id.up && r.key == id.key)
    })
    .flatten()
}

fn queue_menu(h: &Handles, lparam: LPARAM) {
    const PAUSE: u32 = 1;
    const RESUME: u32 = 2;
    const CANCEL: u32 = 3;
    const RETRY: u32 = 4;
    const CLEAR: u32 = 5;
    let (at, row) = menu_spot(h.queue, lparam);
    let job = with(|s| {
        row.and_then(|i| s.queue_shown.get(i))
            .map(|r| (r.id, r.state))
    })
    .flatten();
    let state = job.map(|j| j.1);
    let can = |id: u32, on: bool| if on { id } else { u32::MAX };
    let running = state.is_some_and(|s| !s.finished() && s != JobState::Paused);
    let items = [
        (can(PAUSE, running), "&Pause", false),
        (
            can(RESUME, state == Some(JobState::Paused)),
            "&Resume",
            false,
        ),
        (
            can(CANCEL, state.is_some_and(|s| !s.finished())),
            "&Cancel\tDel",
            false,
        ),
        (
            can(
                RETRY,
                matches!(state, Some(JobState::Failed | JobState::Cancelled)),
            ),
            "Re&try",
            false,
        ),
        (0, "", false),
        (CLEAR, "C&lear finished", false),
    ];
    let queue = shown_queue();
    let chosen = popup(h.queue, &items, Some(at));
    if shown_queue() != queue {
        return;
    }
    let op = match chosen {
        PAUSE => Some(JobOp::Pause),
        RESUME => Some(JobOp::Resume),
        CANCEL => Some(JobOp::Cancel),
        RETRY => Some(JobOp::Retry),
        CLEAR => {
            emit(Event::ClearFinished);
            None
        }
        _ => None,
    };
    if let (Some(op), Some((id, _))) = (op, job) {
        emit(Event::Job(id, op));
    }
}

fn delete(h: &Handles) {
    let keys: Vec<Vec<u8>> = picked(h, true).into_iter().map(|r| r.key).collect();
    if !keys.is_empty() {
        emit(Event::Delete(keys));
    }
}

/// Draws a queue row's progress bar in its Progress column.
fn draw_progress(h: &Handles, cd: &NMLVCUSTOMDRAW) -> LRESULT {
    let item = cd.nmcd.dwItemSpec;
    let Some(pct) = with(|s| s.queue_shown.get(item).map(|r| r.percent)).flatten() else {
        return LRESULT(CDRF_DODEFAULT as isize);
    };
    let Some(pct) = pct else {
        return LRESULT(CDRF_DODEFAULT as isize);
    };
    let dpi = with(|s| s.dpi).unwrap_or(96);
    // SAFETY: drawing into the DC the list gave for this paint; brushes
    // made here are deleted here.
    unsafe {
        let mut rc = RECT {
            top: 2,
            left: LVIR_BOUNDS as i32,
            ..Default::default()
        };
        if SendMessageW(
            h.queue,
            LVM_GETSUBITEMRECT,
            Some(WPARAM(item)),
            Some(LPARAM(&raw mut rc as isize)),
        )
        .0 == 0
        {
            return LRESULT(CDRF_DODEFAULT as isize);
        }
        let dc = cd.nmcd.hdc;
        FillRect(dc, &rc, crate::ui_theme::sys_brush(COLOR_WINDOW));
        let bar_h = scale(6, dpi);
        let mut track = RECT {
            left: rc.left + scale(4, dpi),
            right: rc.right - scale(6, dpi),
            top: rc.top + (rc.bottom - rc.top - bar_h) / 2,
            bottom: 0,
        };
        track.bottom = track.top + bar_h;
        let grey = CreateSolidBrush(crate::ui_theme::track_color());
        FillRect(dc, &track, grey);
        let _ = DeleteObject(HGDIOBJ(grey.0));
        let mut done = track;
        done.right = track.left + (track.right - track.left) * pct.min(100) as i32 / 100;
        let green = CreateSolidBrush(COLORREF(0x000F_7B0F));
        FillRect(dc, &done, green);
        let _ = DeleteObject(HGDIOBJ(green.0));
    }
    LRESULT(CDRF_SKIPDEFAULT as isize)
}

fn notify(h: &Handles, lparam: LPARAM) -> LRESULT {
    // SAFETY: Windows passes a valid NMHDR (and the larger struct its code
    // says) for the duration of WM_NOTIFY.
    unsafe {
        let hdr = &*(lparam.0 as *const NMHDR);
        let from = hdr.hwndFrom;
        if from == h.queue && hdr.code == NM_CUSTOMDRAW {
            let cd = &*(lparam.0 as *const NMLVCUSTOMDRAW);
            let stage = cd.nmcd.dwDrawStage;
            if stage == CDDS_PREPAINT {
                return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
            }
            if stage == CDDS_ITEMPREPAINT {
                return LRESULT(CDRF_NOTIFYSUBITEMDRAW as isize);
            }
            if stage.0 == (CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0) && cd.iSubItem == 2 {
                return draw_progress(h, cd);
            }
            return LRESULT(CDRF_DODEFAULT as isize);
        }
        let remote = from == h.remote.list;
        let is_list = remote || from == h.local.list;
        match hdr.code {
            NM_SETFOCUS => emit(Event::Focused),
            NM_DBLCLK if is_list => {
                let act = &*(lparam.0 as *const NMITEMACTIVATE);
                if act.iItem >= 0 {
                    open(h, remote, Some(act.iItem as usize));
                }
            }
            LVN_KEYDOWN if is_list || from == h.queue => {
                let key = &*(lparam.0 as *const NMLVKEYDOWN);
                let vk = key.wVKey;
                if from == h.queue {
                    if vk == VK_DELETE.0
                        && let Some(id) = selected(h.queue)
                            .first()
                            .and_then(|&i| with(|s| s.queue_shown.get(i).map(|r| r.id)).flatten())
                    {
                        emit(Event::Job(id, JobOp::Cancel));
                    }
                } else if vk == VK_BACK.0 {
                    go_up(h, remote);
                } else if vk == VK_DELETE.0 && remote {
                    delete(h);
                } else if vk == VK_F5.0 {
                    if remote {
                        emit(Event::Refresh);
                    } else {
                        refresh_local(h, true);
                    }
                } else if vk == u16::from(b'A') && key_down(VK_CONTROL.0) {
                    set_state(from, -1, LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0), true);
                }
            }
            LVN_BEGINDRAG if is_list => {
                with(|s| s.drag = Some(Drag::Files { remote }));
                let _ = SetCapture(h.panel);
            }
            _ => {}
        }
    }
    LRESULT(0)
}

/// The list under screen point `pt` and the folder row in it (not "..")
/// the point is on.
fn target_at(h: &Handles, pt: POINT) -> Option<(bool, Option<usize>)> {
    // SAFETY: plain queries and messages to our own controls.
    unsafe {
        let under = WindowFromPoint(pt);
        let remote = if under == h.remote.list || IsChild(h.remote.list, under).as_bool() {
            true
        } else if under == h.local.list || IsChild(h.local.list, under).as_bool() {
            false
        } else {
            return None;
        };
        let list = if remote { h.remote.list } else { h.local.list };
        let mut hit = LVHITTESTINFO {
            pt,
            ..Default::default()
        };
        let _ = ScreenToClient(list, &mut hit.pt);
        let i = SendMessageW(
            list,
            LVM_HITTEST,
            Some(WPARAM(0)),
            Some(LPARAM(&raw mut hit as isize)),
        )
        .0;
        let folder = (i >= 0).then_some(i as usize).filter(|&i| {
            with(|s| {
                let rows = if remote {
                    &s.remote_shown
                } else {
                    &s.local_shown
                };
                rows.get(i).is_some_and(|r| r.dir && !r.up)
            })
            .unwrap_or(false)
        });
        Some((remote, folder))
    }
}

/// Where files dropped from Explorer at screen point `pt` landed.
pub fn drop_at(pt: POINT) -> DropAt {
    let Some(h) = handles() else {
        return DropAt::Pane;
    };
    match target_at(&h, pt) {
        Some((true, folder)) => DropAt::Remote(
            folder.and_then(|i| with(|s| s.remote_shown.get(i).map(|r| r.key.clone())).flatten()),
        ),
        Some((false, _)) => DropAt::Local,
        None => DropAt::Pane,
    }
}

/// Lights up the folder row a drag would drop into (and nothing else).
fn light(target: Option<(HWND, i32)>) {
    let old = with(|s| std::mem::replace(&mut s.lit, target)).flatten();
    if old == target {
        return;
    }
    let flag = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_DROPHILITED.0);
    if let Some((list, i)) = old {
        set_state(list, i, flag, false);
    }
    if let Some((list, i)) = target {
        set_state(list, i, flag, true);
    }
}

fn drag_move(h: &Handles, drag: Drag, x: i32, y: i32) {
    match drag {
        Drag::Split => {
            let Some((a, lists, gap)) = with(|s| (s.arrange, s.lists, s.gap)) else {
                return;
            };
            let gap_size = if a.stacked { gap.h } else { gap.w };
            let pos = if a.stacked { y } else { x };
            let split = crate::files::split_at(a, lists, gap_size, pos);
            if split != a.split {
                with(|s| s.arrange.split = split);
                layout(h);
            }
        }
        Drag::Files { remote } => {
            let mut pt = POINT { x, y };
            // SAFETY: plain conversions and cursor changes.
            unsafe {
                let _ = ClientToScreen(h.panel, &mut pt);
                let target = target_at(h, pt).filter(|t| t.0 != remote);
                let list = if remote { h.local.list } else { h.remote.list };
                light(target.and_then(|t| t.1).map(|i| (list, i as i32)));
                // Local files can also go out onto a terminal pane.
                let out = !remote && outside(h, pt).is_some();
                let cursor = if target.is_some() || out {
                    IDC_HAND
                } else {
                    IDC_NO
                };
                if let Ok(c) = LoadCursorW(None, cursor) {
                    SetCursor(Some(c));
                }
            }
        }
    }
}

/// Screen point `pt` in the main window but off this pane: where, in the
/// main window's client coordinates.
fn outside(h: &Handles, pt: POINT) -> Option<POINT> {
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut panel = RECT::default();
    let mut main = RECT::default();
    // SAFETY: plain queries on our own windows.
    unsafe {
        GetWindowRect(h.panel, &mut panel).ok()?;
        GetWindowRect(h.main, &mut main).ok()?;
        let inside =
            |r: &RECT| pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom;
        if inside(&panel) || !inside(&main) {
            return None;
        }
        let mut at = pt;
        let _ = ScreenToClient(h.main, &mut at);
        Some(at)
    }
}

fn drag_end(h: &Handles, drag: Drag, x: i32, y: i32) {
    match drag {
        Drag::Split => {
            if let Some(a) = with(|s| s.arrange) {
                emit(Event::Arrange(a));
            }
        }
        Drag::Files { remote } => {
            let mut pt = POINT { x, y };
            // SAFETY: a plain conversion.
            unsafe {
                let _ = ClientToScreen(h.panel, &mut pt);
            }
            let Some((to_remote, folder)) = target_at(h, pt) else {
                if let Some(at) = outside(h, pt).filter(|_| !remote) {
                    let paths: Vec<PathBuf> = picked(h, false)
                        .into_iter()
                        .filter_map(|r| r.local)
                        .collect();
                    if !paths.is_empty() {
                        emit(Event::DropOut { paths, at });
                    }
                }
                return;
            };
            if to_remote == remote {
                return;
            }
            let name = with(|s| {
                let rows = if to_remote {
                    &s.remote_shown
                } else {
                    &s.local_shown
                };
                folder.and_then(|i| rows.get(i)).cloned()
            })
            .flatten();
            if to_remote {
                upload(h, name.map(|r| r.key));
            } else {
                download(h, name.and_then(|r| r.local));
            }
        }
    }
}

fn paint(h: &Handles) {
    let Some((title_font, font, dpi, host, labels, title_buttons_x)) = with(|s| {
        (
            s.title_font,
            s.font,
            s.dpi,
            s.view.as_ref().and_then(|v| v.host.clone()),
            s.labels,
            s.title_buttons_x,
        )
    }) else {
        return;
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
        let flags = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        let mut tr = RECT {
            left: pad,
            top: 0,
            right: rc.right - pad,
            bottom: scale(36, dpi),
        };
        SetTextColor(dc, COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT)));
        let mut text: Vec<u16> = "File transfer".encode_utf16().collect();
        let mut measure = tr;
        DrawTextW(dc, &mut text, &mut measure, flags | DT_CALCRECT);
        DrawTextW(dc, &mut text, &mut tr, flags);
        SelectObject(dc, HGDIOBJ(font.0));
        SetTextColor(dc, COLORREF(crate::ui_theme::sys_color(COLOR_GRAYTEXT)));
        let mut host: Vec<u16> = host
            .unwrap_or_else(|| "no SSH host".into())
            .encode_utf16()
            .collect();
        let mut hr = RECT {
            left: measure.right + scale(10, dpi),
            ..tr
        };
        hr.right = hr.right.min(title_buttons_x - scale(6, dpi));
        DrawTextW(
            dc,
            &mut host,
            &mut hr,
            flags | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS,
        );
        SetTextColor(dc, COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT)));
        for (r, name) in labels.iter().zip(["Local", "Remote"]) {
            let mut t: Vec<u16> = name.encode_utf16().collect();
            let mut lr = RECT {
                left: r.x,
                top: r.y,
                right: r.right(),
                bottom: r.bottom(),
            };
            DrawTextW(dc, &mut t, &mut lr, flags);
        }
        SelectObject(dc, old);
        let _ = EndPaint(h.panel, &ps);
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let h = handles().filter(|h| h.panel == hwnd);
    let x = i32::from(lparam.0 as u16 as i16);
    let y = i32::from((lparam.0 >> 16) as u16 as i16);
    match (msg, h) {
        (WM_PAINT, Some(h)) => {
            paint(&h);
            LRESULT(0)
        }
        (WM_ERASEBKGND, _) => LRESULT(1),
        (windows::Win32::UI::WindowsAndMessaging::WM_CTLCOLORSTATIC, Some(h)) => {
            // Labels on the window colour; a problem in red.
            let dc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut _);
            let control = HWND(lparam.0 as *mut _);
            let problem = control == h.status
                && with(|s| {
                    s.local_error.is_some()
                        || s.view
                            .as_ref()
                            .and_then(|v| v.status.as_ref())
                            .is_some_and(|v| v.1)
                })
                .unwrap_or(false);
            // SAFETY: the DC Windows passes for this control's paint.
            unsafe {
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(
                    dc,
                    if problem {
                        crate::ui_theme::error_color()
                    } else {
                        COLORREF(crate::ui_theme::sys_color(COLOR_WINDOWTEXT))
                    },
                );
                LRESULT(crate::ui_theme::sys_brush(COLOR_WINDOW).0 as isize)
            }
        }
        (WM_COMMAND, Some(h)) => {
            command(&h, wparam.0 & 0xFFFF, ((wparam.0 >> 16) & 0xFFFF) as u32);
            LRESULT(0)
        }
        (WM_NOTIFY, Some(h)) => notify(&h, lparam),
        (WM_CONTEXTMENU, Some(h)) => {
            let from = HWND(wparam.0 as *mut _);
            if from == h.local.list || from == h.remote.list {
                context_menu(&h, from, lparam);
            } else if from == h.queue {
                queue_menu(&h, lparam);
            }
            LRESULT(0)
        }
        (WM_SETCURSOR, Some(h)) if HWND(wparam.0 as *mut _) == h.panel => {
            let mut pt = POINT::default();
            // SAFETY: plain queries and a cursor change.
            unsafe {
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(h.panel, &mut pt);
                let over = with(|s| (s.gap.contains(pt.x, pt.y), s.arrange.stacked));
                if let Some((true, stacked)) = over {
                    let c = if stacked { IDC_SIZENS } else { IDC_SIZEWE };
                    if let Ok(c) = LoadCursorW(None, c) {
                        SetCursor(Some(c));
                    }
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        (WM_LBUTTONDOWN, Some(h)) => {
            if with(|s| s.gap.contains(x, y)).unwrap_or(false) {
                with(|s| s.drag = Some(Drag::Split));
                // SAFETY: our own window.
                unsafe {
                    let _ = SetCapture(h.panel);
                }
            }
            LRESULT(0)
        }
        (WM_MOUSEMOVE, Some(h)) => {
            if let Some(drag) = with(|s| s.drag).flatten() {
                drag_move(&h, drag, x, y);
            }
            LRESULT(0)
        }
        (WM_LBUTTONUP, Some(h)) => {
            let drag = with(|s| s.drag.take()).flatten();
            light(None);
            // SAFETY: we hold the capture when dragging.
            unsafe {
                let _ = ReleaseCapture();
            }
            if let Some(drag) = drag {
                drag_end(&h, drag, x, y);
            }
            LRESULT(0)
        }
        (WM_CAPTURECHANGED, Some(_)) => {
            // Escape, or another window took the mouse: no drop.
            with(|s| s.drag = None);
            light(None);
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => crate::ui_theme::def_window_proc(hwnd, msg, wparam, lparam),
    }
}
