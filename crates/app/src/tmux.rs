//! tmux control mode in the window: one [`Tmux`] per SSH pane, which
//! watches the pane's output for `tmux -CC` and, while tmux runs in
//! control mode, shows tmux's panes as native panes and tabs.
//!
//! The pane that ran `tmux -CC` (the gateway) carries the protocol and is
//! hidden meanwhile; it comes back, showing its shell, when tmux detaches
//! or exits. Two layouts are offered per session:
//!
//! - **FeroSSH layout**: every tmux window holds one pane and is one
//!   native pane, which FeroSSH sizes on its own (`refresh-client -C
//!   @1:80x24`, tmux 3.3 and later). Native panes may then each have
//!   their own font, size and theme. FeroSSH owns the split layout and
//!   keeps it, with each window's look, in the session option
//!   `@ferossh-layout`, so attaching from another PC brings it back.
//! - **tmux layout**: each tmux window is a tab, and tmux's own splits are
//!   native splits. Native resizes are sent back with `select-layout`.
//!
//! Like [`crate::ssh`], this only decides; the window carries out the
//! [`Action`]s. It edits the [`Workspace`] itself, so it is tested here
//! with no window, against scripted tmux output and a real tmux.

use std::collections::{BTreeMap, BTreeSet};

use ferossh_tmuxcc::layout::{self as tl, Cell, Kind, Shape};
use ferossh_tmuxcc::saved::{self, Look, Saved};
use ferossh_tmuxcc::{
    Client, Event, MIN_VERSION, Notification, PAUSE_VERSION, PaneId, SessionId, Version,
    WINDOW_SIZES_VERSION, WindowId, command,
};

use crate::layout::{Axis, Dir, Node, PaneId as Native, Workspace};
use crate::same_dir;
use crate::tmux_keys;
use crate::worktree::Worktree;

/// Output kept for a pane until its screen is restored.
const MAX_BUFFER: usize = 4 << 20;
/// How far (seconds) a pane's output may fall behind before tmux pauses
/// it (`pause-after`): tmux then drops what is waiting instead of piling
/// it up for a slow link, and FeroSSH asks for the pane's screen again.
/// Without it, tmux queues everything and drops the whole client once
/// it is minutes behind.
const PAUSE_AFTER: u32 = 10;
/// Panes whose output is kept before they are known.
const MAX_ORPHANS: usize = 64;
/// Windows FeroSSH shows; more are left to tmux.
const MAX_WINDOWS: usize = 256;
/// Bracketed-paste markers around a paste for a tmux pane ([`Tmux::input`]).
pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";
/// History lines brought back per pane, at most.
pub const MAX_HISTORY: usize = 10_000;

/// What each pane's state line holds, in this order.
const PANE_FORMAT: &str = "#{pane_id} #{window_id} #{pane_width} #{pane_height} \
    #{cursor_x} #{cursor_y} #{alternate_on} #{cursor_flag} #{keypad_cursor_flag} \
    #{keypad_flag} #{mouse_standard_flag} #{mouse_button_flag} #{mouse_all_flag} \
    #{mouse_sgr_flag} #{mouse_utf8_flag} #{origin_flag} #{insert_flag} #{wrap_flag} #{scroll_region_upper} \
    #{scroll_region_lower} #{window_layout} #{@ferossh-agent} #{@ferossh-worktree}";

/// The window option that marks a coding agent's window, holding the
/// agent's name ([`agent_name`]); it is read back on every attach, from
/// any PC.
pub const AGENT_OPTION: &str = "@ferossh-agent";

/// The short name a coding agent's window is known by: the program of its
/// command line (`claude` for `claude --resume`, `aider` for
/// `/opt/bin/aider`), letters, digits, `.`, `_` and `-` only, at most 32.
pub fn agent_name(command: &str) -> String {
    let program = command
        .split_whitespace()
        .find(|w| !w.contains('='))
        .unwrap_or("agent");
    let program = program.rsplit('/').next().unwrap_or(program);
    let name: String = program
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect();
    if name.is_empty() {
        "agent".to_owned()
    } else {
        name
    }
}

/// How a session's windows map to native panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One tmux window per native pane; FeroSSH owns the splits.
    Ferossh,
    /// tmux's own splits, one tab per window.
    Tmux,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Ferossh => "ferossh",
            Mode::Tmux => "tmux",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        match text.trim().to_ascii_lowercase().as_str() {
            "ferossh" => Some(Mode::Ferossh),
            "tmux" => Some(Mode::Tmux),
            _ => None,
        }
    }
}

/// What the window must do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send to the gateway's host: command lines for tmux.
    Write(Vec<u8>),
    /// Terminal output for the gateway pane itself (outside control mode).
    GatewayOutput(Vec<u8>),
    /// The workspace has leaf `native` now: make its pane, `cols` x
    /// `rows`, in tmux pane `pane`, looking like `look` where it says.
    Create {
        native: Native,
        pane: PaneId,
        look: Look,
        cols: u16,
        rows: u16,
    },
    /// Host output for a native pane.
    Feed { native: Native, bytes: Vec<u8> },
    /// tmux sized the pane.
    Resize {
        native: Native,
        cols: u16,
        rows: u16,
    },
    /// The pane left the workspace: drop it.
    Remove { native: Native },
    /// Give this pane the keyboard.
    Focus { native: Native },
    /// The gateway is in the workspace again, at the place the tmux tabs
    /// had.
    ShowGateway,
    /// Tabs or splits changed: lay out again.
    Structure,
    /// Offer these sessions to switch to.
    Picker(Vec<String>),
    /// Tell the user, in the gateway pane.
    Notice(String),
    /// Tell the user in a message box: the gateway is hidden while
    /// attached.
    Tell(String),
    /// These panes, made before their window was known to run a coding
    /// agent, turn out to be one's: watch them as agent panes now.
    AgentMarked(Vec<Native>),
    /// Other clients attached to the session keep a window smaller than
    /// the native panes (tmux sizes a window to its smallest client):
    /// offer to detach them. Asked once per attach.
    Crowded {
        session: SessionId,
        have: (u16, u16),
        want: (u16, u16),
        others: u32,
    },
    /// The directory a pane is in, asked for by [`Tmux::ask_dir`]: save it
    /// as the host's start directory.
    StartDir(String),
    /// The directory native pane `native` is in, asked for by
    /// [`Tmux::ask_diff_dir`]: show what changed in its git repository.
    DiffDir { native: Native, dir: String },
    /// Native pane `native`'s directory, as asked by
    /// [`Tmux::ask_worktree_dir`]: make a worktree of its repository.
    /// `None` when tmux did not say: one answer per ask, in order.
    WorktreeDir { native: Native, dir: Option<String> },
}

/// Where a window made from a native action goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    Split { next_to: Native, dir: Dir },
    Tab { after: Native },
}

/// A split waiting for its pane's directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Split {
    /// A window placed next to a native pane (FeroSSH layout).
    Window(Placement),
    /// A coding agent's window, placed like [`Split::Window`]; its name
    /// and command wait in [`Tmux::agents_waiting`] under this number.
    Agent(Placement, u32),
    /// A tmux split of `pane` (tmux layout).
    Pane { pane: PaneId, flags: &'static str },
}

#[derive(Debug, Clone)]
enum Tag {
    Version,
    ModeOption,
    LayoutOption,
    /// Every pane of the session, to build the native panes from.
    Restore,
    /// The panes of one window that appeared later.
    Discover(WindowId),
    Capture(PaneId),
    CaptureAlternate(PaneId),
    /// A paused pane's state, to draw its screen again after `%pause`.
    Resync(PaneId),
    NewWindow(Placement),
    /// The window and pane a coding agent was started in.
    AgentWindow(Placement, u32),
    SplitPane,
    /// A pane's directory, to open a split there.
    SplitHere(Split),
    /// A pane's directory, to start new windows and panes in.
    StartDirHere,
    /// A pane's directory, to show what changed there.
    DiffDirHere(Native),
    /// A pane's directory, to make an agent's worktree from.
    WorktreeDirHere(Native),
    Sessions,
    NewSession,
    /// The prefix keys and base index, for tmux keybindings.
    Keys,
    /// The size tmux gave a window after we asked for this one, and how
    /// many clients the session has.
    SizeCheck(WindowId, (u16, u16)),
    /// How many clients session `.0` has.
    Clients(SessionId),
    /// Our own client's name, before detaching the others of session `.0`.
    OwnClient(SessionId),
    /// The clients of session `.0` to detach, all but ours (`.1`).
    DetachOthers(SessionId, String),
    /// The server's socket, before opening it to the accounts asked for.
    ShareSocket(Share),
    /// Access given to account `.0`: an error is told.
    ShareAdd(String),
    /// The server's socket, before stopping sharing.
    UnshareSocket,
    /// Who may connect, to take everyone else off.
    ShareList,
    /// Who may connect after taking the others off, to check they are.
    ShareCheck,
    /// The session's read-only clients, to detach when sharing stops.
    Watchers(SessionId),
    /// A read-only client detached when sharing stops.
    WatcherOff,
    /// The reply does not matter.
    Ignore,
}

/// The first tmux with an access list (`server-access`): older ones let
/// anyone who can open the socket in.
pub const ACCESS_LIST_VERSION: Version = Version { major: 3, minor: 3 };

/// The first tmux that keeps a read-only account read-only in control
/// mode too. Before it, a read-only client running `tmux -C` can still
/// type into panes and run commands as the server's owner (`run-shell`),
/// so read-only access for another account would be a promise tmux
/// does not keep.
pub const READ_ONLY_VERSION: Version = Version { major: 3, minor: 9 };

/// How Stop sharing went, as its replies come in.
#[derive(Debug)]
struct Unsharing {
    /// How access ended, once known.
    access: Option<Access>,
    /// Whether the session's clients were listed, once known.
    listed: Option<bool>,
    /// Read-only watchers whose detach tmux has not answered yet.
    detaching: usize,
    /// Read-only watchers tmux detached.
    detached: usize,
    /// Read-only watchers tmux did not detach.
    missed: usize,
    /// The account that owns the shared server.
    owner: String,
}

/// How Stop sharing ended for other accounts' access.
#[derive(Debug)]
enum Access {
    /// tmux lists nobody else any more.
    Stopped,
    /// tmux still lists these accounts.
    Left(Vec<String>),
    /// tmux did not say who may connect.
    Unlisted,
    /// The session is on the private server.
    Private,
    /// tmux did not say which server this is.
    Unknown,
}

/// Sharing asked for: the accounts that may join, and whether they may
/// type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub accounts: Vec<String>,
    pub can_type: bool,
}

/// A pane's terminal state, from [`PANE_FORMAT`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PaneState {
    width: u16,
    height: u16,
    cursor_x: u16,
    cursor_y: u16,
    alternate: bool,
    cursor: bool,
    keypad_cursor: bool,
    keypad: bool,
    mouse_standard: bool,
    mouse_button: bool,
    /// Motion reporting (1003); tmux's `mouse_any_flag` is any mouse
    /// mode at all, not this.
    mouse_all: bool,
    mouse_sgr: bool,
    mouse_utf8: bool,
    origin: bool,
    insert: bool,
    wrap: bool,
    region: (u16, u16),
}

#[derive(Debug, Clone)]
struct PaneLine {
    pane: PaneId,
    window: WindowId,
    state: PaneState,
    layout: String,
    /// The window's [`AGENT_OPTION`], when it has one.
    agent: Option<String>,
    /// The window's [`crate::worktree::OPTION`], when it has one.
    worktree: Option<Worktree>,
}

fn parse_pane_line(line: &[u8]) -> Option<PaneLine> {
    // The agent option comes next to last and the worktree option last,
    // whole even if someone set it to a value with spaces or bytes that
    // are not UTF-8 (then it is not ours and is ignored, and the pane
    // still counts); the tests' lines may leave them out.
    let mut parts: Vec<&[u8]> = line.splitn(23, |&b| b == b' ').collect();
    let text = |m: &[u8]| std::str::from_utf8(m).ok().map(str::to_owned);
    let worktree = match parts.len() {
        23 => parts
            .pop()
            .and_then(text)
            .and_then(|v| crate::worktree::parse_option(&v)),
        _ => None,
    };
    let agent = match parts.len() {
        22 => parts
            .pop()
            .and_then(text)
            .and_then(|v| agent_name_of_option(&v)),
        _ => None,
    };
    // A worktree counts only for an agent's window.
    let worktree = worktree.filter(|_| agent.is_some());
    let f: Vec<&str> = parts
        .into_iter()
        .map(|p| std::str::from_utf8(p).ok())
        .collect::<Option<_>>()?;
    let [
        pane,
        window,
        width,
        height,
        cx,
        cy,
        alt,
        cursor,
        kc,
        kp,
        ms,
        mb,
        ma,
        sgr,
        utf8,
        origin,
        ins,
        wrap,
        top,
        bottom,
        layout,
    ] = f.as_slice()
    else {
        return None;
    };
    let n = |s: &str| s.parse::<u16>().unwrap_or(0).min(tl::MAX_SIZE);
    let b = |s: &str| s == "1";
    Some(PaneLine {
        pane: pane.strip_prefix('%')?.parse().ok()?,
        window: window.strip_prefix('@')?.parse().ok()?,
        agent,
        worktree,
        state: PaneState {
            width: n(width).max(1),
            height: n(height).max(1),
            cursor_x: n(cx),
            cursor_y: n(cy),
            alternate: b(alt),
            cursor: *cursor != "0",
            keypad_cursor: b(kc),
            keypad: b(kp),
            mouse_standard: b(ms),
            mouse_button: b(mb),
            mouse_all: b(ma),
            mouse_sgr: b(sgr),
            mouse_utf8: b(utf8),
            origin: b(origin),
            insert: b(ins),
            wrap: *wrap != "0",
            region: (n(top), n(bottom)),
        },
        layout: (*layout).to_owned(),
    })
}

/// The bytes that redraw a pane as tmux has it: its history and screens,
/// the modes and the cursor.
///
/// `current` is plain `capture-pane`: what the pane shows now, after any
/// history asked for. `saved` is `capture-pane -a`: the screen tmux keeps
/// aside while a program (vim, less, mc) has the alternate screen on,
/// which is the *normal* screen, the shell's; it is empty otherwise. So
/// with the alternate screen on, the history and `saved` go to the normal
/// screen and the last `height` lines of `current` to the alternate one.
fn restore_bytes(state: &PaneState, current: &[Vec<u8>], saved: &[Vec<u8>]) -> Vec<u8> {
    let (normal, alternate): (Vec<Vec<u8>>, &[Vec<u8>]) = if state.alternate {
        let split = current.len().saturating_sub(usize::from(state.height));
        let (history, shown) = current.split_at(split);
        // The shell's screen up to its last line that holds something:
        // the shell's cursor was below it when the program started, so
        // that is where its prompt comes back once the program quits.
        let used = saved
            .iter()
            .rposition(|l| !l.is_empty())
            .map_or(0, |i| i + 1);
        let mut normal = history.to_vec();
        normal.extend(saved.iter().take(used).cloned());
        (normal, shown)
    } else {
        (current.to_vec(), &[])
    };
    let mut out = b"\x1b[0m\x1b[H\x1b[2J".to_vec();
    let join = |out: &mut Vec<u8>, lines: &[Vec<u8>]| {
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(line);
        }
        out.extend_from_slice(b"\x1b[0m");
    };
    join(&mut out, &normal);
    if state.alternate {
        if !normal.is_empty() {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\x1b[?1049h\x1b[H\x1b[2J");
        join(&mut out, alternate);
    }
    let (top, bottom) = state.region;
    if top != 0 || bottom.saturating_add(1) != state.height {
        out.extend_from_slice(format!("\x1b[{};{}r", top + 1, bottom + 1).as_bytes());
    }
    for (on, seq) in [
        (state.keypad_cursor, &b"\x1b[?1h"[..]),
        (state.keypad, b"\x1b="),
        (state.mouse_standard, b"\x1b[?1000h"),
        (state.mouse_button, b"\x1b[?1002h"),
        (state.mouse_all, b"\x1b[?1003h"),
        (state.mouse_utf8, b"\x1b[?1005h"),
        (state.mouse_sgr, b"\x1b[?1006h"),
        (state.origin, b"\x1b[?6h"),
        (state.insert, b"\x1b[4h"),
        (!state.wrap, b"\x1b[?7l"),
        (!state.cursor, b"\x1b[?25l"),
    ] {
        if on {
            out.extend_from_slice(seq);
        }
    }
    // tmux gives the cursor row from the top of the screen; in origin
    // mode the move counts from the top of the scroll region.
    let row = if state.origin {
        state.cursor_y.saturating_sub(top)
    } else {
        state.cursor_y
    };
    out.extend_from_slice(format!("\x1b[{};{}H", row + 1, state.cursor_x + 1).as_bytes());
    out
}

/// A pane's screen is being brought back: output waits.
#[derive(Debug, Default)]
struct Restoring {
    buffer: Vec<u8>,
    state: PaneState,
    normal: Vec<Vec<u8>>,
    /// After a `%pause`: the pane's terminal already has a screen and
    /// modes, reset first, and the state comes after the captures
    /// ([`Tag::Resync`]), so its cursor matches them; the alternate
    /// screen waits here for it.
    resync: Option<Vec<Vec<u8>>>,
}

/// Before a paused pane's screen is drawn again: back to the normal
/// screen and every mode the restore may set, off, so the redraw starts
/// from what a new terminal has. Scrollback is kept.
const RESYNC_RESET: &[u8] = b"\x1b[?1049l\x1b[r\x1b[?1l\x1b>\x1b[?1000l\x1b[?1002l\x1b[?1003l\
\x1b[?1005l\x1b[?1006l\x1b[?6l\x1b[4l\x1b[?7h\x1b[?25h";

#[derive(Debug)]
struct Pane {
    native: Native,
    window: WindowId,
    /// `Some` until the screen is restored.
    restoring: Option<Restoring>,
}

#[derive(Debug)]
struct Window {
    layout: Cell,
}

/// The size of a native pane in cells, for sizing tmux's panes.
pub type CellsOf<'a> = &'a dyn Fn(Native) -> Option<(u16, u16)>;
/// A native pane's font, size and theme, for saving.
pub type LookOf<'a> = &'a dyn Fn(Native) -> Look;

/// tmux on one SSH pane.
#[derive(Debug)]
pub struct Tmux {
    pub gateway: Native,
    client: Client<Tag>,
    /// The layout new sessions use, from the host's profile.
    preferred: Mode,
    /// History lines to bring back per pane.
    history: usize,
    /// Offer the other sessions after the next attach.
    pick_after_attach: bool,
    session: Option<(SessionId, String)>,
    version: Option<Version>,
    mode: Mode,
    mode_option: Option<String>,
    saved: Option<Saved>,
    /// The session is being read: notifications wait for it.
    restoring: bool,
    windows: BTreeMap<WindowId, Window>,
    panes: BTreeMap<PaneId, Pane>,
    orphans: BTreeMap<PaneId, Vec<u8>>,
    discovering: BTreeSet<WindowId>,
    placements: BTreeMap<WindowId, Placement>,
    /// A native split asked for this pane: focus it once it exists.
    focus_next: Option<PaneId>,
    /// Native panes of the session before a session switch; they go once
    /// the new session's panes are in.
    stale: Vec<Native>,
    sent_sizes: BTreeMap<WindowId, (u16, u16)>,
    sent_client_size: Option<(u16, u16)>,
    sent_layouts: BTreeMap<WindowId, String>,
    /// Other clients were found keeping a window small (asked once).
    crowded_asked: bool,
    /// How many clients the session attached has, ours included, as
    /// tmux last said (`#{session_attached}`); `None` until it has.
    clients: Option<u32>,
    /// Counts asked for after a client came or went, not answered yet.
    clients_asked: u32,
    saved_text: Option<String>,
    /// The session to attach again after a reconnect.
    pub reattach: Option<String>,
    /// Read the prefix keys on attach (Session > tmux > Use tmux keybindings).
    keys_wanted: bool,
    /// tmux's answer to [`tmux_keys::FORMAT`].
    keys: Option<String>,
    /// Where new windows and panes start unless a split asks for its
    /// pane's directory: the host's start directory, from its profile.
    pub start_dir: Option<String>,
    /// Coding agents asked for, until tmux answers with their windows:
    /// name, command line and worktree, by request number.
    agents_waiting: BTreeMap<u32, (String, String, Option<Worktree>)>,
    /// The number of the last coding agent asked for.
    agent_seq: u32,
    /// The windows that run a coding agent, by [`AGENT_OPTION`].
    agents: BTreeMap<WindowId, String>,
    /// The agent windows started in a worktree of their own, by
    /// [`crate::worktree::OPTION`].
    worktrees: BTreeMap<WindowId, Worktree>,
    /// Set while this client watches (`false`) or joins to type
    /// (`true`) someone's session: FeroSSH then changes nothing of
    /// theirs (layout, sizes, windows, options) and closing leaves.
    joined: Option<bool>,
    /// [`Tmux::join`] typed its line: the next control mode is joined.
    join_pending: Option<bool>,
    /// "Read-only" was told once this attach.
    told_read_only: bool,
    /// Joining was told once this attach (not again on session switches).
    told_joined: bool,
    /// [`Tmux::share`] typed its line: open the next control mode's
    /// server to these accounts once its version is known.
    share_pending: Option<Share>,
    /// Sharing asked for, until the new control session starts: only
    /// that session's version reply may act on it, never a late one
    /// from the session being left.
    share_staged: Option<Share>,
    /// Typed at the shell once control mode has ended (share or join
    /// asked for while attached).
    after_detach: Option<Vec<u8>>,
    /// Told once the session is read (how others join it).
    notice_after_attach: Option<String>,
    /// The last line typed starts the shared server (share or its
    /// reattach): the next control mode is on it.
    typed_shared: bool,
    /// The session attached runs on this account's shared server, not
    /// its private one.
    on_shared: bool,
    /// After a dropped connection: the session to attach again is this
    /// one on the shared server, which keeps whom it lets in.
    reattach_shared: Option<String>,
    /// Stop sharing, while its replies come in.
    unsharing: Option<Unsharing>,
    /// After a dropped connection: the session lost was someone else's
    /// or shared, and is not attached again, so no other session (such
    /// as the profile's) takes its place either.
    stay_at_shell: bool,
    /// tmux said it is a development build (`next-3.9`, `master`,
    /// `openbsd-7.6`): its version does not say which fixes it has.
    dev_build: bool,
}

/// A window's size and its session's client count, for [`Tag::SizeCheck`].
const SIZE_FORMAT: &str = "#{window_width} #{window_height} #{session_attached} #{window-size}";

#[derive(Debug, PartialEq, Eq)]
struct SizeCheck {
    have: (u16, u16),
    clients: u32,
    /// `window-size manual`: tmux keeps the size whatever the clients.
    manual: bool,
}

fn parse_size_check(text: &str) -> Option<SizeCheck> {
    let mut it = text.split_whitespace();
    let mut num = || it.next()?.parse::<u32>().ok();
    let (w, h, clients) = (num()?, num()?, num()?);
    let manual = it.next() == Some("manual");
    Some(SizeCheck {
        have: (u16::try_from(w).ok()?, u16::try_from(h).ok()?),
        clients,
        manual,
    })
}

/// An [`AGENT_OPTION`] value as read back: only what [`agent_name`]
/// writes, so a value set by hand cannot put anything odd on the screen.
fn agent_name_of_option(value: &str) -> Option<String> {
    (!value.is_empty() && agent_name(value) == value).then(|| value.to_owned())
}

fn win_of(text: &[u8]) -> Option<WindowId> {
    std::str::from_utf8(text)
        .ok()?
        .trim()
        .strip_prefix('@')?
        .parse()
        .ok()
}

/// A native split tree for one window's tmux layout.
fn native_tree(cell: &Cell, native: &dyn Fn(PaneId) -> Option<Native>) -> Option<Node> {
    match &cell.kind {
        Kind::Pane(p) => native(*p).map(Node::Leaf),
        Kind::Row(cells) | Kind::Column(cells) => {
            let row = matches!(cell.kind, Kind::Row(_));
            let len = |c: &Cell| if row { c.width } else { c.height };
            // Right-nested: the first child against all the others.
            let mut node: Option<(Node, u16)> = None;
            for c in cells.iter().rev() {
                let Some(tree) = native_tree(c, native) else {
                    continue;
                };
                node = Some(match node {
                    None => (tree, len(c)),
                    Some((rest, rest_len)) => {
                        let both = len(c) + 1 + rest_len;
                        let ratio = f32::from(len(c)) / f32::from(both.saturating_sub(1).max(1));
                        (
                            Node::Split {
                                axis: if row { Axis::Row } else { Axis::Column },
                                ratio: ratio.clamp(0.1, 0.9),
                                first: Box::new(tree),
                                second: Box::new(rest),
                            },
                            both,
                        )
                    }
                });
            }
            node.map(|(n, _)| n)
        }
    }
}

impl Tmux {
    pub fn new(gateway: Native, preferred: Mode, history: usize) -> Self {
        Self {
            gateway,
            client: Client::new(),
            preferred,
            history: history.min(MAX_HISTORY),
            pick_after_attach: false,
            session: None,
            version: None,
            mode: preferred,
            mode_option: None,
            saved: None,
            restoring: false,
            windows: BTreeMap::new(),
            panes: BTreeMap::new(),
            orphans: BTreeMap::new(),
            discovering: BTreeSet::new(),
            placements: BTreeMap::new(),
            focus_next: None,
            stale: Vec::new(),
            sent_sizes: BTreeMap::new(),
            sent_client_size: None,
            sent_layouts: BTreeMap::new(),
            crowded_asked: false,
            clients: None,
            clients_asked: 0,
            saved_text: None,
            reattach: None,
            keys_wanted: false,
            keys: None,
            start_dir: None,
            agents_waiting: BTreeMap::new(),
            agent_seq: 0,
            worktrees: BTreeMap::new(),
            agents: BTreeMap::new(),
            joined: None,
            join_pending: None,
            told_read_only: false,
            told_joined: false,
            share_pending: None,
            share_staged: None,
            after_detach: None,
            notice_after_attach: None,
            typed_shared: false,
            on_shared: false,
            reattach_shared: None,
            unsharing: None,
            stay_at_shell: false,
            dev_build: false,
        }
    }

    /// In control mode.
    pub fn active(&self) -> bool {
        self.client.is_control()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The id of the session attached, while in control mode.
    pub fn session_id(&self) -> Option<SessionId> {
        self.client
            .is_control()
            .then_some(self.session.as_ref().map(|(id, _)| *id))
            .flatten()
    }

    /// The session attached, once tmux has said.
    pub fn session_name(&self) -> Option<&str> {
        self.session.as_ref().map(|(_, name)| name.as_str())
    }

    /// Per-window sizes, so FeroSSH layout works.
    fn window_sizes(&self) -> bool {
        self.version.is_some_and(|v| v >= WINDOW_SIZES_VERSION)
    }

    /// The native panes showing tmux panes.
    pub fn natives(&self) -> Vec<Native> {
        self.panes.values().map(|p| p.native).collect()
    }

    /// The tmux pane a native pane shows.
    pub fn pane_of(&self, native: Native) -> Option<PaneId> {
        self.panes
            .iter()
            .find_map(|(id, p)| (p.native == native).then_some(*id))
    }

    fn native_of(&self, pane: PaneId) -> Option<Native> {
        self.panes.get(&pane).map(|p| p.native)
    }

    /// The text to type at the gateway's shell to attach to `name`; the
    /// other sessions are offered once attached when `pick` is set.
    pub fn attach(&mut self, name: &str, pick: bool) -> Vec<u8> {
        self.pick_after_attach = pick;
        self.join_pending = None;
        self.share_staged = None;
        // A shared session comes back on the shared server, whose access
        // list still holds: nothing is granted again.
        let shared = self.reattach_shared.take().as_deref() == Some(name);
        self.typed_shared = shared;
        if shared {
            command::share_line(name).into_bytes()
        } else {
            command::attach_line(name).into_bytes()
        }
    }

    /// After a dropped connection: whether session `name`, to attach
    /// again, was on the shared server (forgotten here).
    pub fn take_reattach_shared(&mut self, name: &str) -> bool {
        self.reattach_shared.take().as_deref() == Some(name)
    }

    /// The next [`Tmux::attach`] of session `name` goes to the shared
    /// server, as it was before the connection dropped.
    pub fn reattach_on_shared(&mut self, name: &str) {
        self.reattach_shared = Some(name.to_owned());
    }

    /// After a dropped connection: whether the login should stay at the
    /// shell, attaching no tmux session at all (asked once).
    pub fn take_stay_at_shell(&mut self) -> bool {
        std::mem::take(&mut self.stay_at_shell)
    }

    /// The session attached runs on this account's shared server.
    pub fn on_shared_server(&self) -> bool {
        self.on_shared && self.client.is_control()
    }

    /// Whether this client watches (`Some(false)`) or joined to type
    /// (`Some(true)`) someone else's session.
    pub fn joined(&self) -> Option<bool> {
        self.joined.filter(|_| self.client.is_control())
    }

    /// Bytes to type at the gateway's shell now, or once control mode
    /// has ended when attached (detaching first).
    fn type_line(&mut self, line: String) -> Vec<Action> {
        let mut out = Vec::new();
        if self.client.is_control() {
            self.after_detach = Some(line.into_bytes());
            self.reattach = None;
            self.send("detach-client", Tag::Ignore, &mut out);
        } else {
            out.push(Action::Write(line.into_bytes()));
        }
        out
    }

    /// Session > tmux > Join a shared session: watch (or, with
    /// `can_type`, join to type) session `session` (tmux's most recent
    /// for `None`, the others offered once attached) that account
    /// `owner` shares, or of this account's own tmux for `None`. `owner`
    /// must be a [`command::account_name`].
    pub fn join(
        &mut self,
        owner: Option<&str>,
        session: Option<&str>,
        can_type: bool,
    ) -> Vec<Action> {
        self.pick_after_attach = session.is_none();
        self.share_staged = None;
        self.typed_shared = false;
        self.join_pending = Some(can_type);
        self.type_line(command::join_line(owner, session, can_type))
    }

    /// Session > tmux > Share this session, for other accounts: session
    /// `name` on this account's shared tmux server (started if needed),
    /// open to `share.accounts` once tmux's version says it can keep the
    /// promise.
    pub fn share(&mut self, name: &str, share: Share) -> Vec<Action> {
        self.pick_after_attach = false;
        self.join_pending = None;
        self.share_staged = Some(share);
        self.typed_shared = true;
        self.type_line(command::share_line(name))
    }

    /// Session > tmux > Share this session, for this account only: the
    /// session needs nothing changed, so say how to join it, now or once
    /// attached.
    pub fn share_same_account(&mut self, text: String) -> Vec<Action> {
        if self.client.is_control() && !self.restoring {
            vec![Action::Tell(text)]
        } else {
            self.notice_after_attach = Some(text);
            Vec::new()
        }
    }

    /// Session > tmux > Stop sharing: takes every other account off the
    /// server's access list (which detaches them), makes its socket
    /// private again, and detaches this session's read-only clients.
    pub fn unshare(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        // One Stop sharing at a time: a second one waits for the first.
        if !self.client.is_control() || self.joined.is_some() || self.unsharing.is_some() {
            return out;
        }
        self.unsharing = Some(Unsharing {
            access: None,
            listed: self.session.is_none().then_some(false),
            detaching: 0,
            detached: 0,
            missed: 0,
            owner: String::new(),
        });
        self.send(
            "display-message -p '#{socket_path}'",
            Tag::UnshareSocket,
            &mut out,
        );
        if let Some((session, _)) = self.session {
            self.send(
                &format!("list-clients -t ${session} -F '#{{client_name}} #{{client_readonly}}'"),
                Tag::Watchers(session),
                &mut out,
            );
        }
        out
    }

    /// The accounts besides the owner and root that `server-access -l`
    /// (its reply `lines`) lets in.
    fn others_allowed(&self, lines: &[Vec<u8>]) -> Vec<String> {
        let owner = self.unsharing.as_ref().map(|u| u.owner.as_str());
        lines
            .iter()
            // Every entry counts, also names FeroSSH would not accept
            // typed in (spaces too): quoted, they can still be taken off.
            // Only a line in tmux's "NAME (R)" / "NAME (W)" form can be
            // the owner or root (whom tmux will not take off); any other
            // line counts as still let in, so nothing unknown is passed by.
            .filter_map(|line| {
                let text = String::from_utf8_lossy(line);
                let text = text.trim_end();
                match text
                    .strip_suffix(" (R)")
                    .or_else(|| text.strip_suffix(" (W)"))
                {
                    Some(name) if name == "root" || Some(name) == owner => None,
                    Some(name) if !name.is_empty() => Some(name.to_owned()),
                    _ if text.is_empty() => None,
                    _ => Some(text.to_owned()),
                }
            })
            .collect()
    }

    /// Stop sharing knows how access ended.
    fn unshare_step(&mut self, access: Access, out: &mut Vec<Action>) {
        if let Some(u) = &mut self.unsharing {
            u.access = Some(access);
        }
        self.unshare_told(out);
    }

    /// Tells how Stop sharing went, once both access and watchers are done.
    fn unshare_told(&mut self, out: &mut Vec<Action>) {
        let Some(Unsharing {
            access: Some(access),
            listed: Some(listed),
            detaching: 0,
            detached,
            missed,
            ..
        }) = &self.unsharing
        else {
            return;
        };
        let mut watchers = match (listed, detached) {
            (false, _) => {
                "FeroSSH could not list this session's watchers, so none were detached.".to_owned()
            }
            (true, 0) if *missed == 0 => "Nobody was watching read-only.".to_owned(),
            (true, 0) => String::new(),
            (true, 1) => "1 read-only watcher was detached.".to_owned(),
            (true, n) => format!("{n} read-only watchers were detached."),
        };
        if *missed > 0 {
            if !watchers.is_empty() {
                watchers.push(' ');
            }
            watchers.push_str(&format!(
                "tmux did not detach {missed} read-only watcher{}: they had left already, \
                 or they are still watching.",
                if *missed == 1 { "" } else { "s" }
            ));
        }
        let access = match access {
            Access::Stopped => "Stopped sharing: tmux confirms no other account may connect \
                                to your shared tmux server. FeroSSH also tried to set its socket back \
                                to private (tmux does not say whether that worked; the access \
                                list is what keeps others out)."
                .to_owned(),
            Access::Left(left) => format!(
                "Sharing has NOT fully stopped: tmux still lets in {}. Try Stop sharing \
                 again, or run tmux server-access -d for them.",
                left.join(", ")
            ),
            Access::Unlisted => "Sharing may not have stopped: tmux did not list who may \
                                 connect, so FeroSSH cannot confirm other accounts are off."
                .to_owned(),
            Access::Private => "This session is on your private tmux, which other accounts \
                                cannot reach, so nobody needed taking off."
                .to_owned(),
            Access::Unknown => "FeroSSH could not tell which tmux server this session is on, \
                                so it took nobody off. Attach to the shared session and stop \
                                sharing there."
                .to_owned(),
        };
        out.push(Action::Tell(format!("{access} {watchers}")));
        self.unsharing = None;
    }

    /// Opens the shared server to `share.accounts`, once it is known to
    /// be the shared server (`socket`) and its tmux keeps the promise.
    fn open_share(&mut self, socket: &str, share: Share, out: &mut Vec<Action>) {
        if !command::is_share_socket(socket) {
            self.on_shared = false;
            out.push(Action::Tell(
                "This tmux is not FeroSSH's shared server for your account, so the session \
                 stays private."
                    .into(),
            ));
            return;
        }
        self.on_shared = true;
        let version = self.version;
        let Some(v) = version.filter(|v| *v >= ACCESS_LIST_VERSION) else {
            out.push(Action::Tell(format!(
                "tmux {} cannot limit which accounts connect (tmux {ACCESS_LIST_VERSION} \
                 can), so this session stays private.",
                version.map_or_else(|| "here".to_owned(), |v| v.to_string())
            )));
            return;
        };
        if !share.can_type && (v < READ_ONLY_VERSION || self.dev_build) {
            out.push(Action::Tell(format!(
                "tmux {v} does not keep another account read-only: through tmux control \
                 mode it can still type and run commands as you (fixed in tmux \
                 {READ_ONLY_VERSION}; a development build may predate the fix). So this \
                 session stays private. Share it with \"Let them type\" only with someone \
                 you trust with your account."
            )));
            return;
        }
        let access = if share.can_type { "-w" } else { "-r" };
        for account in &share.accounts {
            let a = command::quote(account);
            // One command adds the account with its mode (-a would let
            // it in writable first, until the mode followed).
            self.send(
                &format!("server-access {access} {a}"),
                Tag::ShareAdd(account.clone()),
                out,
            );
        }
        // tmux turns away every account not on the list, so the socket
        // itself may be open now.
        self.send(
            &format!(
                "run-shell {}",
                command::quote(&format!("chmod 666 {socket}"))
            ),
            Tag::Ignore,
            out,
        );
        let owner = socket
            .strip_prefix(command::SHARE_DIR_PREFIX)
            .and_then(|r| r.strip_suffix("/default"))
            .unwrap_or_default();
        let name = self.session_name().unwrap_or("main");
        out.push(Action::Tell(format!(
            "Sharing tmux session {name} with {} ({}). They join with Session > tmux > Join a \
             shared session, shared by account {owner}, or: tmux -S {socket} attach -E{} -t {name}. \
             Session > tmux > Stop sharing ends it.",
            share.accounts.join(", "),
            if share.can_type {
                "they can type, and so run commands as you"
            } else {
                "watch only"
            },
            if share.can_type { "" } else { " -r" },
        )));
    }

    fn send(&mut self, line: &str, tag: Tag, out: &mut Vec<Action>) {
        if let Some(bytes) = self.client.command(line, tag) {
            out.push(Action::Write(bytes));
        }
    }

    /// Bytes from the gateway's host.
    pub fn feed(&mut self, ws: &mut Workspace, bytes: &[u8]) -> Vec<Action> {
        let mut out = Vec::new();
        for event in self.client.feed(bytes) {
            match event {
                Event::Terminal(b) => out.push(Action::GatewayOutput(b)),
                Event::Started => {
                    self.session = None;
                    self.clients = None;
                    self.clients_asked = 0;
                    self.version = None;
                    self.joined = self.join_pending.take();
                    self.share_pending = self.share_staged.take();
                    self.on_shared = std::mem::take(&mut self.typed_shared);
                    self.told_read_only = false;
                    self.told_joined = false;
                    self.unsharing = None;
                }
                Event::Notify(n) => self.on_notification(ws, n, &mut out),
                Event::ReplyStarted(tag) => self.on_reply_started(&tag),
                Event::Reply { tag, ok, lines } => self.on_reply(ws, tag, ok, lines, &mut out),
                Event::Ended => self.teardown(ws, &mut out),
            }
        }
        out
    }

    /// The gateway's connection dropped: tmux keeps running on the
    /// server; the native panes go and the gateway shows again.
    pub fn connection_lost(&mut self, ws: &mut Workspace) -> Vec<Action> {
        let mut out = Vec::new();
        if self.client.is_control() {
            self.reattach = self
                .session
                .as_ref()
                .filter(|_| self.joined.is_none())
                .map(|(_, n)| n.clone());
            self.reattach_shared = self.reattach.clone().filter(|_| self.on_shared);
            // A shared session whose name the share line would change is
            // not attached again at all: never the private server's
            // session of that name, nor another shared one.
            if self
                .reattach_shared
                .as_deref()
                .is_some_and(|n| command::session_name(n) != n)
            {
                self.reattach = None;
                self.reattach_shared = None;
            }
            self.stay_at_shell =
                self.reattach.is_none() && (self.joined.is_some() || self.on_shared);
            self.client.reset();
            self.after_detach = None;
            self.teardown(ws, &mut out);
        }
        out
    }

    fn on_notification(&mut self, ws: &mut Workspace, n: Notification, out: &mut Vec<Action>) {
        match n {
            Notification::Output { pane, data } => self.on_output(pane, data, out),
            Notification::SessionChanged { session, name } => {
                if self.session.as_ref().map(|(s, _)| *s) != Some(session) {
                    self.session = Some((session, name));
                    self.start_restore(out);
                }
            }
            // Asked after the restore's own question too, so the answer
            // is never older than the change.
            Notification::ClientsChanged => {
                if self.session.is_some() {
                    self.clients_asked += 1;
                }
                self.ask_clients(out);
            }
            // Before the guard: a pane tmux paused stays paused until it
            // is let go, restoring or not.
            Notification::Pause { pane } => self.on_pause(pane, out),
            _ if self.restoring => {}
            Notification::WindowAdd { window } => {
                if !self.windows.contains_key(&window) {
                    self.discover(window, out);
                }
            }
            Notification::WindowClose { window } => {
                self.remove_window(ws, window, out);
                out.push(Action::Structure);
            }
            Notification::LayoutChange { window, layout } => {
                self.on_layout_change(ws, window, &layout, out);
            }
            Notification::Exit { reason } if !reason.is_empty() => {
                out.push(Action::Notice(format!("tmux: {reason}")));
            }
            _ => {}
        }
    }

    /// tmux paused `pane` and dropped the output it held back: carry on,
    /// and draw the pane's screen as it is now. A pane being restored (or
    /// not shown yet) gets its screen from that restore.
    fn on_pause(&mut self, pane: PaneId, out: &mut Vec<Action>) {
        self.send(
            &format!("refresh-client -A '%{pane}:continue'"),
            Tag::Ignore,
            out,
        );
        let Some(p) = self.panes.get_mut(&pane).filter(|p| p.restoring.is_none()) else {
            return;
        };
        p.restoring = Some(Restoring {
            resync: Some(Vec::new()),
            ..Restoring::default()
        });
        // The screen only: the history before the pause is still in the
        // native scrollback. The state after, on the same line, so tmux
        // reads no output between them and the cursor matches.
        let normal = format!("capture-pane -p -e -q -t %{pane}");
        let alternate = format!("capture-pane -p -e -q -a -t %{pane}");
        let state = format!("list-panes -t %{pane} -F {}", command::quote(PANE_FORMAT));
        if let Some(bytes) = self.client.commands(vec![
            (normal.as_str(), Tag::Capture(pane)),
            (alternate.as_str(), Tag::CaptureAlternate(pane)),
            (state.as_str(), Tag::Resync(pane)),
        ]) {
            out.push(Action::Write(bytes));
        }
    }

    fn on_output(&mut self, pane: PaneId, data: Vec<u8>, out: &mut Vec<Action>) {
        match self.panes.get_mut(&pane) {
            Some(Pane {
                restoring: Some(r), ..
            }) => {
                if r.buffer.len() + data.len() <= MAX_BUFFER {
                    r.buffer.extend_from_slice(&data);
                }
            }
            Some(p) => out.push(Action::Feed {
                native: p.native,
                bytes: data,
            }),
            None => {
                if self.orphans.len() < MAX_ORPHANS || self.orphans.contains_key(&pane) {
                    let buf = self.orphans.entry(pane).or_default();
                    if buf.len() + data.len() <= MAX_BUFFER {
                        buf.extend_from_slice(&data);
                    }
                }
            }
        }
    }

    /// Reads the whole session again (on attach and session switch).
    fn start_restore(&mut self, out: &mut Vec<Action>) {
        // What is shown now goes once the new panes are in.
        self.stale.extend(self.natives());
        self.panes.clear();
        self.windows.clear();
        self.orphans.clear();
        self.discovering.clear();
        self.agents.clear();
        self.worktrees.clear();
        self.placements.clear();
        self.sent_sizes.clear();
        self.sent_client_size = None;
        self.sent_layouts.clear();
        self.crowded_asked = false;
        self.clients = None;
        self.saved_text = None;
        self.restoring = true;
        // With how many clients the session has, so closing can ask
        // first when others show it too.
        self.send(
            "display-message -p '#{version} #{session_attached_list}'",
            Tag::Version,
            out,
        );
        self.send(
            &format!("show-options -v -q {}", saved::MODE_OPTION),
            Tag::ModeOption,
            out,
        );
        self.send(
            &format!("show-options -v -q {}", saved::OPTION),
            Tag::LayoutOption,
            out,
        );
        self.send(
            &format!("list-panes -s -F {}", command::quote(PANE_FORMAT)),
            Tag::Restore,
            out,
        );
        if std::mem::take(&mut self.pick_after_attach) {
            self.list_sessions(out);
        }
        if self.keys_wanted {
            self.send_keys_query(out);
        }
    }

    /// Asks how many clients the session attached has: a list, since
    /// before tmux 3.3 `#{session_attached}` leaves control clients out.
    fn ask_clients(&mut self, out: &mut Vec<Action>) {
        let Some((session, _)) = self.session else {
            return;
        };
        self.send(
            &format!("list-clients -t ${session} -F '#{{client_name}}'"),
            Tag::Clients(session),
            out,
        );
    }

    /// How many other clients (another FeroSSH connection, a terminal)
    /// show the session this client attached, as tmux last said. Closing
    /// one of its panes in tmux closes it for them too. A session joined
    /// (someone else's) counts none: closing there only leaves it.
    pub fn others_attached(&self) -> u32 {
        if !self.client.is_control() || self.joined.is_some() {
            return 0;
        }
        self.clients.map_or(0, |c| c.saturating_sub(1))
    }

    /// Whether other clients may show the session this client attached:
    /// they do, or tmux has not said yet (no count, or a client came or
    /// went and the new count is not in). Closing then asks first rather
    /// than close panes another client shows.
    pub fn maybe_shared(&self) -> bool {
        if !self.client.is_control() || self.joined.is_some() {
            return false;
        }
        self.clients.is_none_or(|c| c > 1) || self.clients_asked > 0
    }

    fn send_keys_query(&mut self, out: &mut Vec<Action>) {
        self.send(
            &format!("display-message -p {}", command::quote(tmux_keys::FORMAT)),
            Tag::Keys,
            out,
        );
    }

    /// Session > tmux > Use tmux keybindings: read the prefix keys on each attach
    /// while on, and now if attached already.
    pub fn want_keys(&mut self, on: bool) -> Vec<Action> {
        let mut out = Vec::new();
        self.keys_wanted = on;
        if on && self.client.is_control() {
            self.send_keys_query(&mut out);
        }
        out
    }

    /// The prefix keys and base index tmux gave, as it wrote them.
    pub fn keys(&self) -> Option<&str> {
        self.keys.as_deref()
    }

    fn list_sessions(&mut self, out: &mut Vec<Action>) {
        self.send(
            "list-sessions -F '#{session_id} #{session_name}'",
            Tag::Sessions,
            out,
        );
    }

    /// Session > tmux > Switch tmux session.
    pub fn pick_session(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        self.list_sessions(&mut out);
        out
    }

    /// Attaches this client to session `name` (from the picker), or to a
    /// new session for `None`.
    pub fn switch_session(&mut self, name: Option<&str>) -> Vec<Action> {
        let mut out = Vec::new();
        match name {
            // A joined client never brings its environment along
            // (update-environment) into the owner's sessions.
            Some(n) => self.send(
                &format!(
                    "switch-client{} -t {}",
                    if self.joined.is_some() { " -E" } else { "" },
                    command::quote(&format!("={n}"))
                ),
                Tag::Ignore,
                &mut out,
            ),
            None if self.joined().is_some() => {
                self.refuse_joined(&mut out);
            }
            None => self.send(
                &format!(
                    "new-session -d{} -P -F '#{{session_name}}'",
                    self.start_cwd()
                ),
                Tag::NewSession,
                &mut out,
            ),
        }
        out
    }

    fn discover(&mut self, window: WindowId, out: &mut Vec<Action>) {
        if self.discovering.insert(window) {
            self.send(
                &format!("list-panes -t @{window} -F {}", command::quote(PANE_FORMAT)),
                Tag::Discover(window),
                out,
            );
        }
    }

    fn on_reply_started(&mut self, tag: &Tag) {
        // Output from before this point is in the capture.
        if let Tag::Capture(p) = tag
            && let Some(Pane {
                restoring: Some(r), ..
            }) = self.panes.get_mut(p)
        {
            r.buffer.clear();
        }
    }

    fn on_reply(
        &mut self,
        ws: &mut Workspace,
        tag: Tag,
        ok: bool,
        lines: Vec<Vec<u8>>,
        out: &mut Vec<Action>,
    ) {
        let first = || {
            lines
                .first()
                .map(|l| String::from_utf8_lossy(l).trim().to_owned())
                .unwrap_or_default()
        };
        match tag {
            Tag::Version => {
                let reply = first();
                let mut words = reply.split_whitespace();
                let version = words.next().unwrap_or_default();
                self.version = ok.then(|| Version::parse(version)).flatten();
                // Flow control, for every client but one that only watches
                // (it may not be let to continue a pane tmux paused).
                if self.version.is_some_and(|v| v >= PAUSE_VERSION) && self.joined != Some(false) {
                    self.send(
                        &format!("refresh-client -f pause-after={PAUSE_AFTER}"),
                        Tag::Ignore,
                        out,
                    );
                }
                // Releases are numbers ("3.4", "3.3a").
                self.dev_build = !version.starts_with(|c: char| c.is_ascii_digit());
                // The session's clients by name, suspended ones too (which
                // `#{session_attached}` leaves out). A count asked for after
                // a client came or went is newer. Before 3.3 control clients
                // can be missing: listed instead (after what sharing asks,
                // which waits on this reply).
                let list_clients = if self.version.is_some_and(|v| v >= WINDOW_SIZES_VERSION) {
                    if self.clients.is_none() {
                        self.clients = words
                            .next()
                            .map(|names| names.split(',').filter(|n| !n.is_empty()).count())
                            .and_then(|n| u32::try_from(n).ok())
                            .filter(|n| *n > 0);
                    }
                    false
                } else {
                    ok && self.clients_asked == 0
                };
                if let Some(share) = self.share_pending.take() {
                    self.send(
                        "display-message -p '#{socket_path}'",
                        Tag::ShareSocket(share),
                        out,
                    );
                }
                if list_clients {
                    self.ask_clients(out);
                }
            }
            Tag::ShareSocket(share) => {
                if ok {
                    self.open_share(&first(), share, out);
                } else {
                    out.push(Action::Tell(
                        "tmux did not say which server this is, so the session stays private."
                            .into(),
                    ));
                }
            }
            Tag::ShareAdd(account) => {
                let why = first();
                if !ok {
                    out.push(Action::Tell(format!(
                        "tmux did not let {account} in: {why}"
                    )));
                }
            }
            Tag::UnshareSocket => {
                let socket = first();
                let owner = ok.then(|| command::share_owner(&socket)).flatten();
                match (ok, owner) {
                    (true, Some(owner)) => {
                        if let Some(u) = &mut self.unsharing {
                            u.owner = owner;
                        }
                        // tmux does not say whether a shell command
                        // worked; who may connect is what it enforces,
                        // and that is checked below.
                        self.send(
                            &format!(
                                "run-shell {}",
                                command::quote(&format!("chmod 600 {socket}"))
                            ),
                            Tag::Ignore,
                            out,
                        );
                        self.send("server-access -l", Tag::ShareList, out);
                    }
                    (true, None) => self.unshare_step(Access::Private, out),
                    (false, _) => self.unshare_step(Access::Unknown, out),
                }
            }
            Tag::ShareList => {
                if !ok {
                    self.unshare_step(Access::Unlisted, out);
                    return;
                }
                for account in self.others_allowed(&lines) {
                    let line = format!("server-access -d {}", command::quote(&account));
                    self.send(&line, Tag::Ignore, out);
                }
                self.send("server-access -l", Tag::ShareCheck, out);
            }
            Tag::ShareCheck => {
                let access = if !ok {
                    Access::Unlisted
                } else {
                    match self.others_allowed(&lines) {
                        left if left.is_empty() => Access::Stopped,
                        left => Access::Left(left),
                    }
                };
                self.unshare_step(access, out);
            }
            Tag::Watchers(session) => {
                let listed = ok && self.session.as_ref().map(|(s, _)| *s) == Some(session);
                let mut n = 0;
                if listed {
                    for line in &lines {
                        let text = String::from_utf8_lossy(line);
                        if let Some(name) = text.trim().strip_suffix(" 1") {
                            let line = format!("detach-client -t {}", command::quote(name));
                            self.send(&line, Tag::WatcherOff, out);
                            n += 1;
                        }
                    }
                }
                if let Some(u) = &mut self.unsharing {
                    u.listed = Some(listed);
                    u.detaching = n;
                }
                self.unshare_told(out);
            }
            Tag::WatcherOff => {
                if let Some(u) = &mut self.unsharing {
                    u.detaching = u.detaching.saturating_sub(1);
                    if ok {
                        u.detached += 1;
                    } else {
                        u.missed += 1;
                    }
                }
                self.unshare_told(out);
            }
            Tag::ModeOption => self.mode_option = ok.then(first).filter(|m| !m.is_empty()),
            Tag::LayoutOption => {
                self.saved = ok.then(|| Saved::parse(&first())).flatten();
            }
            Tag::Restore => self.finish_restore(ws, &lines, out),
            Tag::Discover(window) => {
                self.discovering.remove(&window);
                if ok && !self.restoring {
                    self.integrate(ws, window, &lines, out);
                }
            }
            Tag::Capture(p) => {
                if let Some(Pane {
                    restoring: Some(r), ..
                }) = self.panes.get_mut(&p)
                {
                    r.normal = if ok { lines } else { Vec::new() };
                }
            }
            Tag::Resync(p) => {
                let state = lines
                    .iter()
                    .filter_map(|l| parse_pane_line(l))
                    .find(|l| l.pane == p)
                    .map(|l| l.state);
                if let Some(pane) = self.panes.get_mut(&p)
                    && pane.restoring.as_ref().is_some_and(|r| r.resync.is_some())
                    && let Some(r) = pane.restoring.take()
                {
                    let alternate = r.resync.unwrap_or_default();
                    let mut bytes = Vec::new();
                    // Without the state the screen is not drawn: what
                    // came since still is.
                    if let Some(state) = state.filter(|_| ok) {
                        bytes.extend_from_slice(RESYNC_RESET);
                        bytes.extend(restore_bytes(&state, &r.normal, &alternate));
                    }
                    bytes.extend_from_slice(&r.buffer);
                    if !bytes.is_empty() {
                        out.push(Action::Feed {
                            native: pane.native,
                            bytes,
                        });
                    }
                }
            }
            Tag::CaptureAlternate(p) => {
                // After a pause the state comes next and finishes it.
                if let Some(Pane {
                    restoring:
                        Some(Restoring {
                            resync: Some(alt), ..
                        }),
                    ..
                }) = self.panes.get_mut(&p)
                {
                    *alt = if ok { lines } else { Vec::new() };
                } else if let Some(pane) = self.panes.get_mut(&p)
                    && let Some(r) = pane.restoring.take()
                {
                    let alternate = if ok { lines } else { Vec::new() };
                    let mut bytes = restore_bytes(&r.state, &r.normal, &alternate);
                    bytes.extend_from_slice(&r.buffer);
                    out.push(Action::Feed {
                        native: pane.native,
                        bytes,
                    });
                }
            }
            Tag::NewWindow(placement) => {
                if let Some(w) = lines.first().and_then(|l| win_of(l)) {
                    self.placements.insert(w, placement);
                    if !self.windows.contains_key(&w) {
                        self.discover(w, out);
                    }
                }
            }
            Tag::AgentWindow(placement, n) => {
                let Some((name, line, worktree)) = self.agents_waiting.remove(&n) else {
                    return;
                };
                let text = first();
                let mut ids = text.split(' ');
                let w = ids.next().and_then(|t| win_of(t.as_bytes()));
                let p: Option<PaneId> = ids.next().and_then(|t| t.strip_prefix('%')?.parse().ok());
                let (Some(w), Some(p), true) = (w, p, ok) else {
                    out.push(Action::Notice(format!(
                        "tmux could not open a window for {name}: {text}"
                    )));
                    return;
                };
                // The worktree's mark first: whoever sees the agent sees
                // its branch too.
                if let Some(wt) = worktree {
                    self.send(
                        &format!(
                            "set-option -w -t @{w} {} {}",
                            crate::worktree::OPTION,
                            crate::worktree::tmux_quote(&crate::worktree::option_value(&wt))
                        ),
                        Tag::Ignore,
                        out,
                    );
                    self.worktrees.insert(w, wt);
                }
                self.mark_agent(w, name.clone(), out);
                self.send(
                    &format!(
                        "set-option -w -t @{w} {AGENT_OPTION} {}",
                        command::quote(&name)
                    ),
                    Tag::Ignore,
                    out,
                );
                let mut keys = line.into_bytes();
                keys.push(b'\r');
                for c in command::send_keys(p, &keys) {
                    self.send(&c, Tag::Ignore, out);
                }
                self.placements.insert(w, placement);
                if !self.windows.contains_key(&w) {
                    self.discover(w, out);
                }
            }
            Tag::SplitHere(split) => {
                // A pane that went meanwhile, or a path tmux cannot take:
                // the start directory, or where tmux starts shells.
                let mut cwd = if ok {
                    same_dir::tmux_cwd(&first())
                } else {
                    String::new()
                };
                if cwd.is_empty() {
                    cwd = self.start_cwd();
                }
                self.open_split(split, &cwd, out);
            }
            Tag::StartDirHere => {
                let dir = first();
                let dir = dir.trim_end_matches(['\r', '\n']);
                if ok && !dir.is_empty() {
                    out.push(Action::StartDir(dir.to_owned()));
                }
            }
            Tag::DiffDirHere(native) | Tag::WorktreeDirHere(native) => {
                let worktree = matches!(tag, Tag::WorktreeDirHere(_));
                // Spaces are part of a folder's name: only the line end
                // goes.
                let dir = lines
                    .first()
                    .map(|l| String::from_utf8_lossy(l).into_owned())
                    .unwrap_or_default();
                let dir = dir.trim_end_matches(['\r', '\n']);
                let said = (ok && !dir.is_empty()).then(|| dir.to_owned());
                if worktree {
                    out.push(Action::WorktreeDir { native, dir: said });
                } else if let Some(dir) = said {
                    out.push(Action::DiffDir { native, dir });
                } else {
                    out.push(Action::Notice(
                        "tmux did not say which folder that pane is in.".into(),
                    ));
                }
            }
            Tag::SplitPane => {
                self.focus_next = std::str::from_utf8(lines.first().map_or(&[][..], |l| l))
                    .ok()
                    .and_then(|t| t.trim().strip_prefix('%')?.parse().ok());
            }
            Tag::Sessions => {
                let current = self.session.as_ref().map(|(s, _)| *s);
                let names: Vec<String> = lines
                    .iter()
                    .filter_map(|l| {
                        let text = String::from_utf8_lossy(l);
                        let (id, name) = text.split_once(' ')?;
                        let id: SessionId = id.strip_prefix('$')?.parse().ok()?;
                        (Some(id) != current).then(|| name.to_owned())
                    })
                    .collect();
                if !names.is_empty() {
                    out.push(Action::Picker(names));
                }
            }
            Tag::NewSession => {
                let name = first();
                if ok && !name.is_empty() {
                    let mut more = self.switch_session(Some(&name));
                    out.append(&mut more);
                }
            }
            Tag::Keys => self.keys = ok.then(first).filter(|k| !k.is_empty()),
            Tag::SizeCheck(w, want) => {
                // Only the latest size asked for counts.
                let current = if self.window_sizes() {
                    self.sent_sizes.get(&w) == Some(&want)
                } else {
                    self.sent_client_size == Some(want)
                };
                if !ok || !current || self.crowded_asked {
                    return;
                }
                if let Some(check) = parse_size_check(&first())
                    && (check.have.0 < want.0 || check.have.1 < want.1)
                    && check.clients > 1
                    // A manual size stays whatever the other clients do.
                    && !check.manual
                    && let Some((session, _)) = self.session
                {
                    self.crowded_asked = true;
                    out.push(Action::Crowded {
                        session,
                        have: check.have,
                        want,
                        others: check.clients - 1,
                    });
                }
            }
            Tag::DetachOthers(session, me) => {
                if !ok || self.session.as_ref().map(|(s, _)| *s) != Some(session) {
                    return;
                }
                for name in lines
                    .iter()
                    .map(|l| String::from_utf8_lossy(l).trim().to_owned())
                {
                    if !name.is_empty() && name != me {
                        let line = format!("detach-client -t {}", command::quote(&name));
                        self.send(&line, Tag::Ignore, out);
                    }
                }
            }
            Tag::Clients(session) => {
                self.clients_asked = self.clients_asked.saturating_sub(1);
                if self.session.as_ref().map(|(s, _)| *s) == Some(session) {
                    // A failed count is unknown, never "alone".
                    self.clients = ok
                        .then(|| lines.iter().filter(|l| !l.is_empty()).count())
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| *n > 0);
                }
            }
            Tag::OwnClient(session) => {
                if !ok || self.session.as_ref().map(|(s, _)| *s) != Some(session) {
                    return;
                }
                let me = first();
                if me.is_empty() {
                    return;
                }
                let line = format!("list-clients -t ${session} -F '#{{client_name}}'");
                self.send(&line, Tag::DetachOthers(session, me), out);
            }
            Tag::Ignore => {}
        }
    }

    /// The session's panes are known: build the native tabs.
    fn finish_restore(&mut self, ws: &mut Workspace, lines: &[Vec<u8>], out: &mut Vec<Action>) {
        self.restoring = false;
        let version_ok = self.version.is_some_and(|v| v >= MIN_VERSION);
        if !version_ok {
            out.push(Action::Notice(format!(
                "FeroSSH needs tmux {MIN_VERSION} or later for its tmux integration{}; \
                 detaching.",
                self.version
                    .map(|v| format!(" (this server has {v})"))
                    .unwrap_or_default()
            )));
            self.send("detach-client", Tag::Ignore, out);
            return;
        }
        let stored = self.mode_option.as_deref().and_then(Mode::parse);
        self.mode = stored.unwrap_or(self.preferred);
        if self.mode == Mode::Ferossh && !self.window_sizes() {
            self.mode = Mode::Tmux;
            out.push(Action::Notice(format!(
                "tmux {} is older than {WINDOW_SIZES_VERSION}: panes follow tmux's own \
                 layout, one font per window.",
                self.version.unwrap_or(MIN_VERSION)
            )));
        } else if stored.is_none() && self.joined.is_none() {
            let line = format!("set-option -q {} {}", saved::MODE_OPTION, self.mode.name());
            self.send(&line, Tag::Ignore, out);
        }
        // Group the panes by window, in tmux's order.
        let mut order: Vec<WindowId> = Vec::new();
        let mut infos: BTreeMap<WindowId, Vec<PaneLine>> = BTreeMap::new();
        for line in lines.iter().filter_map(|l| parse_pane_line(l)) {
            if let Some(a) = &line.agent {
                self.agents.insert(line.window, a.clone());
            }
            if let Some(wt) = &line.worktree {
                self.worktrees.insert(line.window, wt.clone());
            }
            if !infos.contains_key(&line.window) {
                if order.len() >= MAX_WINDOWS {
                    continue;
                }
                order.push(line.window);
            }
            infos.entry(line.window).or_default().push(line);
        }
        let mut made: Vec<(Node, Native)> = Vec::new();
        let saved = self.saved.take().unwrap_or_default();
        let mut used: BTreeSet<WindowId> = BTreeSet::new();
        let mut active_tab = 0;
        // Windows the user hid stay hidden, out of the tabs.
        let mut hidden: Vec<Node> = Vec::new();
        if self.mode == Mode::Ferossh {
            for w in &saved.hidden {
                if !infos.contains_key(w) || !used.insert(*w) {
                    continue;
                }
                if let Some(tree) =
                    self.expand(ws, &saved::Node::Window(*w), &mut infos, &saved, out)
                {
                    hidden.push(tree);
                }
            }
        }
        if self.mode == Mode::Ferossh {
            for (i, tab) in saved.tabs.iter().enumerate() {
                let root = tab
                    .root
                    .clone()
                    .retain(&mut |w| infos.contains_key(&w) && used.insert(w));
                let Some(root) = root else {
                    continue;
                };
                if i == saved.active {
                    active_tab = made.len();
                }
                let tree = self.expand(ws, &root, &mut infos, &saved, out);
                if let Some(tree) = tree {
                    let focus = self
                        .first_native_of(tab.focus)
                        .or_else(|| tree.panes().first().copied())
                        .unwrap_or(0);
                    made.push((tree, focus));
                }
            }
        }
        for w in order {
            if used.contains(&w) {
                continue;
            }
            if let Some(tree) = self.expand(ws, &saved::Node::Window(w), &mut infos, &saved, out) {
                let focus = tree.panes().first().copied().unwrap_or(0);
                made.push((tree, focus));
            }
        }
        if made.is_empty() {
            // Nothing else to show: the hidden ones show after all.
            made = hidden
                .drain(..)
                .map(|tree| {
                    let focus = tree.panes().first().copied().unwrap_or(0);
                    (tree, focus)
                })
                .collect();
        }
        if made.is_empty() {
            return;
        }
        for tree in hidden {
            for native in tree.panes() {
                ws.add_hidden(native);
            }
        }
        // The new tabs go where the gateway or the old panes were.
        let mut place: Vec<Native> = self.stale.clone();
        place.push(self.gateway);
        let at = ws
            .tabs
            .iter()
            .position(|t| place.iter().any(|p| t.root.contains(*p)))
            .unwrap_or(ws.tabs.len());
        let first_focus = made
            .get(active_tab)
            .map(|(_, f)| *f)
            .or_else(|| made.first().map(|(_, f)| *f));
        for (i, (root, focus)) in made.into_iter().enumerate() {
            ws.insert_tab(at + i, root, focus);
        }
        for native in std::mem::take(&mut self.stale) {
            ws.remove_pane(native);
            out.push(Action::Remove { native });
        }
        ws.remove_pane(self.gateway);
        if let Some(f) = first_focus {
            ws.focus(f);
        }
        // Hidden panes of windows that show leave their tabs again (the
        // focus moving to a pane that shows), in FeroSSH's layout only.
        if self.mode == Mode::Ferossh {
            for p in &saved.hidden_panes {
                if let Some(n) = self.native_of(*p)
                    && ws.tab_of(n).is_some()
                {
                    ws.hide(n);
                }
            }
        }
        if first_focus.is_some() {
            out.push(Action::Focus {
                native: ws.focused(),
            });
        }
        let name = self.session_name().unwrap_or_default().to_owned();
        match self
            .joined
            .filter(|_| !std::mem::replace(&mut self.told_joined, true))
        {
            Some(false) => out.push(Action::Tell(format!(
                "Watching tmux session {name} read-only. Session > tmux > Detach tmux leaves it."
            ))),
            Some(true) => out.push(Action::Tell(format!(
                "Joined tmux session {name}: what you type goes to its owner's panes. Session > \
                 tmux > Detach tmux leaves it."
            ))),
            None => {}
        }
        if let Some(text) = self.notice_after_attach.take() {
            out.push(Action::Tell(text));
        }
        out.push(Action::Structure);
    }

    fn first_native_of(&self, window: WindowId) -> Option<Native> {
        self.panes
            .values()
            .find(|p| p.window == window)
            .map(|p| p.native)
    }

    /// Native panes for the windows of a saved tree: each window's own
    /// tmux layout in its place. Their panes are made and restored.
    fn expand(
        &mut self,
        ws: &mut Workspace,
        node: &saved::Node,
        infos: &mut BTreeMap<WindowId, Vec<PaneLine>>,
        saved: &Saved,
        out: &mut Vec<Action>,
    ) -> Option<Node> {
        match node {
            saved::Node::Window(w) => {
                let lines = infos.remove(w)?;
                let look = saved.looks.get(w).cloned().unwrap_or_default();
                self.add_window(ws, *w, &lines, &look, out)
            }
            saved::Node::Split {
                row,
                ratio,
                first,
                second,
            } => {
                let a = self.expand(ws, first, infos, saved, out);
                let b = self.expand(ws, second, infos, saved, out);
                match (a, b) {
                    (Some(a), Some(b)) => Some(Node::Split {
                        axis: if *row { Axis::Row } else { Axis::Column },
                        ratio: f32::from(*ratio) / 1000.0,
                        first: Box::new(a),
                        second: Box::new(b),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    /// Makes the native panes of window `w` that do not exist yet, and
    /// asks tmux for their screens. Returns the window's native tree.
    fn add_window(
        &mut self,
        ws: &mut Workspace,
        w: WindowId,
        lines: &[PaneLine],
        look: &Look,
        out: &mut Vec<Action>,
    ) -> Option<Node> {
        let layout = lines.first().and_then(|l| tl::parse(&l.layout))?;
        for line in lines {
            if self.panes.contains_key(&line.pane) || !layout.panes().contains(&line.pane) {
                continue;
            }
            let native = ws.alloc_id();
            out.push(Action::Create {
                native,
                pane: line.pane,
                look: look.clone(),
                cols: line.state.width,
                rows: line.state.height,
            });
            let buffer = self.orphans.remove(&line.pane).unwrap_or_default();
            self.panes.insert(
                line.pane,
                Pane {
                    native,
                    window: w,
                    restoring: Some(Restoring {
                        buffer,
                        state: line.state.clone(),
                        normal: Vec::new(),
                        resync: None,
                    }),
                },
            );
            let history = self.history;
            let p = line.pane;
            // Lines that only wrapped come back whole (-J), so they wrap
            // again in the native pane and copy as one line. Not while the
            // alternate screen is on: its screen is then the last `height`
            // lines of this capture, which joining would miscount.
            let join = if line.state.alternate { "" } else { " -J" };
            self.send(
                &format!("capture-pane -p -e -q{join} -t %{p} -S -{history}"),
                Tag::Capture(p),
                out,
            );
            self.send(
                &format!("capture-pane -p -e -q -a -t %{p}"),
                Tag::CaptureAlternate(p),
                out,
            );
        }
        let panes = &self.panes;
        let tree = native_tree(&layout, &|p| panes.get(&p).map(|x| x.native))?;
        self.windows.insert(w, Window { layout });
        Some(tree)
    }

    /// A window that appeared later, or new panes in a known window.
    fn integrate(
        &mut self,
        ws: &mut Workspace,
        w: WindowId,
        lines: &[Vec<u8>],
        out: &mut Vec<Action>,
    ) {
        let lines: Vec<PaneLine> = lines
            .iter()
            .filter_map(|l| parse_pane_line(l))
            .filter(|l| l.window == w)
            .collect();
        if lines.is_empty() {
            return;
        }
        if let Some(a) = lines.iter().find_map(|l| l.agent.clone()) {
            self.mark_agent(w, a, out);
        }
        if let Some(wt) = lines.iter().find_map(|l| l.worktree.clone()) {
            self.worktrees.insert(w, wt);
        }
        let known = self.windows.contains_key(&w);
        if !known && self.windows.len() >= MAX_WINDOWS {
            return;
        }
        let old: Vec<Native> = self
            .panes
            .values()
            .filter(|p| p.window == w)
            .map(|p| p.native)
            .collect();
        // A new window looks like the pane it was split from.
        let placement = self.placements.remove(&w);
        let look = Look::default();
        let Some(tree) = self.add_window(ws, w, &lines, &look, out) else {
            return;
        };
        let focus = tree.panes().first().copied();
        if known {
            ws.replace_group(&old, tree);
        } else {
            let placed = match placement {
                Some(Placement::Split { next_to, dir }) => ws.graft(next_to, tree.clone(), dir),
                _ => false,
            };
            if !placed {
                let after = match placement {
                    Some(Placement::Tab { after })
                    | Some(Placement::Split { next_to: after, .. }) => ws.tab_of(after),
                    None => self.last_tab(ws),
                };
                let at = after.map_or(ws.tabs.len(), |i| i + 1);
                let f = focus.unwrap_or(0);
                ws.insert_tab(at, tree, f);
            }
            if placement.is_some()
                && let Some(f) = focus
            {
                ws.focus(f);
                out.push(Action::Focus { native: f });
            }
        }
        if let Some(p) = self.focus_next.take() {
            if let Some(n) = self.native_of(p) {
                ws.focus(n);
                out.push(Action::Focus { native: n });
            } else {
                self.focus_next = Some(p);
            }
        }
        out.push(Action::Structure);
    }

    /// The last tab holding a pane of this session.
    fn last_tab(&self, ws: &Workspace) -> Option<usize> {
        let natives = self.natives();
        ws.tabs
            .iter()
            .rposition(|t| natives.iter().any(|n| t.root.contains(*n)))
    }

    fn on_layout_change(
        &mut self,
        ws: &mut Workspace,
        w: WindowId,
        layout: &str,
        out: &mut Vec<Action>,
    ) {
        let Some(cell) = tl::parse(layout) else {
            return;
        };
        let Some(old) = self.windows.get(&w).map(|win| win.layout.clone()) else {
            if !self.discovering.contains(&w) {
                self.discover(w, out);
            }
            return;
        };
        let now = cell.panes();
        let gone: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(id, p)| p.window == w && !now.contains(id))
            .map(|(id, _)| *id)
            .collect();
        for p in &gone {
            if let Some(pane) = self.panes.remove(p) {
                self.remove_native(ws, pane.native, out);
            }
        }
        if now.iter().any(|p| !self.panes.contains_key(p)) {
            // New panes: their state comes with the window's pane list.
            self.discover(w, out);
            return;
        }
        for (p, cols, rows) in cell.sizes() {
            if let Some(native) = self.native_of(p) {
                out.push(Action::Resize { native, cols, rows });
            }
        }
        let reshaped = !old.same_shape(&cell);
        if let Some(win) = self.windows.get_mut(&w) {
            win.layout = cell.clone();
        }
        if reshaped {
            let panes = &self.panes;
            let members: Vec<Native> = panes
                .values()
                .filter(|p| p.window == w)
                .map(|p| p.native)
                .collect();
            if let Some(tree) = native_tree(&cell, &|p| panes.get(&p).map(|x| x.native)) {
                ws.replace_group(&members, tree);
            }
            out.push(Action::Structure);
        } else if !gone.is_empty() {
            out.push(Action::Structure);
        }
    }

    fn remove_window(&mut self, ws: &mut Workspace, w: WindowId, out: &mut Vec<Action>) {
        self.windows.remove(&w);
        self.agents.remove(&w);
        self.worktrees.remove(&w);
        self.sent_sizes.remove(&w);
        self.sent_layouts.remove(&w);
        let gone: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.window == w)
            .map(|(id, _)| *id)
            .collect();
        for p in gone {
            if let Some(pane) = self.panes.remove(&p) {
                self.remove_native(ws, pane.native, out);
            }
        }
    }

    /// Takes a native pane out; the gateway takes the place of the last.
    fn remove_native(&mut self, ws: &mut Workspace, native: Native, out: &mut Vec<Action>) {
        if !ws.remove_pane(native) && ws.tab_of(native).is_some() {
            ws.replace_group(&[native], Node::Leaf(self.gateway));
            ws.focus(self.gateway);
            out.push(Action::ShowGateway);
        }
        out.push(Action::Remove { native });
    }

    /// Control mode ended: the native panes go, the gateway comes back
    /// where the first of them was.
    fn teardown(&mut self, ws: &mut Workspace, out: &mut Vec<Action>) {
        // A Stop sharing still waiting for replies gets none now.
        self.unsharing = None;
        let mut natives = self.natives();
        natives.append(&mut self.stale);
        if ws.tab_of(self.gateway).is_none() {
            let at = ws
                .tabs
                .iter()
                .position(|t| natives.iter().any(|n| t.root.contains(*n)))
                .unwrap_or(ws.tabs.len());
            ws.insert_tab(at, Node::Leaf(self.gateway), self.gateway);
            ws.focus(self.gateway);
            out.push(Action::ShowGateway);
        }
        for native in natives {
            ws.remove_pane(native);
            out.push(Action::Remove { native });
        }
        self.agents_waiting.clear();
        self.panes.clear();
        self.windows.clear();
        self.orphans.clear();
        self.discovering.clear();
        self.agents.clear();
        self.worktrees.clear();
        self.placements.clear();
        self.session = None;
        self.clients = None;
        self.clients_asked = 0;
        self.restoring = false;
        self.joined = None;
        out.push(Action::Structure);
        // A share or join asked for while attached: type it at the shell
        // that is back now.
        if let Some(line) = self.after_detach.take() {
            out.push(Action::Write(line));
        }
    }

    /// Refuses a change while watching or joined to someone's session:
    /// `true` (and a notice) when it must not happen.
    fn refuse_joined(&mut self, out: &mut Vec<Action>) -> bool {
        if self.joined().is_none() {
            return false;
        }
        out.push(Action::Tell(
            "This is someone else's shared session: FeroSSH leaves its windows, panes and \
             layout to them."
                .into(),
        ));
        true
    }

    /// Typed or pasted bytes for a native pane.
    pub fn input(&mut self, native: Native, bytes: &[u8]) -> Vec<Action> {
        let mut out = Vec::new();
        if self.joined() == Some(false) {
            if !std::mem::replace(&mut self.told_read_only, true) {
                out.push(Action::Tell(
                    "You are watching this session read-only: what you type is not sent. \
                     Session > tmux > Detach tmux leaves it."
                        .into(),
                ));
            }
            return out;
        }
        if let Some(p) = self.pane_of(native) {
            // A paste goes through tmux's paste buffer, which brackets it
            // only if the program in the pane asked for that: tmux knows,
            // while the native terminal may not (after a reattach it
            // cannot learn it). The window brackets every paste for a tmux
            // pane so that it is told apart from typing here. What comes
            // before it (an SSH escape character held back) is typed.
            if let Some(start) = bytes
                .windows(PASTE_START.len())
                .position(|w| w == PASTE_START)
                && let Some((typed, rest)) = bytes.split_at_checked(start)
                && let Some(text) = rest
                    .strip_prefix(PASTE_START)
                    .and_then(|b| b.strip_suffix(PASTE_END))
            {
                let mut lines = command::send_keys(p, typed);
                lines.extend(command::paste(p, text));
                for line in lines {
                    self.send(&line, Tag::Ignore, &mut out);
                }
                return out;
            }
            for line in command::send_keys(p, bytes) {
                self.send(&line, Tag::Ignore, &mut out);
            }
        }
        out
    }

    /// Splits native pane `native`: a new window beside it in FeroSSH
    /// layout, a tmux split in tmux layout, in the pane's directory.
    pub fn split(&mut self, native: Native, dir: Dir) -> Vec<Action> {
        let mut out = Vec::new();
        if self.refuse_joined(&mut out) {
            return out;
        }
        let Some(p) = self.pane_of(native) else {
            return out;
        };
        let split = match self.mode {
            Mode::Ferossh => Split::Window(Placement::Split {
                next_to: native,
                dir,
            }),
            Mode::Tmux => Split::Pane {
                pane: p,
                flags: match dir {
                    Dir::Right => "-h",
                    Dir::Left => "-hb",
                    Dir::Down => "-v",
                    Dir::Up => "-vb",
                },
            },
        };
        self.open_here(Some(p), split, &mut out);
        out
    }

    /// Opens `split` in tmux pane `p`'s directory, which tmux is asked
    /// for first: tmux expands `#{pane_current_path}` in `-c` for the
    /// client's current pane, not the target. Without a pane, in the
    /// start directory.
    fn open_here(&mut self, p: Option<PaneId>, split: Split, out: &mut Vec<Action>) {
        match p {
            Some(p) => self.send(
                &format!("display-message -p -t %{p} '#{{pane_current_path}}'"),
                Tag::SplitHere(split),
                out,
            ),
            None => {
                let cwd = self.start_cwd();
                self.open_split(split, &cwd, out);
            }
        }
    }

    /// The `-c` option for new windows and panes: the start directory,
    /// or empty for tmux's own choice.
    fn start_cwd(&self) -> String {
        self.start_dir
            .as_deref()
            .map(same_dir::tmux_cwd)
            .unwrap_or_default()
    }

    /// Asks tmux which directory native pane `native` is in; the answer
    /// comes back as [`Action::StartDir`].
    pub fn ask_dir(&mut self, native: Native) -> Vec<Action> {
        let mut out = Vec::new();
        if let Some(p) = self.pane_of(native) {
            self.send(
                &format!("display-message -p -t %{p} '#{{pane_current_path}}'"),
                Tag::StartDirHere,
                &mut out,
            );
        }
        out
    }

    /// Asks tmux for native pane `native`'s directory, to make a coding
    /// agent's worktree from; the answer comes back as
    /// [`Action::WorktreeDir`].
    pub fn ask_worktree_dir(&mut self, native: Native) -> Vec<Action> {
        let mut out = Vec::new();
        if let Some(p) = self.pane_of(native) {
            self.send(
                &format!("display-message -p -t %{p} '#{{pane_current_path}}'"),
                Tag::WorktreeDirHere(native),
                &mut out,
            );
        }
        out
    }

    /// Closes the whole tmux window of coding agent pane `native` (the
    /// agent and any panes split off it), as Remove worktree does once its
    /// folder is gone. Nothing for another pane, or in a session joined.
    pub fn close_agent_window(&mut self, native: Native) -> Vec<Action> {
        let mut out = Vec::new();
        if self.agent_of(native).is_none() || self.refuse_joined(&mut out) {
            return out;
        }
        let Some(w) = self
            .pane_of(native)
            .and_then(|p| self.panes.get(&p))
            .map(|x| x.window)
        else {
            return out;
        };
        self.send(&format!("kill-window -t @{w}"), Tag::Ignore, &mut out);
        out
    }

    /// Asks tmux which directory native pane `native` is in, to show what
    /// changed there; the answer comes back as [`Action::DiffDir`].
    pub fn ask_diff_dir(&mut self, native: Native) -> Vec<Action> {
        let mut out = Vec::new();
        if let Some(p) = self.pane_of(native) {
            self.send(
                &format!("display-message -p -t %{p} '#{{pane_current_path}}'"),
                Tag::DiffDirHere(native),
                &mut out,
            );
        }
        out
    }

    /// Opens `split`; `cwd` is its `-c` option or empty.
    fn open_split(&mut self, split: Split, cwd: &str, out: &mut Vec<Action>) {
        match split {
            Split::Window(placement) => self.send(
                &format!("new-window -d{cwd} -P -F '#{{window_id}}'"),
                Tag::NewWindow(placement),
                out,
            ),
            Split::Agent(placement, n) => {
                let name = self
                    .agents_waiting
                    .get(&n)
                    .map(|(name, _, _)| name.clone())
                    .unwrap_or_default();
                self.send(
                    &format!(
                        "new-window -d{cwd} -n {} -P -F '#{{window_id}} #{{pane_id}}'",
                        command::quote(&name)
                    ),
                    Tag::AgentWindow(placement, n),
                    out,
                );
            }
            Split::Pane { pane, flags } => self.send(
                &format!("split-window {flags}{cwd} -t %{pane} -P -F '#{{pane_id}}'"),
                Tag::SplitPane,
                out,
            ),
        }
    }

    /// Whether [`Self::start_agent`] would refuse for the starts tmux has
    /// not answered yet.
    pub fn agents_full(&self) -> bool {
        self.agents_waiting.len() >= 16
    }

    /// Starts coding agent `command` in a new tmux window, a tab after
    /// `native`'s, in its directory: the window is named and marked with
    /// [`AGENT_OPTION`], and the command is typed at its shell, so the
    /// shell stays when the agent exits. tmux keeps it running whatever
    /// happens to the connection.
    /// `dir`, when given, is the directory to start in instead of
    /// `native`'s (an SSH shell outside tmux that asked for the agent).
    /// With `worktree`, the agent starts in that worktree's folder (and
    /// not in `dir`), and its window is marked with it.
    pub fn start_agent(
        &mut self,
        native: Native,
        command: &str,
        dir: Option<&str>,
        worktree: Option<&Worktree>,
    ) -> Vec<Action> {
        let cwd = worktree
            .map(|w| w.path.as_str())
            .or(dir)
            .map(same_dir::tmux_cwd);
        self.start_agent_with(native, command, cwd, worktree)
    }

    /// [`Self::start_agent`] in folder `rel` of the home folder of the
    /// user tmux runs as (`.`: the home folder), as a prompt showed
    /// `~/rel`.
    pub fn start_agent_in_home(&mut self, native: Native, command: &str, rel: &str) -> Vec<Action> {
        self.start_agent_with(native, command, Some(same_dir::tmux_home_cwd(rel)), None)
    }

    /// [`Self::start_agent`] with tmux `-c` option `cwd`, or in the
    /// folder of pane `native` when `None`.
    fn start_agent_with(
        &mut self,
        native: Native,
        command: &str,
        cwd: Option<String>,
        worktree: Option<&Worktree>,
    ) -> Vec<Action> {
        let mut out = Vec::new();
        if !self.active() || command.trim().is_empty() || self.refuse_joined(&mut out) {
            return out;
        }
        // A connection that never answers keeps only a few; the ones it
        // accepted still finish.
        if self.agents_full() {
            out.push(Action::Notice(
                "tmux has not answered the last agent starts yet; try again in a moment.".into(),
            ));
            return out;
        }
        self.agent_seq = self.agent_seq.wrapping_add(1);
        let n = self.agent_seq;
        self.agents_waiting.insert(
            n,
            (
                agent_name(command),
                command.trim().to_owned(),
                worktree.cloned(),
            ),
        );
        let split = Split::Agent(Placement::Tab { after: native }, n);
        match cwd {
            // The asking shell's directory, or tmux's default when it cannot
            // be given to tmux: never another pane's.
            Some(cwd) => {
                self.open_split(split, &cwd, &mut out);
            }
            None => {
                let p = self.pane_of(native);
                self.open_here(p, split, &mut out);
            }
        }
        out
    }

    /// Records that window `w` runs coding agent `name`. Panes of `w`
    /// made before that was known are told of in an
    /// [`Action::AgentMarked`], so they are watched as agent panes too.
    fn mark_agent(&mut self, w: WindowId, name: String, out: &mut Vec<Action>) {
        if self.agents.insert(w, name).is_some() {
            return;
        }
        let natives: Vec<Native> = self
            .panes
            .values()
            .filter(|p| p.window == w)
            .map(|p| p.native)
            .collect();
        if !natives.is_empty() {
            out.push(Action::AgentMarked(natives));
        }
    }

    /// The coding agent native pane `native` runs, by its name, if its
    /// tmux window is an agent's.
    pub fn agent_of(&self, native: Native) -> Option<&str> {
        let p = self.pane_of(native)?;
        let w = self.panes.get(&p)?.window;
        self.agents.get(&w).map(String::as_str)
    }

    /// The worktree coding agent pane `native` was started in, if it was
    /// started in one of its own.
    pub fn worktree_of(&self, native: Native) -> Option<&Worktree> {
        let p = self.pane_of(native)?;
        let w = self.panes.get(&p)?.window;
        self.agents.get(&w)?;
        self.worktrees.get(&w)
    }

    /// A new tmux window as a tab after `native`'s, in its directory.
    pub fn new_tab(&mut self, native: Native) -> Vec<Action> {
        let mut out = Vec::new();
        if self.refuse_joined(&mut out) {
            return out;
        }
        let p = self.pane_of(native);
        self.open_here(p, Split::Window(Placement::Tab { after: native }), &mut out);
        out
    }

    /// Closes the tmux pane (its window, when it is the only one there).
    /// The native pane goes when tmux says it went.
    pub fn close(&mut self, native: Native) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(p) = self.pane_of(native) else {
            return out;
        };
        // Someone else's windows stay: closing leaves their session.
        if self.joined().is_some() {
            return self.detach();
        }
        let window = self.panes.get(&p).map(|x| x.window);
        let alone =
            window.is_some_and(|w| self.panes.values().filter(|x| x.window == w).count() == 1);
        let line = match window {
            Some(w) if alone => format!("kill-window -t @{w}"),
            _ => format!("kill-pane -t %{p}"),
        };
        self.send(&line, Tag::Ignore, &mut out);
        out
    }

    /// The user named native pane `native`'s session (or cleared its
    /// name): its tmux window takes the name, so tmux's own status line and
    /// other clients show it. A cleared name gives the window back to
    /// tmux's automatic naming. The name is also saved with the window's
    /// look by [`Tmux::sync_saved`], which brings it back on attach.
    pub fn rename(&mut self, native: Native, name: Option<&str>) -> Vec<Action> {
        let mut out = Vec::new();
        if self.refuse_joined(&mut out) {
            return out;
        }
        let Some(w) = self
            .pane_of(native)
            .and_then(|p| self.panes.get(&p))
            .map(|p| p.window)
        else {
            return out;
        };
        let line = match name.map(command::quote).filter(|q| q != "''") {
            Some(q) => format!("rename-window -t @{w} {q}"),
            None => format!("set-option -w -t @{w} automatic-rename on"),
        };
        self.send(&line, Tag::Ignore, &mut out);
        out
    }

    /// Leaves tmux running and the gateway at its shell.
    /// Detaches every other client from the session, so that tmux sizes
    /// its windows to FeroSSH's panes again.
    ///
    /// Only clients of this session go (`detach-client -a` would detach
    /// every other session's clients too), and only while `session`, the
    /// one the user was asked about, is still the one attached.
    pub fn detach_others(&mut self, session: Option<SessionId>) -> Vec<Action> {
        let mut out = Vec::new();
        let Some((current, _)) = self.session else {
            return out;
        };
        if self.refuse_joined(&mut out) {
            return out;
        }
        if !self.client.is_control() || session.is_some_and(|s| s != current) {
            return out;
        }
        self.send(
            "display-message -p '#{client_name}'",
            Tag::OwnClient(current),
            &mut out,
        );
        out
    }

    pub fn detach(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        self.reattach = None;
        self.send("detach-client", Tag::Ignore, &mut out);
        out
    }

    /// Switches the session's layout mode and reads it again.
    pub fn set_mode(&mut self, mode: Mode) -> Vec<Action> {
        let mut out = Vec::new();
        if !self.client.is_control() || mode == self.mode || self.refuse_joined(&mut out) {
            return out;
        }
        if mode == Mode::Ferossh && !self.window_sizes() {
            out.push(Action::Notice(format!(
                "FeroSSH layout needs tmux {WINDOW_SIZES_VERSION} or later."
            )));
            return out;
        }
        self.send(
            &format!("set-option -q {} {}", saved::MODE_OPTION, mode.name()),
            Tag::Ignore,
            &mut out,
        );
        self.start_restore(&mut out);
        out
    }

    /// Tells tmux how big each window's native panes are.
    pub fn sync_sizes(&mut self, ws: &Workspace, cells: CellsOf<'_>) -> Vec<Action> {
        let mut out = Vec::new();
        // Someone else's session keeps its sizes and layout: this client
        // joined with ignore-size and shows the owner's sizes.
        if !self.client.is_control() || self.restoring || self.joined.is_some() {
            return out;
        }
        let mut wanted: Vec<(WindowId, (u16, u16), Option<String>)> = Vec::new();
        for (w, win) in &self.windows {
            let want = |p: PaneId| self.native_of(p).and_then(cells);
            let Some((shape, (cols, rows))) = shape_of(&win.layout, &want) else {
                continue;
            };
            let layout = if matches!(win.layout.kind, Kind::Pane(_)) {
                None
            } else {
                tl::place(&shape, cols, rows, 0, 0).map(|c| c.to_layout_string())
            };
            wanted.push((*w, (cols, rows), layout));
        }
        let focused = self
            .pane_of(ws.focused())
            .and_then(|p| self.panes.get(&p))
            .map(|p| p.window);
        for (w, size, layout) in wanted {
            let sent = if self.window_sizes() {
                if self.sent_sizes.get(&w) != Some(&size) {
                    self.sent_sizes.insert(w, size);
                    let line = format!("refresh-client -C @{w}:{}x{}", size.0, size.1);
                    self.send(&line, Tag::Ignore, &mut out);
                    true
                } else {
                    false
                }
            } else if focused == Some(w) && self.sent_client_size != Some(size) {
                self.sent_client_size = Some(size);
                let line = format!("refresh-client -C {}x{}", size.0, size.1);
                self.send(&line, Tag::Ignore, &mut out);
                true
            } else {
                false
            };
            if sent
                && !self.crowded_asked
                && let Some((session, _)) = self.session
            {
                // tmux resizes at once: did it give the window that size?
                // The session too, so a window linked into another
                // session counts this one's clients.
                let line = format!(
                    "display-message -p -t ${session}:@{w} {}",
                    command::quote(SIZE_FORMAT)
                );
                self.send(&line, Tag::SizeCheck(w, size), &mut out);
            }
            if let Some(layout) = layout {
                let current = self
                    .windows
                    .get(&w)
                    .map(|win| win.layout.to_layout_string())
                    .unwrap_or_default();
                let body = |s: &str| {
                    s.split_once(',')
                        .map(|(_, b)| b.to_owned())
                        .unwrap_or_default()
                };
                if body(&layout) != body(&current) && self.sent_layouts.get(&w) != Some(&layout) {
                    self.sent_layouts.insert(w, layout.clone());
                    let line = format!("select-layout -t @{w} {}", command::quote(&layout));
                    self.send(&line, Tag::Ignore, &mut out);
                }
            }
        }
        out
    }

    /// Writes the layout and looks to the session when they changed.
    pub fn sync_saved(&mut self, ws: &Workspace, look_of: LookOf<'_>) -> Vec<Action> {
        let mut out = Vec::new();
        if !self.client.is_control()
            || self.restoring
            || self.panes.is_empty()
            || self.joined.is_some()
        {
            return out;
        }
        let window_of = |n: Native| {
            self.pane_of(n)
                .and_then(|p| self.panes.get(&p))
                .map(|p| p.window)
        };
        let mut saved = Saved::default();
        if self.mode == Mode::Ferossh {
            for (i, tab) in ws.tabs.iter().enumerate() {
                let Some(root) = collapse(&tab.root, &window_of) else {
                    continue;
                };
                if i == ws.active {
                    saved.active = saved.tabs.len();
                }
                let windows = root.windows();
                let focus = window_of(tab.focus)
                    .filter(|w| windows.contains(w))
                    .or_else(|| windows.first().copied())
                    .unwrap_or(0);
                saved.tabs.push(saved::Tab { root, focus });
            }
        }
        if self.mode == Mode::Ferossh {
            let shown: BTreeSet<WindowId> =
                saved.tabs.iter().flat_map(|t| t.root.windows()).collect();
            for n in ws.hidden() {
                let Some(w) = window_of(n) else {
                    continue;
                };
                if !shown.contains(&w) {
                    if !saved.hidden.contains(&w) {
                        saved.hidden.push(w);
                    }
                } else if let Some(p) = self.pane_of(n) {
                    // Part of a window that shows: the pane alone.
                    saved.hidden_panes.push(p);
                }
            }
        }
        for p in self.panes.values() {
            saved
                .looks
                .entry(p.window)
                .or_insert_with(|| look_of(p.native));
        }
        let text = saved.to_text();
        if self.saved_text.as_ref() != Some(&text) {
            let line = format!("set-option -q {} {}", saved::OPTION, command::quote(&text));
            self.saved_text = Some(text);
            self.send(&line, Tag::Ignore, &mut out);
        }
        out
    }
}

/// The saved tree of a native tab: its tmux panes' windows, a window's
/// panes standing as one. `None` if the tab shows none of them.
fn collapse(node: &Node, window_of: &dyn Fn(Native) -> Option<WindowId>) -> Option<saved::Node> {
    match node {
        Node::Leaf(id) => window_of(*id).map(saved::Node::Window),
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => match (collapse(first, window_of), collapse(second, window_of)) {
            (Some(saved::Node::Window(a)), Some(saved::Node::Window(b))) if a == b => {
                Some(saved::Node::Window(a))
            }
            (Some(a), Some(b)) => Some(saved::Node::Split {
                row: *axis == Axis::Row,
                ratio: (ratio * 1000.0).round().clamp(100.0, 900.0) as u16,
                first: Box::new(a),
                second: Box::new(b),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

/// A window's layout with the sizes its native panes want, and the size
/// of the whole: rows add widths and take the smallest height, columns
/// the other way round, so that no pane gets more cells than it shows.
fn shape_of(
    cell: &Cell,
    want: &dyn Fn(PaneId) -> Option<(u16, u16)>,
) -> Option<(Shape, (u16, u16))> {
    match &cell.kind {
        Kind::Pane(p) => {
            let (c, r) = want(*p)?;
            Some((
                Shape::Pane(*p),
                (c.clamp(1, tl::MAX_SIZE), r.clamp(1, tl::MAX_SIZE)),
            ))
        }
        Kind::Row(cells) | Kind::Column(cells) => {
            let row = matches!(cell.kind, Kind::Row(_));
            let mut children = Vec::new();
            let (mut along, mut across) = (0u16, u16::MAX);
            for c in cells {
                let (shape, (w, h)) = shape_of(c, want)?;
                let (len, other) = if row { (w, h) } else { (h, w) };
                if !children.is_empty() {
                    along = along.saturating_add(1);
                }
                along = along.saturating_add(len);
                across = across.min(other);
                children.push((len, shape));
            }
            let size = if row {
                (along, across)
            } else {
                (across, along)
            };
            let shape = if row {
                Shape::Row(children)
            } else {
                Shape::Column(children)
            };
            Some((shape, (size.0.min(tl::MAX_SIZE), size.1.min(tl::MAX_SIZE))))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    const START: &[u8] = b"\x1bP1000p%begin 1 1 0\n%end 1 1 0\n%session-changed $0 main\n";

    /// A pane state line for `%pane` in `@window` of `w` x `h`.
    fn pane_line(pane: u32, window: u32, w: u16, h: u16, layout: &str) -> String {
        format!(
            "%{pane} @{window} {w} {h} 2 3 0 1 0 0 0 0 0 0 0 0 0 1 0 {} {layout}",
            h - 1
        )
    }

    /// The reply block for command number `n`.
    fn reply(n: u32, lines: &[String]) -> Vec<u8> {
        let mut out = format!("%begin 9 {n} 1\n");
        for l in lines {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&format!("%end 9 {n} 1\n"));
        out.into_bytes()
    }

    /// The reply to `refresh-client -f pause-after=...`, sent once tmux
    /// says its version (3.2 or later): fed on its own, after the
    /// replies before it are read.
    const PAUSE_ACK: &[u8] = b"%begin 9 90 1\n%end 9 90 1\n";

    fn writes(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Write(b) => Some(String::from_utf8_lossy(b).trim_end().to_owned()),
                _ => None,
            })
            .collect()
    }

    /// A session attached after `prep` (a join or share typed first);
    /// tmux says it is `version` with one pane.
    fn attached_after(
        version: &str,
        prep: impl FnOnce(&mut Tmux) -> Vec<Action>,
    ) -> (Tmux, Workspace, Vec<Action>, Vec<Action>) {
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        let typed = prep(&mut t);
        t.feed(&mut ws, START);
        let mut input = reply(1, &[version.into()]);
        input.extend(reply(2, &[]));
        input.extend(reply(3, &[]));
        input.extend(reply(4, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]));
        let mut actions = t.feed(&mut ws, &input);
        if writes(&actions)
            .iter()
            .any(|w| w.starts_with("refresh-client -f"))
        {
            actions.extend(t.feed(&mut ws, PAUSE_ACK));
        }
        (t, ws, typed, actions)
    }

    fn tells(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Tell(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn watching_changes_nothing_of_the_owners() {
        let (mut t, ws, typed, a) = attached_after("3.4", |t| t.join(None, Some("pair"), false));
        assert_eq!(
            writes(&typed),
            ["tmux -CC attach-session -E -f read-only,ignore-size -t =pair"]
        );
        assert_eq!(t.joined(), Some(false));
        // No mode option written into their session; told once.
        assert!(
            !writes(&a).iter().any(|w| w.contains("set-option")),
            "{a:?}"
        );
        assert_eq!(tells(&a).len(), 1);
        let n = ws.focused();
        let a = t.input(n, b"ls\r");
        assert!(writes(&a).is_empty());
        assert_eq!(tells(&a).len(), 1);
        assert!(tells(&t.input(n, b"x")).is_empty());
        for a in [
            t.split(n, Dir::Right),
            t.new_tab(n),
            t.rename(n, Some("x")),
            t.set_mode(Mode::Tmux),
            t.detach_others(None),
            t.switch_session(None),
            t.start_agent(n, "claude", None, None),
        ] {
            assert!(writes(&a).is_empty(), "{a:?}");
        }
        // Switching to another of their sessions brings no environment.
        assert_eq!(
            writes(&t.switch_session(Some("other"))),
            ["switch-client -E -t '=other'"]
        );
        // Closing leaves, never kills their window.
        assert_eq!(writes(&t.close(n)), ["detach-client"]);
        assert!(t.unshare().is_empty());
    }

    #[test]
    fn joining_to_type_sends_keys_but_keeps_their_layout() {
        let (mut t, ws, typed, _) = attached_after("3.4", |t| t.join(None, None, true));
        assert_eq!(
            writes(&typed),
            ["tmux -CC attach-session -E -f ignore-size"]
        );
        assert_eq!(t.joined(), Some(true));
        let n = ws.focused();
        assert_eq!(writes(&t.input(n, b"a")), ["send-keys -t %0 -H 61"]);
        assert!(writes(&t.split(n, Dir::Down)).is_empty());
        assert_eq!(writes(&t.close(n)), ["detach-client"]);
    }

    #[test]
    fn a_lost_join_leaves_the_login_at_the_shell() {
        let (mut t, mut ws, _, _) = attached_after("3.4", |t| t.join(None, Some("pair"), false));
        let _ = t.connection_lost(&mut ws);
        assert_eq!(t.reattach, None);
        assert!(t.take_stay_at_shell());
        // One's own session lost: attached again, nothing withheld.
        let (mut t, mut ws, _) =
            attached(None, None, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]);
        let _ = t.connection_lost(&mut ws);
        assert!(t.reattach.is_some());
        assert!(!t.take_stay_at_shell());
    }

    #[test]
    fn a_plain_attach_after_a_join_is_ones_own_again() {
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        t.join(None, None, false);
        t.attach("main", false);
        t.feed(&mut ws, START);
        assert_eq!(t.joined(), None);
    }

    fn share(accounts: &[&str], can_type: bool) -> Share {
        Share {
            accounts: accounts.iter().map(|a| (*a).to_owned()).collect(),
            can_type,
        }
    }

    /// Shares as `bob` on tmux `version`; returns what was written and
    /// told once tmux gave `socket`.
    fn shared(version: &str, can_type: bool, socket: &str) -> (Vec<String>, Vec<String>) {
        let (mut t, mut ws, typed, a) =
            attached_after(version, |t| t.share("pair", share(&["bob"], can_type)));
        assert_eq!(writes(&typed), [command::share_line("pair").trim_end()]);
        assert!(writes(&a).contains(&"display-message -p '#{socket_path}'".to_owned()));
        let a = t.feed(&mut ws, &reply(5, &[socket.into()]));
        (writes(&a), tells(&a))
    }

    #[test]
    fn sharing_watch_only_needs_a_tmux_that_enforces_it() {
        let socket = "/tmp/ferossh-share-alice/default";
        let (w, told) = shared("3.4", false, socket);
        assert!(w.is_empty(), "{w:?}");
        assert!(told[0].contains("stays private"), "{told:?}");
        let (w, _) = shared("3.9", false, socket);
        assert_eq!(
            w,
            [
                "server-access -r 'bob'",
                "run-shell 'chmod 666 /tmp/ferossh-share-alice/default'",
            ]
        );
    }

    #[test]
    fn sharing_to_type_is_offered_from_access_lists_on() {
        let socket = "/tmp/ferossh-share-alice/default";
        let (w, told) = shared("3.4", true, socket);
        assert_eq!(w[0], "server-access -w 'bob'");
        assert!(told[0].contains("run commands as you"), "{told:?}");
        // No access list: nothing opens.
        let (w, told) = shared("3.2", true, socket);
        assert!(w.is_empty());
        assert!(told[0].contains("cannot limit"));
        // Not the shared server (the line failed and tmux was started some
        // other way): nothing opens.
        let (w, _) = shared("3.4", true, "/tmp/tmux-1000/default");
        assert!(w.is_empty());
    }

    #[test]
    fn a_development_build_is_not_trusted_with_watch_only() {
        let (w, told) = shared("next-3.9", false, "/tmp/ferossh-share-alice/default");
        assert!(w.is_empty(), "{w:?}");
        assert!(told[0].contains("development build"), "{told:?}");
        let (w, _) = shared("next-3.9", true, "/tmp/ferossh-share-alice/default");
        assert_eq!(w[0], "server-access -w 'bob'");
    }

    #[test]
    fn a_shared_session_comes_back_on_the_shared_server() {
        let (mut t, mut ws, _, _) =
            attached_after("3.4", |t| t.share("pair", share(&["bob"], true)));
        t.feed(
            &mut ws,
            &reply(5, &["/tmp/ferossh-share-alice/default".into()]),
        );
        assert!(t.on_shared_server());
        let _ = t.connection_lost(&mut ws);
        let name = t.reattach.clone().unwrap();
        // Typed again on the shared server, granting nothing anew.
        assert_eq!(
            t.attach(&name, false),
            command::share_line("main").into_bytes()
        );
        t.feed(&mut ws, START);
        assert!(t.on_shared_server());
        let a = t.feed(&mut ws, &reply(1, &["3.4".into()]));
        assert_eq!(writes(&a), ["refresh-client -f pause-after=10"], "{a:?}");
        // A later plain attach is the private server again.
        let _ = t.connection_lost(&mut ws);
        assert_eq!(
            t.attach("other", false),
            command::attach_line("other").into_bytes()
        );
    }

    #[test]
    fn shared_reattach_survives_a_new_watcher_but_not_an_odd_name() {
        let (mut t, mut ws, _, _) =
            attached_after("3.4", |t| t.share("pair", share(&["bob"], true)));
        t.feed(
            &mut ws,
            &reply(5, &["/tmp/ferossh-share-alice/default".into()]),
        );
        let _ = t.connection_lost(&mut ws);
        let name = t.reattach.take().unwrap();
        assert!(t.take_reattach_shared(&name));
        // The window makes a new watcher for the next login.
        let (_, gateway) = Workspace::new();
        let mut fresh = Tmux::new(gateway, Mode::Ferossh, 100);
        fresh.reattach_on_shared(&name);
        assert_eq!(
            fresh.attach(&name, false),
            command::share_line(&name).into_bytes()
        );

        // A session switched to on the shared server, named so that the
        // share line would change it: not attached again anywhere.
        let (mut t, mut ws, _, _) =
            attached_after("3.4", |t| t.share("pair", share(&["bob"], true)));
        t.feed(
            &mut ws,
            &reply(5, &["/tmp/ferossh-share-alice/default".into()]),
        );
        t.feed(&mut ws, b"%session-changed $3 x y\n");
        let _ = t.connection_lost(&mut ws);
        assert_eq!(t.reattach, None);
        // Nor does the profile's session take its place: the login stays
        // at the shell, once.
        assert!(t.take_stay_at_shell());
        assert!(!t.take_stay_at_shell());
    }

    #[test]
    fn stop_sharing_on_the_private_server_says_nobody_was_taken_off() {
        let (mut t, mut ws, a) =
            attached(None, None, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]);
        let mut n = 4;
        for _ in writes(&a) {
            n += 1;
            t.feed(&mut ws, &reply(n, &[]));
        }
        t.unshare();
        let mut input = reply(n + 1, &["/tmp/tmux-1000/default".into()]);
        input.extend(reply(n + 2, &["/dev/pts/3 0".into()]));
        let a = t.feed(&mut ws, &input);
        assert!(writes(&a).is_empty(), "{a:?}");
        assert!(tells(&a)[0].contains("private tmux"), "{a:?}");
    }

    #[test]
    fn share_while_attached_detaches_then_types() {
        let (mut t, mut ws, _) =
            attached(None, None, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]);
        let a = t.share("pair", share(&["bob"], true));
        assert_eq!(writes(&a), ["detach-client"]);
        // A late reply of the session being left cannot take the share.
        assert!(t.share_pending.is_none());
        let a = t.feed(&mut ws, b"%exit detached\n\x1b\\[detached]\r\n");
        assert_eq!(writes(&a), [command::share_line("pair").trim_end()]);
        assert!(t.reattach.is_none());
        // The new control session is the one that opens it.
        t.feed(&mut ws, START);
        assert_eq!(t.share_pending, Some(share(&["bob"], true)));
    }

    #[test]
    fn unsharing_takes_other_accounts_off_and_detaches_watchers() {
        let (mut t, mut ws, a) =
            attached(None, None, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]);
        // The restore's own commands are answered first.
        let mut n = 4;
        for _ in writes(&a) {
            n += 1;
            t.feed(&mut ws, &reply(n, &[]));
        }
        let a = t.unshare();
        assert_eq!(
            writes(&a),
            [
                "display-message -p '#{socket_path}'",
                "list-clients -t $0 -F '#{client_name} #{client_readonly}'"
            ]
        );
        let mut input = reply(n + 1, &["/tmp/ferossh-share-alice/default".into()]);
        input.extend(reply(n + 2, &["/dev/pts/3 0".into(), "client-9 1".into()]));
        let a = t.feed(&mut ws, &input);
        assert_eq!(
            writes(&a),
            [
                "run-shell 'chmod 600 /tmp/ferossh-share-alice/default'",
                "server-access -l",
                "detach-client -t 'client-9'",
            ]
        );
        // Not told yet: nobody has been taken off.
        assert!(tells(&a).is_empty(), "{a:?}");
        let mut input = reply(n + 3, &[]);
        input.extend(reply(
            n + 4,
            &["alice (W)".into(), "bob (R)".into(), "root (W)".into()],
        ));
        let a = t.feed(&mut ws, &input);
        assert_eq!(writes(&a), ["server-access -d 'bob'", "server-access -l"]);
        assert!(tells(&a).is_empty(), "{a:?}");
        // The check still lists bob: the owner is told so.
        let mut input = reply(n + 5, &[]);
        input.extend(reply(n + 6, &[]));
        input.extend(reply(n + 7, &["alice (W)".into(), "bob (R)".into()]));
        let a = t.feed(&mut ws, &input);
        let told = tells(&a);
        assert!(
            told[0].contains("NOT fully stopped") && told[0].contains("bob"),
            "{a:?}"
        );
        assert!(told[0].contains("1 read-only watcher"), "{a:?}");

        // Again, and this time tmux takes bob off.
        let a = t.unshare();
        assert_eq!(writes(&a).len(), 2);
        let mut input = reply(n + 8, &["/tmp/ferossh-share-alice/default".into()]);
        input.extend(reply(n + 9, &["/dev/pts/3 0".into()]));
        t.feed(&mut ws, &input);
        let mut input = reply(n + 10, &[]);
        input.extend(reply(n + 11, &["alice (W)".into(), "bob (R)".into()]));
        t.feed(&mut ws, &input);
        let mut input = reply(n + 12, &[]);
        input.extend(reply(n + 13, &["alice (W)".into()]));
        let a = t.feed(&mut ws, &input);
        let told = tells(&a);
        assert_eq!(told.len(), 1, "{a:?}");
        assert!(told[0].starts_with("Stopped sharing"), "{a:?}");
        assert!(told[0].contains("Nobody was watching"), "{a:?}");
    }

    #[test]
    fn stop_sharing_runs_once_and_takes_off_every_listed_entry() {
        let (mut t, mut ws, a) =
            attached(None, None, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]);
        let mut n = 4;
        for _ in writes(&a) {
            n += 1;
            t.feed(&mut ws, &reply(n, &[]));
        }
        assert_eq!(writes(&t.unshare()).len(), 2);
        // A second Stop sharing while the first waits sends nothing.
        assert!(t.unshare().is_empty());
        let mut input = reply(n + 1, &["/tmp/ferossh-share-alice/default".into()]);
        input.extend(reply(n + 2, &["client-9 1".into()]));
        t.feed(&mut ws, &input);
        // The detach fails (the watcher left), and tmux lists a name
        // FeroSSH would not take typed in: it is still taken off.
        let mut input = reply(n + 3, &[]);
        input.extend(reply(
            n + 4,
            &[
                "alice (W)".into(),
                "dom\\carol (R)".into(),
                "alice helper (W)".into(),
            ],
        ));
        input.extend(format!("%begin 9 {0} 1\ngone\n%error 9 {0} 1\n", n + 5).into_bytes());
        let a = t.feed(&mut ws, &input);
        assert_eq!(
            writes(&a),
            [
                "server-access -d 'dom\\carol'",
                "server-access -d 'alice helper'",
                "server-access -l"
            ]
        );
        let mut input = reply(n + 6, &[]);
        input.extend(reply(n + 7, &[]));
        input.extend(reply(n + 8, &["alice (W)".into()]));
        let a = t.feed(&mut ws, &input);
        let told = tells(&a);
        assert!(told[0].starts_with("Stopped sharing"), "{told:?}");
        assert!(told[0].contains("did not detach 1"), "{told:?}");
        // Done: Stop sharing works again.
        assert_eq!(writes(&t.unshare()).len(), 2);
    }

    #[test]
    fn stop_sharing_does_not_pass_by_a_line_it_cannot_read() {
        let (mut t, mut ws, a) =
            attached(None, None, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]);
        let mut n = 4;
        for _ in writes(&a) {
            n += 1;
            t.feed(&mut ws, &reply(n, &[]));
        }
        t.unshare();
        let mut input = reply(n + 1, &["/tmp/ferossh-share-alice/default".into()]);
        input.extend(reply(n + 2, &[]));
        t.feed(&mut ws, &input);
        // A bare "alice" (no mode) is not taken for the owner's entry.
        let mut input = reply(n + 3, &[]);
        input.extend(reply(n + 4, &["alice (W)".into()]));
        let a = t.feed(&mut ws, &input);
        assert_eq!(writes(&a), ["server-access -l"]);
        let a = t.feed(
            &mut ws,
            &reply(n + 5, &["alice (W)".into(), "alice".into()]),
        );
        let told = tells(&a);
        assert!(told[0].contains("NOT fully stopped"), "{told:?}");
    }

    #[test]
    fn stop_sharing_says_so_when_tmux_does_not_name_its_server() {
        let (mut t, mut ws, a) =
            attached(None, None, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]);
        let mut n = 4;
        for _ in writes(&a) {
            n += 1;
            t.feed(&mut ws, &reply(n, &[]));
        }
        t.unshare();
        let mut input = format!("%begin 9 {} 1\nno\n%error 9 {} 1\n", n + 1, n + 1).into_bytes();
        input.extend(reply(n + 2, &[]));
        let a = t.feed(&mut ws, &input);
        assert!(writes(&a).is_empty(), "{a:?}");
        assert!(tells(&a)[0].contains("could not tell"), "{a:?}");
    }

    /// Attaches to a session with the given panes and saved layout; returns
    /// the controller, the workspace and the native id of each tmux pane.
    fn attached(
        mode: Option<&str>,
        layout: Option<&str>,
        panes: &[String],
    ) -> (Tmux, Workspace, Vec<Action>) {
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        let a = t.feed(&mut ws, START);
        assert_eq!(writes(&a).len(), 4);
        let mut input = reply(1, &["3.4".into()]);
        input.extend(reply(
            2,
            &mode.map(|m| vec![m.to_owned()]).unwrap_or_default(),
        ));
        input.extend(reply(
            3,
            &layout.map(|l| vec![l.to_owned()]).unwrap_or_default(),
        ));
        input.extend(reply(4, panes));
        let mut actions = t.feed(&mut ws, &input);
        actions.extend(t.feed(&mut ws, PAUSE_ACK));
        (t, ws, actions)
    }

    #[test]
    fn reads_pane_lines() {
        let l = parse_pane_line(pane_line(3, 1, 80, 24, "b25f,80x24,0,0,3").as_bytes()).unwrap();
        assert_eq!((l.pane, l.window), (3, 1));
        assert_eq!((l.state.width, l.state.height), (80, 24));
        assert_eq!((l.state.cursor_x, l.state.cursor_y), (2, 3));
        assert!(l.state.cursor && l.state.wrap && !l.state.alternate);
        assert!(parse_pane_line(b"%1 @1 80 24").is_none());
        assert!(parse_pane_line(b"x @1 80 24 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 l").is_none());
    }

    #[test]
    fn restores_screens_and_modes() {
        let state = PaneState {
            width: 80,
            height: 3,
            cursor_x: 4,
            cursor_y: 1,
            alternate: true,
            cursor: false,
            keypad_cursor: true,
            mouse_sgr: true,
            wrap: true,
            region: (0, 2),
            ..PaneState::default()
        };
        // With vim open, plain capture-pane is the history and then vim's
        // screen; -a is the shell's screen, kept aside meanwhile.
        let lines = |l: &[&str]| l.iter().map(|x| x.as_bytes().to_vec()).collect::<Vec<_>>();
        let current = lines(&["old", "vim 1", "vim 2", "~"]);
        let saved = lines(&["$ ls", "$ vim x", "", ""]);
        let text = String::from_utf8(restore_bytes(&state, &current, &saved)).unwrap();
        assert_eq!(
            text,
            "\x1b[0m\x1b[H\x1b[2Jold\r\n$ ls\r\n$ vim x\x1b[0m\r\n\
             \x1b[?1049h\x1b[H\x1b[2Jvim 1\r\nvim 2\r\n~\x1b[0m\
             \x1b[?1h\x1b[?1006h\x1b[?25l\x1b[2;5H"
        );
        // Without the alternate screen, -a is empty and the capture is all.
        let normal = PaneState {
            alternate: false,
            ..state
        };
        let text = String::from_utf8(restore_bytes(&normal, &current, &[])).unwrap();
        assert!(text.starts_with("\x1b[0m\x1b[H\x1b[2Jold\r\nvim 1\r\nvim 2\r\n~\x1b[0m\x1b[?1h"));
    }

    #[test]
    fn restores_only_the_mouse_modes_that_were_on() {
        // htop's modes as tmux reports them: 1000 and 1006 on, and
        // `mouse_any_flag` (any mode) 1, which must not become 1003.
        let line = "%1 @1 80 24 0 0 1 1 0 0 1 0 0 1 0 0 0 1 0 23 l";
        let state = parse_pane_line(line.as_bytes()).unwrap().state;
        assert!(state.mouse_standard && state.mouse_sgr && !state.mouse_all);
        let text = String::from_utf8(restore_bytes(&state, &[], &[])).unwrap();
        assert!(text.contains("\x1b[?1000h\x1b[?1006h"), "{text:?}");
        assert!(
            !text.contains("?1003h") && !text.contains("?1002h"),
            "{text:?}"
        );
    }

    #[test]
    fn origin_mode_moves_within_the_region() {
        let state = PaneState {
            width: 80,
            height: 24,
            cursor_x: 0,
            cursor_y: 7,
            cursor: true,
            wrap: true,
            origin: true,
            region: (5, 20),
            ..PaneState::default()
        };
        let text = String::from_utf8(restore_bytes(&state, &[], &[])).unwrap();
        assert!(text.ends_with("\x1b[6;21r\x1b[?6h\x1b[3;1H"), "{text:?}");
    }

    #[test]
    fn reads_the_prefix_keys_when_asked() {
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        // Off: nothing is asked.
        assert!(t.want_keys(false).is_empty());
        assert!(t.want_keys(true).is_empty(), "not attached yet");
        let a = t.feed(&mut ws, START);
        let w = writes(&a);
        assert_eq!(w.len(), 5);
        assert_eq!(
            w[4],
            "display-message -p '#{prefix} #{prefix2} #{base-index} #{repeat-time}'"
        );
        let mut input = reply(1, &["3.4".into()]);
        input.extend(reply(2, &[]));
        input.extend(reply(3, &[]));
        input.extend(reply(4, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]));
        input.extend(reply(5, &["C-a None 1 500".into()]));
        t.feed(&mut ws, &input);
        assert_eq!(t.keys(), Some("C-a None 1 500"));
        // Turned on while attached: asked at once.
        let w = writes(&t.want_keys(true));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn attach_builds_tabs_and_hides_the_gateway() {
        let panes = [
            pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0"),
            pane_line(1, 1, 80, 24, "b25f,80x24,0,0,1"),
            pane_line(2, 2, 80, 24, "x,80x24,0,0{40x24,0,0,2,39x24,41,0,3}"),
            pane_line(3, 2, 80, 24, "x,80x24,0,0{40x24,0,0,2,39x24,41,0,3}"),
        ];
        let saved = "ferossh1 active=0 tab=r300(@1,@0)/@0 look=@1,Consolas,14,Dracula";
        let (t, ws, actions) = attached(None, Some(saved), &panes);
        // The saved tab, then window 2 (two tmux panes) as a tab of its own.
        assert_eq!(ws.tabs.len(), 2);
        let n = |p| t.native_of(p).unwrap();
        assert_eq!(ws.tabs[0].root.panes(), [n(1), n(0)]);
        assert!(matches!(ws.tabs[0].root, Node::Split { ratio, .. } if (ratio - 0.3).abs() < 0.01));
        assert_eq!(ws.tabs[1].root.panes(), [n(2), n(3)]);
        assert_eq!(ws.focused(), n(0));
        assert!(ws.tab_of(t.gateway).is_none());
        let created: Vec<(PaneId, Look)> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Create { pane, look, .. } => Some((*pane, look.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(created.len(), 4);
        assert_eq!(created[0].0, 1);
        assert_eq!(created[0].1.theme.as_deref(), Some("Dracula"));
        let w = writes(&actions);
        assert!(w.contains(&"set-option -q @ferossh-mode ferossh".to_owned()));
        assert!(w.contains(&"capture-pane -p -e -q -J -t %3 -S -100".to_owned()));
        assert_eq!(t.mode(), Mode::Ferossh);
    }

    #[test]
    fn hidden_windows_stay_hidden_and_are_saved() {
        let panes = [
            pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0"),
            pane_line(1, 1, 80, 24, "b25f,80x24,0,0,1"),
            pane_line(2, 2, 80, 24, "b25f,80x24,0,0,2"),
        ];
        let saved = "ferossh1 active=0 tab=r500(@0,@1)/@0 hidden=@2";
        let (mut t, mut ws, _) = attached(None, Some(saved), &panes);
        let [n0, n1, n2] = [0, 1, 2].map(|p| t.native_of(p).unwrap());
        assert_eq!(ws.all_panes(), [n0, n1]);
        assert_eq!(ws.hidden(), [n2]);
        let look = |_| Look::default();
        let w = writes(&t.sync_saved(&ws, &look));
        assert!(w[0].contains("tab=r500(@0,@1)/@0 hidden=@2 look="), "{w:?}");
        // Hiding another and showing the first saves that.
        assert!(ws.hide(n1));
        assert!(ws.show(n2));
        let w = writes(&t.sync_saved(&ws, &look));
        assert!(w[0].contains("hidden=@1 look="), "{w:?}");
        assert!(!w[0].contains("hidden=@2"), "{w:?}");
        // A hidden pane's window closing forgets it.
        let a = t.feed(&mut ws, b"%window-close @1\n");
        assert!(a.contains(&Action::Remove { native: n1 }));
        assert!(ws.hidden().is_empty());
    }

    #[test]
    fn a_hidden_pane_of_a_window_that_shows_stays_hidden() {
        let layout = "x,80x24,0,0{40x24,0,0,2,39x24,41,0,3}";
        let panes = [
            pane_line(2, 2, 80, 24, layout),
            pane_line(3, 2, 80, 24, layout),
        ];
        let (t, mut ws, _) = attached(None, None, &panes);
        let [n2, n3] = [2, 3].map(|p| t.native_of(p).unwrap());
        assert!(ws.hide(n3));
        let mut t = t;
        let w = writes(&t.sync_saved(&ws, &|_| Look::default()));
        assert!(w[0].contains("tab=@2/@2 hidden=%3 look="), "{w:?}");
        // Attaching again with that layout hides the pane again.
        let saved = "ferossh1 active=0 tab=@2/@2 hidden=%3";
        let (t, ws, _) = attached(None, Some(saved), &panes);
        let [m2, m3] = [2, 3].map(|p| t.native_of(p).unwrap());
        assert_eq!(ws.all_panes(), [m2]);
        assert_eq!(ws.hidden(), [m3]);
        // The window's first pane hidden: the focus goes to the other.
        let saved = "ferossh1 active=0 tab=@2/@2 hidden=%2";
        let (t, ws, a) = attached(None, Some(saved), &panes);
        let [m2, m3] = [2, 3].map(|p| t.native_of(p).unwrap());
        assert_eq!(ws.all_panes(), [m3]);
        assert_eq!(ws.hidden(), [m2]);
        assert_eq!(ws.focused(), m3);
        assert!(a.contains(&Action::Focus { native: m3 }));
        // In tmux's own layout nothing is hidden.
        let (_, ws, _) = attached(Some("tmux"), Some(saved), &panes);
        assert!(ws.hidden().is_empty());
        let _ = n2;
    }

    #[test]
    fn hidden_windows_show_when_nothing_else_would() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (t, ws, _) = attached(None, Some("ferossh1 active=0 hidden=@0"), &panes);
        assert_eq!(ws.all_panes(), [t.native_of(0).unwrap()]);
        assert!(ws.hidden().is_empty());
    }

    #[test]
    fn flow_control_is_on_from_tmux_3_2_and_a_pause_redraws_the_screen() {
        let panes = [pane_line(5, 1, 80, 24, "b25f,80x24,0,0,5")];
        let (mut t, mut ws, a) = attached(Some("tmux"), None, &panes);
        assert!(writes(&a).contains(&"refresh-client -f pause-after=10".to_owned()));
        let native = t.native_of(5).unwrap();
        // The restore's two captures.
        t.feed(&mut ws, &reply(5, &[]));
        t.feed(&mut ws, &reply(6, &[]));
        // Output comes as %extended-output with the flag on.
        let a = t.feed(&mut ws, b"%extended-output %5 20 : hi\n");
        assert!(a.contains(&Action::Feed {
            native,
            bytes: b"hi".to_vec()
        }));
        let a = t.feed(&mut ws, b"%pause %5\n");
        assert_eq!(
            writes(&a),
            [
                "refresh-client -A '%5:continue'".to_owned(),
                format!(
                    "capture-pane -p -e -q -t %5 ; capture-pane -p -e -q -a -t %5 ; \
                     list-panes -t %5 -F {}",
                    command::quote(PANE_FORMAT)
                ),
            ]
        );
        // A second pause before the redraw only lets the pane go again.
        let a = t.feed(&mut ws, b"%pause %5\n");
        assert_eq!(writes(&a), ["refresh-client -A '%5:continue'"]);
        t.feed(&mut ws, b"%begin 9 7 1\n%end 9 7 1\n");
        // Output before the capture is in it; after it, kept for later.
        t.feed(&mut ws, b"%extended-output %5 0 : old\n");
        t.feed(&mut ws, &reply(8, &["$ now".into()]));
        t.feed(&mut ws, &reply(9, &[]));
        let a = t.feed(&mut ws, b"%extended-output %5 0 : x\n");
        assert!(a.is_empty(), "{a:?}");
        // The state after the captures (cursor at 2,3) finishes it.
        let a = t.feed(
            &mut ws,
            &reply(10, &[pane_line(5, 1, 80, 24, "b25f,80x24,0,0,5")]),
        );
        let fed = a.iter().find_map(|a| match a {
            Action::Feed { native: n, bytes } if *n == native => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            _ => None,
        });
        let fed = fed.unwrap();
        assert!(fed.starts_with("\x1b[?1049l"), "{fed:?}");
        assert!(fed.contains("\x1b[0m\x1b[H\x1b[2J$ now"), "{fed:?}");
        assert!(
            fed.ends_with("\x1b[4;3Hx") && !fed.contains("old"),
            "{fed:?}"
        );
    }

    #[test]
    fn a_pane_gone_before_the_redraw_after_a_pause_lets_go() {
        let panes = [pane_line(5, 1, 80, 24, "b25f,80x24,0,0,5")];
        let (mut t, mut ws, _) = attached(Some("tmux"), None, &panes);
        let native = t.native_of(5).unwrap();
        t.feed(&mut ws, &reply(5, &[]));
        t.feed(&mut ws, &reply(6, &[]));
        t.feed(&mut ws, b"%pause %5\n");
        t.feed(&mut ws, b"%begin 9 7 1\n%end 9 7 1\n");
        // The first capture fails: tmux skips the other two on its line.
        let a = t.feed(
            &mut ws,
            b"%begin 9 8 1\ncan't find pane: %5\n%error 9 8 1\n%extended-output %5 0 : y\n",
        );
        assert!(
            a.contains(&Action::Feed {
                native,
                bytes: b"y".to_vec()
            }),
            "{a:?}"
        );
        assert_eq!(t.client.pending(), 0);
    }

    #[test]
    fn a_pause_while_restoring_still_lets_the_pane_go() {
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        t.feed(&mut ws, START);
        let a = t.feed(&mut ws, b"%pause %3\n");
        assert_eq!(writes(&a), ["refresh-client -A '%3:continue'"]);
    }

    #[test]
    fn no_flow_control_before_tmux_3_2_or_when_only_watching() {
        let (_, _, _, a) = attached_after("3.1", |_| Vec::new());
        assert!(
            !writes(&a).iter().any(|w| w.contains("pause-after")),
            "{a:?}"
        );
        let (_, _, _, a) = attached_after("3.4", |t| t.join(None, Some("pair"), false));
        assert!(
            !writes(&a).iter().any(|w| w.contains("pause-after")),
            "{a:?}"
        );
        let (_, _, _, a) = attached_after("3.4", |t| t.join(None, Some("pair"), true));
        assert!(
            writes(&a).iter().any(|w| w.contains("pause-after")),
            "{a:?}"
        );
    }

    #[test]
    fn output_waits_for_the_restored_screen() {
        let panes = [pane_line(5, 1, 80, 24, "b25f,80x24,0,0,5")];
        let (mut t, mut ws, _) = attached(Some("tmux"), None, &panes);
        assert_eq!(t.mode(), Mode::Tmux);
        let native = t.native_of(5).unwrap();
        // Before the capture: already in it, dropped. During and after:
        // kept, and fed after the screen.
        let mut input = b"%output %5 old\n".to_vec();
        input.extend(b"%begin 9 5 1\n$ ls\n");
        let a = t.feed(&mut ws, &input);
        assert!(!a.iter().any(|a| matches!(a, Action::Feed { .. })));
        let mut input = b"%end 9 5 1\n%output %5 new\n".to_vec();
        input.extend(reply(6, &[]));
        input.extend(b"%output %5 live\n");
        let a = t.feed(&mut ws, &input);
        let fed: Vec<String> = a
            .iter()
            .filter_map(|a| match a {
                Action::Feed { native: n, bytes } if *n == native => {
                    Some(String::from_utf8_lossy(bytes).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(fed.len(), 2);
        assert!(fed[0].contains("$ ls") && fed[0].ends_with("new") && !fed[0].contains("old"));
        assert_eq!(fed[1], "live");
    }

    #[test]
    fn splits_place_new_windows_and_save() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("ferossh"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        let w = writes(&t.split(a0, Dir::Down));
        assert_eq!(w, ["display-message -p -t %0 '#{pane_current_path}'"]);
        // Replies 5 and 6 were the captures; 7 the pane's directory (none
        // here) and 8 the new window.
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &[]));
        assert_eq!(
            writes(&t.feed(&mut ws, &input)),
            ["new-window -d -P -F '#{window_id}'"]
        );
        let mut input = reply(8, &["@4".into()]);
        input.extend(b"%window-add @4\n");
        let a = t.feed(&mut ws, &input);
        assert_eq!(
            writes(&a),
            [format!(
                "list-panes -t @4 -F {}",
                command::quote(PANE_FORMAT)
            )]
        );
        let a = t.feed(
            &mut ws,
            &reply(9, &[pane_line(9, 4, 80, 24, "b25f,80x24,0,0,9")]),
        );
        let n9 = t.native_of(9).unwrap();
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.tabs[0].root.panes(), [a0, n9]);
        assert_eq!(ws.focused(), n9);
        assert!(a.contains(&Action::Focus { native: n9 }));
        // Each window gets its pane's size; the layout and looks are saved.
        let w = writes(&t.sync_sizes(&ws, &|n| Some(if n == a0 { (100, 20) } else { (60, 10) })));
        let check = |w: u32| {
            format!(
                "display-message -p -t $0:@{w} {}",
                command::quote(SIZE_FORMAT)
            )
        };
        assert_eq!(
            w,
            [
                "refresh-client -C @0:100x20".to_owned(),
                check(0),
                "refresh-client -C @4:60x10".to_owned(),
                check(4)
            ]
        );
        assert!(
            writes(&t.sync_sizes(&ws, &|_| Some((100, 20))))
                .iter()
                .all(|l| !l.contains("@0"))
        );
        let look = |n: Native| Look {
            font: Some("Consolas".into()),
            size: Some(if n == n9 { 16.0 } else { 10.0 }),
            theme: None,
            name: None,
        };
        let w = writes(&t.sync_saved(&ws, &look));
        assert_eq!(
            w,
            [
                "set-option -q @ferossh-layout 'ferossh1 active=0 tab=c500(@0,@4)/@4 \
              look=@0,Consolas,10, look=@4,Consolas,16,'"
            ]
        );
        assert!(
            t.sync_saved(&ws, &look).is_empty(),
            "unchanged: not sent again"
        );
        // A named session renames its window and is saved with its look.
        assert_eq!(
            writes(&t.rename(n9, Some("it's db"))),
            ["rename-window -t @4 'its db'"]
        );
        let named = |n: Native| Look {
            name: (n == n9).then(|| "db".to_owned()),
            ..look(n)
        };
        assert!(writes(&t.sync_saved(&ws, &named))[0].ends_with("look=@4,Consolas,16,,db'"));
        assert_eq!(
            writes(&t.rename(n9, None)),
            ["set-option -w -t @4 automatic-rename on"]
        );
        assert!(t.rename(999, Some("x")).is_empty());
        // Closing a pane kills its window; tmux then says so.
        assert_eq!(writes(&t.close(n9)), ["kill-window -t @4"]);
        let a = t.feed(&mut ws, b"%window-close @4\n");
        assert!(a.contains(&Action::Remove { native: n9 }));
        assert_eq!(ws.all_panes(), [a0]);
    }

    #[test]
    fn new_windows_and_panes_start_in_the_start_directory() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("ferossh"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        t.start_dir = Some("/srv/my #1".into());
        // Splits and tabs follow their pane; the start directory is for
        // when tmux has no answer, and for new sessions.
        let ask = ["display-message -p -t %0 '#{pane_current_path}'"];
        assert_eq!(writes(&t.split(a0, Dir::Right)), ask);
        assert_eq!(writes(&t.new_tab(a0)), ask);
        assert_eq!(
            writes(&t.switch_session(None)),
            ["new-session -d -c '/srv/my ##1' -P -F '#{session_name}'"]
        );
        // Session > Set start directory here asks tmux for the pane's.
        assert_eq!(writes(&t.ask_dir(a0)), ask);
        // Replies 5 and 6 were the captures; 7 to 10 the commands above.
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &[]));
        input.extend(reply(8, &[]));
        input.extend(reply(9, &[]));
        input.extend(reply(10, &["/home/me/src".into()]));
        let a = t.feed(&mut ws, &input);
        assert_eq!(
            writes(&a),
            [
                "new-window -d -c '/srv/my ##1' -P -F '#{window_id}'",
                "new-window -d -c '/srv/my ##1' -P -F '#{window_id}'"
            ]
        );
        assert!(
            a.contains(&Action::StartDir("/home/me/src".into())),
            "{a:?}"
        );
        assert!(t.ask_dir(999).is_empty());
        // Show what changed asks the same, for that pane; an empty answer
        // is said rather than dropped.
        assert_eq!(writes(&t.ask_diff_dir(a0)), ask);
        assert_eq!(writes(&t.ask_diff_dir(a0)), ask);
        // 11 and 12 were the new windows above.
        let mut input = reply(11, &[]);
        input.extend(reply(12, &[]));
        input.extend(reply(13, &["/home/me/app ".into()]));
        input.extend(reply(14, &[]));
        let a = t.feed(&mut ws, &input);
        assert!(
            a.contains(&Action::DiffDir {
                native: a0,
                dir: "/home/me/app ".into()
            }),
            "{a:?}"
        );
        assert!(a.iter().any(|x| matches!(x, Action::Notice(_))), "{a:?}");
        assert!(t.ask_diff_dir(999).is_empty());
    }

    #[test]
    fn agents_get_a_marked_window_and_their_command_typed() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("ferossh"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        assert_eq!(t.agent_of(a0), None);
        // Its pane's directory first, as for a new tab.
        assert_eq!(
            writes(&t.start_agent(a0, "  claude --resume ", None, None)),
            ["display-message -p -t %0 '#{pane_current_path}'"]
        );
        // Replies 5 and 6 were the captures.
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &["/srv/app".into()]));
        let a = t.feed(&mut ws, &input);
        assert_eq!(
            writes(&a),
            ["new-window -d -c '/srv/app' -n 'claude' -P -F '#{window_id} #{pane_id}'"]
        );
        let a = t.feed(&mut ws, &reply(8, &["@4 %9".into()]));
        let w = writes(&a);
        assert_eq!(w[0], "set-option -w -t @4 @ferossh-agent 'claude'");
        // "claude --resume" and Enter, as hex: nothing read as a key name.
        assert!(
            w[1].starts_with("send-keys -t %9 -H 63 6c 61 75 64 65 20 2d 2d"),
            "{w:?}"
        );
        assert!(w[1].ends_with(" 0d"), "{w:?}");
        assert!(w[2].starts_with("list-panes -t @4 -F"), "{w:?}");
        let mut line = pane_line(9, 4, 80, 24, "b25f,80x24,0,0,9");
        line.push_str(" claude");
        // 9 and 10 were the option and the keys.
        let mut input = reply(9, &[]);
        input.extend(reply(10, &[]));
        input.extend(reply(11, &[line]));
        let a = t.feed(&mut ws, &input);
        let native = a
            .iter()
            .find_map(|a| match a {
                Action::Create {
                    native, pane: 9, ..
                } => Some(*native),
                _ => None,
            })
            .unwrap();
        assert_eq!(t.agent_of(native), Some("claude"));
        assert_eq!(t.agent_of(a0), None);
    }

    #[test]
    fn a_window_marked_later_turns_its_panes_into_agent_panes() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("ferossh"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        // Replies 5 and 6 were the captures.
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(b"%layout-change @0 x,80x24,0,0{40x24,0,0,0,39x24,41,0,1} x *\n");
        let a = t.feed(&mut ws, &input);
        assert_eq!(writes(&a).len(), 1, "asks for the window's panes");
        let mut line = pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0");
        line.push_str(" claude");
        let a = t.feed(&mut ws, &reply(7, &[line]));
        assert!(
            a.iter()
                .any(|a| matches!(a, Action::AgentMarked(n) if n == &[a0])),
            "{a:?}"
        );
        assert_eq!(t.agent_of(a0), Some("claude"));
    }

    #[test]
    fn an_agent_from_a_shell_starts_in_that_shells_directory() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, _ws, _) = attached(Some("ferossh"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        assert_eq!(
            writes(&t.start_agent(a0, "codex", Some("/srv/api #1"), None)),
            ["new-window -d -c '/srv/api ##1' -n 'codex' -P -F '#{window_id} #{pane_id}'"]
        );
    }

    #[test]
    fn an_agent_in_a_worktree_starts_there_and_its_window_keeps_it() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("ferossh"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        let wt = Worktree {
            branch: "claude-2".into(),
            path: "/srv/my app's-claude-2".into(),
        };
        // The worktree's folder, not the pane's or the one given.
        assert_eq!(
            writes(&t.start_agent(a0, "claude", Some("/srv/other"), Some(&wt))),
            [
                "new-window -d -c '/srv/my app'\\''s-claude-2' -n 'claude' -P -F '#{window_id} #{pane_id}'"
            ]
        );
        // Replies 5 and 6 were the captures.
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &["@4 %9".into()]));
        let a = t.feed(&mut ws, &input);
        let w = writes(&a);
        // The worktree's mark first, the apostrophe kept.
        assert_eq!(
            w[0],
            "set-option -w -t @4 @ferossh-worktree 'claude-2 /srv/my app'\\''s-claude-2'"
        );
        assert_eq!(w[1], "set-option -w -t @4 @ferossh-agent 'claude'");
        // Read back on the next attach, from any PC.
        let mut line = pane_line(9, 4, 80, 24, "b25f,80x24,0,0,9");
        line.push_str(" claude claude-2 /srv/my app's-claude-2");
        let parsed = parse_pane_line(line.as_bytes()).unwrap();
        assert_eq!(parsed.agent.as_deref(), Some("claude"));
        assert_eq!(parsed.worktree.as_ref(), Some(&wt));
        // Only with an agent, and only as written.
        let mut line = pane_line(9, 4, 80, 24, "b25f,80x24,0,0,9");
        line.push_str("  claude-2 /srv/x");
        assert_eq!(parse_pane_line(line.as_bytes()).unwrap().worktree, None);
        let mut line = pane_line(9, 4, 80, 24, "b25f,80x24,0,0,9");
        line.push_str(" claude ../x /srv/x");
        assert_eq!(parse_pane_line(line.as_bytes()).unwrap().worktree, None);
        // 8 to 10 were the two options and the keys.
        let mut input = reply(8, &[]);
        input.extend(reply(9, &[]));
        input.extend(reply(10, &[]));
        let mut line = pane_line(9, 4, 80, 24, "b25f,80x24,0,0,9");
        line.push_str(" claude claude-2 /srv/my app's-claude-2");
        input.extend(reply(11, &[line]));
        let a = t.feed(&mut ws, &input);
        let native = a
            .iter()
            .find_map(|a| match a {
                Action::Create {
                    native, pane: 9, ..
                } => Some(*native),
                _ => None,
            })
            .unwrap();
        assert_eq!(t.worktree_of(native), Some(&wt));
        assert_eq!(t.worktree_of(a0), None);
        // The new pane's captures come first.
        let captures = writes(&a).len();
        // Remove worktree closes the agent's whole window, and only an
        // agent's.
        assert_eq!(writes(&t.close_agent_window(native)), ["kill-window -t @4"]);
        assert!(t.close_agent_window(a0).is_empty());
        // Start in a new worktree asks tmux for the pane's folder first.
        assert_eq!(
            writes(&t.ask_worktree_dir(a0)),
            ["display-message -p -t %0 '#{pane_current_path}'"]
        );
        let mut input = Vec::new();
        for n in 12..12 + captures {
            input.extend(reply(n as u32, &[]));
        }
        let next = (12 + captures) as u32;
        input.extend(reply(next, &[]));
        input.extend(reply(next + 1, &["/srv/my app ".into()]));
        let a = t.feed(&mut ws, &input);
        assert!(
            a.contains(&Action::WorktreeDir {
                native: a0,
                dir: Some("/srv/my app ".into())
            }),
            "{a:?}"
        );
    }

    #[test]
    fn agent_names_are_plain() {
        assert_eq!(agent_name("claude --resume"), "claude");
        assert_eq!(agent_name("FOO=1 /opt/bin/aider --model x"), "aider");
        assert_eq!(agent_name("codex"), "codex");
        assert_eq!(agent_name("$(evil)"), "evil");
        assert_eq!(agent_name("'"), "agent");
        assert_eq!(agent_name(""), "agent");
        // Read back only as written.
        assert_eq!(agent_name_of_option("claude"), Some("claude".into()));
        assert_eq!(agent_name_of_option("bad\u{202e}x"), None);
        assert_eq!(agent_name_of_option(""), None);
        let mut line = pane_line(3, 1, 80, 24, "b25f,80x24,0,0,3");
        line.push_str(" gemini");
        assert_eq!(
            parse_pane_line(line.as_bytes()).unwrap().agent.as_deref(),
            Some("gemini")
        );
        line.push_str("$x");
        assert_eq!(parse_pane_line(line.as_bytes()).unwrap().agent, None);
        // A marker that is not UTF-8 costs the agent name, not the pane.
        let mut bytes = pane_line(3, 1, 80, 24, "b25f,80x24,0,0,3").into_bytes();
        bytes.extend_from_slice(b" gem\xffini");
        assert_eq!(parse_pane_line(&bytes).unwrap().agent, None);
    }

    #[test]
    fn splits_in_the_same_directory_ask_tmux_first() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("ferossh"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        assert_eq!(
            writes(&t.split(a0, Dir::Right)),
            ["display-message -p -t %0 '#{pane_current_path}'"]
        );
        // Replies 5 and 6 were the captures; 7 is the directory.
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &["/srv/my #1".into()]));
        assert_eq!(
            writes(&t.feed(&mut ws, &input)),
            ["new-window -d -c '/srv/my ##1' -P -F '#{window_id}'"]
        );
        // The new window lands next to the pane, as a plain split's does.
        let mut input = reply(8, &["@4".into()]);
        input.extend(b"%window-add @4\n");
        t.feed(&mut ws, &input);
        t.feed(
            &mut ws,
            &reply(9, &[pane_line(9, 4, 80, 24, "b25f,80x24,0,0,9")]),
        );
        let n9 = t.native_of(9).unwrap();
        assert_eq!(ws.tabs[0].root.panes(), [a0, n9]);
        // A path tmux cannot take, or a failed query: a plain split.
        // Replies 10 and 11 are the new pane's captures.
        assert_eq!(writes(&t.split(a0, Dir::Down)).len(), 1);
        let mut input = reply(10, &[]);
        input.extend(reply(11, &[]));
        input.extend(reply(12, &["/tmp/a\tb".into()]));
        assert_eq!(
            writes(&t.feed(&mut ws, &input)),
            ["new-window -d -P -F '#{window_id}'"]
        );
    }

    #[test]
    fn tmux_layout_splits_in_the_same_directory() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("tmux"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        assert_eq!(writes(&t.split(a0, Dir::Down)).len(), 1);
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &["/home/me/src".into()]));
        assert_eq!(
            writes(&t.feed(&mut ws, &input)),
            ["split-window -v -c '/home/me/src' -t %0 -P -F '#{pane_id}'"]
        );
    }

    #[test]
    fn a_client_coming_or_going_counts_as_shared_until_tmux_answers() {
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        t.feed(&mut ws, START);
        let mut input = reply(1, &["3.4 /dev/pts/3".into()]);
        input.extend(reply(2, &[]));
        input.extend(reply(3, &[]));
        input.extend(reply(4, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]));
        let a = t.feed(&mut ws, &input);
        assert!(!t.maybe_shared(), "alone");
        // Captures of the restored pane.
        let mut n = 4;
        for _ in writes(&a) {
            n += 1;
            t.feed(&mut ws, &reply(n, &[]));
        }
        // Another client attached: shared at once, before the count.
        let a = t.feed(&mut ws, b"%client-session-changed /dev/pts/4 $0 main\n");
        assert_eq!(writes(&a), ["list-clients -t $0 -F '#{client_name}'"]);
        assert!(t.maybe_shared());
        assert_eq!(t.others_attached(), 0);
        t.feed(
            &mut ws,
            &reply(n + 1, &["/dev/pts/3".into(), "/dev/pts/4".into()]),
        );
        assert!(t.maybe_shared());
        assert_eq!(t.others_attached(), 1);
        // It left: still asked about until tmux says so.
        t.feed(&mut ws, b"%client-detached /dev/pts/4\n");
        assert!(t.maybe_shared());
        t.feed(&mut ws, &reply(n + 2, &["/dev/pts/3".into()]));
        assert!(!t.maybe_shared());
    }

    #[test]
    fn other_clients_keeping_a_window_small_are_offered_to_detach() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, a) = attached(None, None, &panes);
        let mut n = 4;
        let mut answer = |t: &mut Tmux, ws: &mut Workspace, lines: &[String]| {
            n += 1;
            t.feed(ws, &reply(n, lines))
        };
        for _ in writes(&a) {
            answer(&mut t, &mut ws, &[]);
        }
        // tmux gives the window the size asked for: nothing to say.
        let w = writes(&t.sync_sizes(&ws, &|_| Some((90, 30))));
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[1].starts_with("display-message -p -t $0:@0 "));
        answer(&mut t, &mut ws, &[]);
        let a = answer(&mut t, &mut ws, &["90 30 2".into()]);
        assert!(!a.iter().any(|a| matches!(a, Action::Crowded { .. })));
        // A manual size is not the other clients' doing: no question.
        assert_eq!(writes(&t.sync_sizes(&ws, &|_| Some((110, 35)))).len(), 2);
        answer(&mut t, &mut ws, &[]);
        let a = answer(&mut t, &mut ws, &["90 30 2 manual".into()]);
        assert!(!a.iter().any(|a| matches!(a, Action::Crowded { .. })));
        // A smaller window with other clients attached: asked once.
        assert_eq!(writes(&t.sync_sizes(&ws, &|_| Some((120, 40)))).len(), 2);
        answer(&mut t, &mut ws, &[]);
        let a = answer(&mut t, &mut ws, &["100 30 3".into()]);
        assert!(a.contains(&Action::Crowded {
            session: 0,
            have: (100, 30),
            want: (120, 40),
            others: 2
        }));
        let w = writes(&t.sync_sizes(&ws, &|_| Some((121, 40))));
        assert_eq!(w, ["refresh-client -C @0:121x40"]);
        // Only this session's other clients go, and only while it is
        // still the session asked about.
        assert!(t.detach_others(Some(7)).is_empty());
        assert_eq!(
            writes(&t.detach_others(Some(0))),
            ["display-message -p '#{client_name}'"]
        );
        answer(&mut t, &mut ws, &[]);
        let a = answer(&mut t, &mut ws, &["client-2".into()]);
        assert_eq!(writes(&a), ["list-clients -t $0 -F '#{client_name}'"]);
        let a = answer(
            &mut t,
            &mut ws,
            &["client-1".into(), "client-2".into(), "/dev/pts/3".into()],
        );
        assert_eq!(
            writes(&a),
            [
                "detach-client -t 'client-1'",
                "detach-client -t '/dev/pts/3'"
            ]
        );
        // Smaller but alone (tmux clamps the size itself), or a manual
        // size: no question.
        let check = parse_size_check("100 30 1 latest").unwrap();
        assert_eq!(
            (check.have, check.clients, check.manual),
            ((100, 30), 1, false)
        );
        assert!(parse_size_check("100 30 2 manual").unwrap().manual);
        assert_eq!(parse_size_check("100 x 1"), None);
    }

    #[test]
    fn tmux_layout_follows_tmux_splits() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("tmux"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        assert_eq!(writes(&t.split(a0, Dir::Right)).len(), 1);
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &[]));
        assert_eq!(
            writes(&t.feed(&mut ws, &input)),
            ["split-window -h -t %0 -P -F '#{pane_id}'"]
        );
        let mut input = reply(8, &["%1".into()]);
        input.extend(b"%layout-change @0 x,80x24,0,0{40x24,0,0,0,39x24,41,0,1} x *\n");
        let a = t.feed(&mut ws, &input);
        assert_eq!(writes(&a).len(), 1, "asks for the new pane");
        let layout = "x,80x24,0,0{40x24,0,0,0,39x24,41,0,1}";
        t.feed(
            &mut ws,
            &reply(
                9,
                &[
                    pane_line(0, 0, 40, 24, layout),
                    pane_line(1, 0, 39, 24, layout),
                ],
            ),
        );
        let n1 = t.native_of(1).unwrap();
        assert_eq!(ws.tabs[0].root.panes(), [a0, n1]);
        assert_eq!(ws.focused(), n1);
        // Native sizes go back as the window's size plus a layout.
        let w = writes(&t.sync_sizes(&ws, &|n| Some(if n == a0 { (50, 30) } else { (49, 31) })));
        assert_eq!(w.len(), 3);
        assert_eq!(w[0], "refresh-client -C @0:100x30");
        assert!(w[1].starts_with("display-message -p -t $0:@0 "));
        assert!(
            w[2].starts_with("select-layout -t @0 '")
                && w[2].ends_with(",100x30,0,0{50x30,0,0,0,49x30,51,0,1}'")
        );
        // tmux resizes: the native terminals follow, the tree stays.
        let a = t.feed(
            &mut ws,
            b"%layout-change @0 x,100x30,0,0{50x30,0,0,0,49x30,51,0,1} x *\n",
        );
        assert!(a.contains(&Action::Resize {
            native: n1,
            cols: 49,
            rows: 30
        }));
        assert!(!a.contains(&Action::Structure));
        // A pane closes in tmux.
        let a = t.feed(&mut ws, b"%layout-change @0 x,100x30,0,0,1 x *\n");
        assert!(a.contains(&Action::Remove { native: a0 }));
        assert_eq!(ws.all_panes(), [n1]);
    }

    #[test]
    fn exit_brings_the_gateway_back() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(None, None, &panes);
        let a0 = t.native_of(0).unwrap();
        let a = t.feed(&mut ws, b"%exit detached\n\x1b\\[detached]\r\n");
        assert!(a.contains(&Action::ShowGateway));
        assert!(a.contains(&Action::Remove { native: a0 }));
        assert!(a.contains(&Action::GatewayOutput(b"[detached]\r\n".to_vec())));
        assert_eq!(ws.all_panes(), [t.gateway]);
        assert!(!t.active());
    }

    #[test]
    fn a_lost_connection_remembers_the_session() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(None, None, &panes);
        let a = t.connection_lost(&mut ws);
        assert!(a.contains(&Action::ShowGateway));
        assert_eq!(t.reattach.as_deref(), Some("main"));
        assert_eq!(ws.all_panes(), [t.gateway]);
    }

    #[test]
    fn old_tmux_gets_tmux_layout_or_nothing() {
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        t.feed(&mut ws, START);
        let mut input = reply(1, &["3.2a".into()]);
        input.extend(reply(2, &[]));
        input.extend(reply(3, &[]));
        input.extend(reply(4, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]));
        let a = t.feed(&mut ws, &input);
        assert_eq!(t.mode(), Mode::Tmux);
        assert!(a.iter().any(|a| matches!(a, Action::Notice(_))));
        assert!(!writes(&a).iter().any(|w| w.contains("@ferossh-mode")));
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        t.feed(&mut ws, START);
        let mut input = reply(1, &["2.9".into()]);
        input.extend(reply(2, &[]));
        input.extend(reply(3, &[]));
        input.extend(reply(4, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]));
        let a = t.feed(&mut ws, &input);
        assert!(writes(&a).contains(&"detach-client".to_owned()));
        assert_eq!(ws.all_panes(), [gateway]);
    }

    #[test]
    fn picks_other_sessions() {
        let (mut ws, gateway) = Workspace::new();
        let mut t = Tmux::new(gateway, Mode::Ferossh, 100);
        assert_eq!(t.attach("work", true), b"tmux -CC new-session -A -s work\r");
        let a = t.feed(&mut ws, START);
        assert_eq!(writes(&a).len(), 5);
        let mut input = reply(1, &["3.4".into()]);
        input.extend(reply(2, &[]));
        input.extend(reply(3, &[]));
        input.extend(reply(4, &[pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")]));
        t.feed(&mut ws, &input);
        let a = t.feed(
            &mut ws,
            &reply(5, &["$0 main".into(), "$1 work".into(), "$2 x y".into()]),
        );
        assert!(a.contains(&Action::Picker(vec!["work".into(), "x y".into()])));
        assert_eq!(
            writes(&t.switch_session(Some("x y"))),
            ["switch-client -t '=x y'"]
        );
    }

    #[test]
    fn input_goes_as_hex_keys() {
        let panes = [pane_line(7, 0, 80, 24, "b25f,80x24,0,0,7")];
        let (mut t, _ws, _) = attached(None, None, &panes);
        let n = t.native_of(7).unwrap();
        assert_eq!(
            writes(&t.input(n, b"ls\r")),
            ["send-keys -t %7 -H 6c 73 0d"]
        );
        assert!(t.input(999, b"x").is_empty());
    }

    #[test]
    fn pastes_go_through_tmux_so_it_brackets_them() {
        let panes = [pane_line(7, 0, 80, 24, "b25f,80x24,0,0,7")];
        let (mut t, _ws, _) = attached(None, None, &panes);
        let n = t.native_of(7).unwrap();
        assert_eq!(
            writes(&t.input(n, b"\x1b[200~a;b\r\x1b[201~")),
            [
                r#"set-buffer -b ferossh-paste "a\073b\015""#,
                "paste-buffer -p -r -d -b ferossh-paste -t %7",
            ]
        );
        // An escape character held back before the paste is typed first.
        assert_eq!(
            writes(&t.input(n, b"~\x1b[200~x\x1b[201~")),
            [
                "send-keys -t %7 -H 7e",
                r#"set-buffer -b ferossh-paste "x""#,
                "paste-buffer -p -r -d -b ferossh-paste -t %7",
            ]
        );
    }
}
