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
    Client, Event, MIN_VERSION, Notification, PaneId, SessionId, Version, WINDOW_SIZES_VERSION,
    WindowId, command,
};

use crate::layout::{Axis, Dir, Node, PaneId as Native, Workspace};

/// Output kept for a pane until its screen is restored.
const MAX_BUFFER: usize = 4 << 20;
/// Panes whose output is kept before they are known.
const MAX_ORPHANS: usize = 64;
/// Windows FeroSSH shows; more are left to tmux.
const MAX_WINDOWS: usize = 256;
/// History lines brought back per pane, at most.
pub const MAX_HISTORY: usize = 10_000;

/// What each pane's state line holds, in this order.
const PANE_FORMAT: &str = "#{pane_id} #{window_id} #{pane_width} #{pane_height} \
    #{cursor_x} #{cursor_y} #{alternate_on} #{cursor_flag} #{keypad_cursor_flag} \
    #{keypad_flag} #{mouse_standard_flag} #{mouse_button_flag} #{mouse_any_flag} \
    #{mouse_sgr_flag} #{insert_flag} #{wrap_flag} #{scroll_region_upper} \
    #{scroll_region_lower} #{window_layout}";

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
}

/// Where a window made from a native action goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    Split { next_to: Native, dir: Dir },
    Tab { after: Native },
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
    NewWindow(Placement),
    SplitPane,
    Sessions,
    NewSession,
    /// The reply does not matter.
    Ignore,
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
    mouse_any: bool,
    mouse_sgr: bool,
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
}

fn parse_pane_line(line: &[u8]) -> Option<PaneLine> {
    let text = std::str::from_utf8(line).ok()?;
    let f: Vec<&str> = text.split(' ').collect();
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
            mouse_any: b(ma),
            mouse_sgr: b(sgr),
            insert: b(ins),
            wrap: *wrap != "0",
            region: (n(top), n(bottom)),
        },
        layout: (*layout).to_owned(),
    })
}

/// The bytes that redraw a pane as tmux has it: its history and screen
/// (`capture-pane`), the alternate screen, the modes and the cursor.
fn restore_bytes(state: &PaneState, normal: &[Vec<u8>], alternate: &[Vec<u8>]) -> Vec<u8> {
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
    join(&mut out, normal);
    if state.alternate {
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
        (state.mouse_any, b"\x1b[?1003h"),
        (state.mouse_sgr, b"\x1b[?1006h"),
        (state.insert, b"\x1b[4h"),
        (!state.wrap, b"\x1b[?7l"),
        (!state.cursor, b"\x1b[?25l"),
    ] {
        if on {
            out.extend_from_slice(seq);
        }
    }
    out.extend_from_slice(
        format!("\x1b[{};{}H", state.cursor_y + 1, state.cursor_x + 1).as_bytes(),
    );
    out
}

/// A pane's screen is being brought back: output waits.
#[derive(Debug, Default)]
struct Restoring {
    buffer: Vec<u8>,
    state: PaneState,
    normal: Vec<Vec<u8>>,
}

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
    saved_text: Option<String>,
    /// The session to attach again after a reconnect.
    pub reattach: Option<String>,
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
            saved_text: None,
            reattach: None,
        }
    }

    /// In control mode.
    pub fn active(&self) -> bool {
        self.client.is_control()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn session_name(&self) -> Option<&str> {
        self.session.as_ref().map(|(_, n)| n.as_str())
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
        command::attach_line(name).into_bytes()
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
                    self.version = None;
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
            self.reattach = self.session.as_ref().map(|(_, n)| n.clone());
            self.client.reset();
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
            Notification::Pause { pane } => {
                self.send(
                    &format!("refresh-client -A '%{pane}:continue'"),
                    Tag::Ignore,
                    out,
                );
            }
            Notification::Exit { reason } if !reason.is_empty() => {
                out.push(Action::Notice(format!("tmux: {reason}")));
            }
            _ => {}
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
        self.placements.clear();
        self.sent_sizes.clear();
        self.sent_client_size = None;
        self.sent_layouts.clear();
        self.saved_text = None;
        self.restoring = true;
        self.send("display-message -p '#{version}'", Tag::Version, out);
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
    }

    fn list_sessions(&mut self, out: &mut Vec<Action>) {
        self.send(
            "list-sessions -F '#{session_id} #{session_name}'",
            Tag::Sessions,
            out,
        );
    }

    /// Session > Switch tmux session.
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
            Some(n) => self.send(
                &format!("switch-client -t {}", command::quote(&format!("={n}"))),
                Tag::Ignore,
                &mut out,
            ),
            None => self.send(
                "new-session -d -P -F '#{session_name}'",
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
            Tag::Version => self.version = ok.then(|| Version::parse(&first())).flatten(),
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
            Tag::CaptureAlternate(p) => {
                if let Some(pane) = self.panes.get_mut(&p)
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
        } else if stored.is_none() {
            let line = format!("set-option -q {} {}", saved::MODE_OPTION, self.mode.name());
            self.send(&line, Tag::Ignore, out);
        }
        // Group the panes by window, in tmux's order.
        let mut order: Vec<WindowId> = Vec::new();
        let mut infos: BTreeMap<WindowId, Vec<PaneLine>> = BTreeMap::new();
        for line in lines.iter().filter_map(|l| parse_pane_line(l)) {
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
            return;
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
            out.push(Action::Focus { native: f });
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
                    }),
                },
            );
            let history = self.history;
            let p = line.pane;
            self.send(
                &format!("capture-pane -p -e -q -t %{p} -S -{history}"),
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
        self.panes.clear();
        self.windows.clear();
        self.orphans.clear();
        self.discovering.clear();
        self.placements.clear();
        self.session = None;
        self.restoring = false;
        out.push(Action::Structure);
    }

    /// Typed or pasted bytes for a native pane.
    pub fn input(&mut self, native: Native, bytes: &[u8]) -> Vec<Action> {
        let mut out = Vec::new();
        if let Some(p) = self.pane_of(native) {
            for line in command::send_keys(p, bytes) {
                self.send(&line, Tag::Ignore, &mut out);
            }
        }
        out
    }

    /// Splits native pane `native`: a new window beside it in FeroSSH
    /// layout, a tmux split in tmux layout.
    pub fn split(&mut self, native: Native, dir: Dir) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(p) = self.pane_of(native) else {
            return out;
        };
        match self.mode {
            Mode::Ferossh => self.send(
                "new-window -d -P -F '#{window_id}'",
                Tag::NewWindow(Placement::Split {
                    next_to: native,
                    dir,
                }),
                &mut out,
            ),
            Mode::Tmux => {
                let flags = match dir {
                    Dir::Right => "-h",
                    Dir::Left => "-hb",
                    Dir::Down => "-v",
                    Dir::Up => "-vb",
                };
                self.send(
                    &format!("split-window {flags} -t %{p} -P -F '#{{pane_id}}'"),
                    Tag::SplitPane,
                    &mut out,
                );
            }
        }
        out
    }

    /// A new tmux window as a tab after `native`'s.
    pub fn new_tab(&mut self, native: Native) -> Vec<Action> {
        let mut out = Vec::new();
        self.send(
            "new-window -d -P -F '#{window_id}'",
            Tag::NewWindow(Placement::Tab { after: native }),
            &mut out,
        );
        out
    }

    /// Closes the tmux pane (its window, when it is the only one there).
    /// The native pane goes when tmux says it went.
    pub fn close(&mut self, native: Native) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(p) = self.pane_of(native) else {
            return out;
        };
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

    /// Leaves tmux running and the gateway at its shell.
    pub fn detach(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        self.reattach = None;
        self.send("detach-client", Tag::Ignore, &mut out);
        out
    }

    /// Switches the session's layout mode and reads it again.
    pub fn set_mode(&mut self, mode: Mode) -> Vec<Action> {
        let mut out = Vec::new();
        if !self.client.is_control() || mode == self.mode {
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
        if !self.client.is_control() || self.restoring {
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
            if self.window_sizes() {
                if self.sent_sizes.get(&w) != Some(&size) {
                    self.sent_sizes.insert(w, size);
                    let line = format!("refresh-client -C @{w}:{}x{}", size.0, size.1);
                    self.send(&line, Tag::Ignore, &mut out);
                }
            } else if focused == Some(w) && self.sent_client_size != Some(size) {
                self.sent_client_size = Some(size);
                let line = format!("refresh-client -C {}x{}", size.0, size.1);
                self.send(&line, Tag::Ignore, &mut out);
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
        if !self.client.is_control() || self.restoring || self.panes.is_empty() {
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
            "%{pane} @{window} {w} {h} 2 3 0 1 0 0 0 0 0 0 0 1 0 {} {layout}",
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

    fn writes(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Write(b) => Some(String::from_utf8_lossy(b).trim_end().to_owned()),
                _ => None,
            })
            .collect()
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
        let actions = t.feed(&mut ws, &input);
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
        assert!(parse_pane_line(b"x @1 80 24 0 0 0 0 0 0 0 0 0 0 0 0 0 0 l").is_none());
    }

    #[test]
    fn restores_screens_and_modes() {
        let state = PaneState {
            width: 80,
            height: 24,
            cursor_x: 4,
            cursor_y: 1,
            alternate: true,
            cursor: false,
            keypad_cursor: true,
            mouse_sgr: true,
            wrap: true,
            region: (0, 23),
            ..PaneState::default()
        };
        let bytes = restore_bytes(&state, &[b"a".to_vec(), b"b".to_vec()], &[b"vim".to_vec()]);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(
            text,
            "\x1b[0m\x1b[H\x1b[2Ja\r\nb\x1b[0m\x1b[?1049h\x1b[H\x1b[2Jvim\x1b[0m\
             \x1b[?1h\x1b[?1006h\x1b[?25l\x1b[2;5H"
        );
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
        assert!(w.contains(&"capture-pane -p -e -q -t %3 -S -100".to_owned()));
        assert_eq!(t.mode(), Mode::Ferossh);
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
        assert_eq!(w, ["new-window -d -P -F '#{window_id}'"]);
        // Replies 5 and 6 were the captures; 7 is the new window.
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &["@4".into()]));
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
            &reply(8, &[pane_line(9, 4, 80, 24, "b25f,80x24,0,0,9")]),
        );
        let n9 = t.native_of(9).unwrap();
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.tabs[0].root.panes(), [a0, n9]);
        assert_eq!(ws.focused(), n9);
        assert!(a.contains(&Action::Focus { native: n9 }));
        // Each window gets its pane's size; the layout and looks are saved.
        let w = writes(&t.sync_sizes(&ws, &|n| Some(if n == a0 { (100, 20) } else { (60, 10) })));
        assert_eq!(
            w,
            ["refresh-client -C @0:100x20", "refresh-client -C @4:60x10"]
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
        // Closing a pane kills its window; tmux then says so.
        assert_eq!(writes(&t.close(n9)), ["kill-window -t @4"]);
        let a = t.feed(&mut ws, b"%window-close @4\n");
        assert!(a.contains(&Action::Remove { native: n9 }));
        assert_eq!(ws.all_panes(), [a0]);
    }

    #[test]
    fn tmux_layout_follows_tmux_splits() {
        let panes = [pane_line(0, 0, 80, 24, "b25f,80x24,0,0,0")];
        let (mut t, mut ws, _) = attached(Some("tmux"), None, &panes);
        let a0 = t.native_of(0).unwrap();
        assert_eq!(
            writes(&t.split(a0, Dir::Right)),
            ["split-window -h -t %0 -P -F '#{pane_id}'"]
        );
        let mut input = reply(5, &[]);
        input.extend(reply(6, &[]));
        input.extend(reply(7, &["%1".into()]));
        input.extend(b"%layout-change @0 x,80x24,0,0{40x24,0,0,0,39x24,41,0,1} x *\n");
        let a = t.feed(&mut ws, &input);
        assert_eq!(writes(&a).len(), 1, "asks for the new pane");
        let layout = "x,80x24,0,0{40x24,0,0,0,39x24,41,0,1}";
        t.feed(
            &mut ws,
            &reply(
                8,
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
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], "refresh-client -C @0:100x30");
        assert!(
            w[1].starts_with("select-layout -t @0 '")
                && w[1].ends_with(",100x30,0,0{50x30,0,0,0,49x30,51,0,1}'")
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
}
