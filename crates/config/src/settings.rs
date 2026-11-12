//! FeroSSH's own settings file, `%APPDATA%\FeroSSH\ferossh.conf`: session
//! profiles (a host's font, text size and colour theme), favourite hosts,
//! colour theme rules and a few window options.
//!
//! It uses ssh_config's syntax, so it reads like the file it sits beside:
//!
//! ```text
//! SidePane right
//! DownloadFolder "D:\Transfers"
//! VerifyTransfers yes
//! ZModem yes
//! AutoReconnect yes
//! RemoteAppCopyAgent yes
//! RemoteAppRemoveAgent no
//! RemoteAppShareSelection no
//! DesktopClipboard both
//! DesktopShareSelection no
//! Highlight yes
//! HighlightStandard yes
//! HighlightRule
//!     Words error failed "permission denied"
//!     Colour bright-red
//!     Bold yes
//! HighlightRule
//!     Pattern \bOOM-killer\b
//!     Background red
//!     Stop yes
//! Favourite web
//! ThemeRule
//!     Hosts prod-* *.prod.example.com
//!     Tags production
//!     Theme Production
//!     Tint 800000 60
//! Profile web
//!     Font "Cascadia Mono"
//!     FontSize 12
//!     Theme Dracula
//!     Tmux main
//!     TmuxLayout ferossh
//!     StartDir /srv/www
//! Profile core-switch
//!     LegacyAlgorithms yes
//!     Highlight no
//!     AutoReconnect no
//! Profile desktop-box
//!     DesktopPort 3390
//!     VncPort 5901
//!     VirtualDesktop startxfce4
//!     DesktopSize 1920x1080
//!     DesktopClipboard from-host
//! Profile office-pc
//!     Type rdp
//!     Via bastion
//! Profile core-console
//!     Type serial
//!     Serial 115200,8N1,none
//!     LoginScript expect Username:
//!     LoginScript send admin
//!     LoginScript expect Password:
//!     LoginScript password
//!     LoginScript expect >
//!     LoginScript send enable
//!     LoginScript expect Password:
//!     LoginScript secret enable
//! Profile old-router
//!     Type telnet
//! Profile gimp-on-build
//!     Type remoteapp
//!     Via build-box
//!     RemoteApp gimp
//!     RemoteAppRemoveAgent yes
//!     RemoteAppClipboard to-host
//! Profile win-srv
//!     Type winrm
//!     WinrmCommand cmd
//!     WinrmHttps yes
//! Profile laptop-roaming
//!     Type mosh
//!     MoshServer ~/bin/mosh-server
//!     MoshPorts 60001:60010
//!     MoshPredict always
//! Profile uploads
//!     Type sftp
//!     RemoteFolder /incoming
//! Secret enable
//!     PasswordCommand op read "op://Network/%n/password"
//! ```
//!
//! Command snippets have a file each in the `snippets` folder beside it
//! (see [`crate::snippet_store`]), in the same syntax:
//!
//! ```text
//! Snippet "Show interfaces"
//!     Line terminal length 0
//!     Line show ip interface brief
//! Snippet "Back up the config"
//!     Hosts sw-* core-*
//!     Folder Network
//!     Hotkey F2
//!     Line copy running-config tftp://10.0.0.5/${host}-${stamp}.cfg
//!     Line @expect ]?
//!     Line @key enter
//! Snippet "Find in logs"
//!     Line @type grep -i error /var/log/syslog
//! Snippet "Next page"
//!     Hosts sw-*
//!     When --More--
//!     Line @key space
//! ```
//!
//! Older versions kept those `Snippet` blocks in this file; they are
//! read so they can move to the folder.
//!
//! FeroSSH rewrites the file when a setting changes. Unknown keywords are
//! skipped, so an older FeroSSH reads a newer file.

use std::fmt::Write as _;

use crate::highlight::{HighlightColour, HighlightRule};
use crate::host_theme::ThemeRule;
use crate::hotkey::Hotkey;
use crate::login::{SecretSource, Snippet, Step, parse_hotkey, valid_secret_name};
use crate::password::PasswordSource;
use crate::serial::LineSettings;
use crate::ssh_config::split_line;

/// How one host's panes look. Unset fields use the theme rules and
/// defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// The `Host` name from ssh_config, or what was typed.
    pub host: String,
    pub font: Option<String>,
    pub font_size: Option<f32>,
    pub theme: Option<String>,
    /// Lines of scrollback for this host's panes.
    pub scrollback: Option<usize>,
    /// Attach to this tmux session after logging in (off when unset).
    pub tmux: Option<String>,
    /// The tmux layout for new sessions: `ferossh` (one window per pane)
    /// or `tmux` (tmux's own splits).
    pub tmux_layout: Option<String>,
    /// The server directory new shells and tmux windows and panes of
    /// this host start in; the server's choice (the home folder) when
    /// unset.
    pub start_dir: Option<String>,
    /// The folder on this PC File transfer's Local list opens for this
    /// host; the download folder when unset.
    pub local_folder: Option<String>,
    /// The server folder File transfer's Remote list opens for this host;
    /// the shell's or the home folder when unset.
    pub remote_folder: Option<String>,
    /// Also offer the legacy algorithm set (ECDH NIST curves, DH groups
    /// 14 to 18 with SHA-2, AES-CTR, plain HMAC-SHA-2) for older switches,
    /// routers and servers.
    pub legacy_algorithms: bool,
    /// The server's RDP port for Session > Open remote desktop; 3389 when
    /// unset.
    pub desktop_port: Option<u16>,
    /// The server's VNC port for the built-in desktop viewer; 5900 when
    /// unset.
    pub vnc_port: Option<u16>,
    /// What Session > Open virtual desktop runs in its Xvfb display: a
    /// desktop session or a single program, as a shell command. Unset,
    /// it picks a desktop session the server has.
    pub virtual_desktop: Option<String>,
    /// The size of this host's remote and virtual desktops: following
    /// the pane (the default, when unset) or fixed.
    pub desktop_size: Option<DesktopSize>,
    /// What the session opens: a shell (the default) or a desktop.
    pub kind: SessionKind,
    /// For a desktop session: the SSH host whose connection carries it.
    /// Unset, an RDP session connects straight to the machine.
    pub via: Option<String>,
    /// Where the login password comes from.
    pub password: PasswordSource,
    /// For a serial session: speed, framing and flow control. The COM
    /// port is the session's `HostName`.
    pub serial: Option<LineSettings>,
    /// What to answer after the login: prompts to wait for, lines and
    /// passwords to type.
    pub login_script: Vec<Step>,
    /// Connect again on its own when the connection drops; the global
    /// setting when unset.
    pub auto_reconnect: Option<bool>,
    /// Keyword highlighting on or off for this host; the global setting
    /// when unset.
    pub highlight: Option<bool>,
    /// The program Session > Run remote app offers first (and what a
    /// remote app session runs), as a shell-like command line.
    pub remote_app: Option<String>,
    /// Copy the FeroStream agent to this host when it lacks it; the
    /// global setting when unset.
    pub remote_app_copy_agent: Option<bool>,
    /// Remove the copied agent from this host when its last remote app
    /// ends; the global setting when unset.
    pub remote_app_remove_agent: Option<bool>,
    /// Limits which way copied text travels between this host's remote
    /// apps and Windows: `to-host`, `from-host` or `off`. It only narrows
    /// the global setting (Session > Remote apps), never widens it.
    pub remote_app_clipboard: Option<String>,
    /// Limits which way copied text travels between this host's remote
    /// and virtual desktops and Windows: `to-host`, `from-host` or `off`.
    /// It only narrows the global `DesktopClipboard`, never widens it.
    pub desktop_clipboard: Option<String>,
    /// For a Mosh session: the server program to start over SSH;
    /// `mosh-server` when unset.
    pub mosh_server: Option<String>,
    /// For a Mosh session: the UDP port, or range of ports, the server
    /// may listen on (a firewall's opening); any free one in 60001 to
    /// 60999 when unset.
    pub mosh_ports: Option<(u16, u16)>,
    /// For a Mosh session: when typed text shows before the server
    /// echoes it, `always` or `never`; only on a slow link when unset.
    pub mosh_predict: Option<String>,
    /// For a WinRM session: the command its shell runs;
    /// [`DEFAULT_WINRM_COMMAND`] when unset.
    pub winrm_command: Option<String>,
    /// For a WinRM session: HTTPS (port [`DEFAULT_WINRM_HTTPS_PORT`])
    /// instead of HTTP with encrypted messages.
    pub winrm_https: bool,
}

/// The ways copied text can travel, as the clipboard settings spell
/// them: `both`, `to-host` (only from Windows to the host), `from-host`
/// (only from the host to Windows) and `off`. Unset means `off` for the
/// global settings (see [`global_clipboard_ways`]) and no limit (`both`)
/// on a connection.
pub const CLIPBOARD_WAYS: [&str; 4] = ["both", "to-host", "from-host", "off"];

/// The clipboard way `name` (one of [`CLIPBOARD_WAYS`], any case), or
/// `None`.
pub fn clipboard_way(name: &str) -> Option<String> {
    let w = name.to_ascii_lowercase();
    CLIPBOARD_WAYS.contains(&w.as_str()).then_some(w)
}

/// Which ways a connection's clipboard limit lets text travel: (to the
/// host, from the host). Unset or unknown limits nothing: both.
pub fn clipboard_ways(way: Option<&str>) -> (bool, bool) {
    match way.map(str::to_ascii_lowercase).as_deref() {
        Some("to-host") => (true, false),
        Some("from-host") => (false, true),
        Some("off") => (false, false),
        _ => (true, true),
    }
}

/// Which ways the global clipboard setting (`DesktopClipboard`,
/// `RemoteAppClipboard`) lets text travel: (to the host, from the host).
/// Unset or unknown is off, so copied text only crosses to or from a host
/// after the user chose that.
pub fn global_clipboard_ways(way: Option<&str>) -> (bool, bool) {
    match way.and_then(clipboard_way) {
        Some(w) => clipboard_ways(Some(&w)),
        None => (false, false),
    }
}

/// What opening a session does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionKind {
    /// An SSH login with a shell.
    #[default]
    Ssh,
    /// A Remote Desktop (RDP) connection in Remote Desktop Connection.
    Rdp,
    /// A VNC desktop in the built-in viewer, always through SSH.
    Vnc,
    /// A serial console on a COM port of this PC.
    Serial,
    /// A Telnet terminal, for old network gear and console servers:
    /// unencrypted, and shown as such.
    Telnet,
    /// A virtual desktop (Xvfb or Xvnc) started on the `Via` SSH host.
    Xvfb,
    /// A program on the `Via` SSH host whose windows show as native
    /// windows here (FeroStream).
    RemoteApp,
    /// A terminal over Mosh: SSH logs in and starts `mosh-server`, then
    /// the session goes over UDP, survives network changes and sleep.
    Mosh,
    /// A Windows Remote Shell (WinRM, WS-Management over HTTP): PowerShell
    /// or cmd on a Windows server, line by line, without a terminal.
    WinRm,
    /// File transfer only: SSH logs in without asking for a shell and
    /// File transfer opens on the host, for SFTP-only accounts
    /// (`ForceCommand internal-sftp`).
    Sftp,
}

/// How big a desktop is: as big as its pane, following it as it changes
/// where the server can, or a fixed size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSize {
    Follow,
    Fixed(u16, u16),
}

impl DesktopSize {
    /// `follow` (or `auto`, `window`) or `WIDTHxHEIGHT`, 320x200 to
    /// 8192x8192.
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().to_ascii_lowercase();
        if ["follow", "auto", "window", "pane"].contains(&t.as_str()) {
            return Some(DesktopSize::Follow);
        }
        let (w, h) = t.split_once(['x', '*'])?;
        let w: u16 = w.trim().parse().ok()?;
        let h: u16 = h.trim().parse().ok()?;
        ((320..=8192).contains(&w) && (200..=8192).contains(&h)).then_some(DesktopSize::Fixed(w, h))
    }

    /// The fixed size, `None` when it follows the pane.
    pub fn fixed(self) -> Option<(u16, u16)> {
        match self {
            DesktopSize::Follow => None,
            DesktopSize::Fixed(w, h) => Some((w, h)),
        }
    }
}

impl std::fmt::Display for DesktopSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DesktopSize::Follow => f.write_str("follow"),
            DesktopSize::Fixed(w, h) => write!(f, "{w}x{h}"),
        }
    }
}

/// Whether `program` can be the Mosh server: a program name or path,
/// nothing a shell would read as more.
pub fn valid_mosh_server(program: &str) -> bool {
    !program.is_empty()
        && program.len() <= 256
        && program.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'~' | b'+')
        })
}

/// A Mosh UDP port (`60001`) or range (`60001:60010`, or with `-`).
pub fn mosh_ports(text: &str) -> Option<(u16, u16)> {
    let text = text.trim();
    let (low, high) = text
        .split_once(':')
        .or_else(|| text.split_once('-'))
        .unwrap_or((text, text));
    let low: u16 = low.trim().parse().ok()?;
    let high: u16 = high.trim().parse().ok()?;
    (low != 0 && low <= high).then_some((low, high))
}

/// A Mosh echo prediction setting: `adaptive`, `always` or `never`,
/// in any case.
pub fn mosh_predict(text: &str) -> Option<&'static str> {
    ["adaptive", "always", "never"]
        .into_iter()
        .find(|w| w.eq_ignore_ascii_case(text.trim()))
}

/// How [`mosh_ports`] reads a port or range back.
pub fn mosh_ports_text((low, high): (u16, u16)) -> String {
    if low == high {
        low.to_string()
    } else {
        format!("{low}:{high}")
    }
}

impl SessionKind {
    pub fn name(self) -> &'static str {
        match self {
            SessionKind::Ssh => "ssh",
            SessionKind::Rdp => "rdp",
            SessionKind::Vnc => "vnc",
            SessionKind::Serial => "serial",
            SessionKind::Telnet => "telnet",
            SessionKind::Xvfb => "xvfb",
            SessionKind::RemoteApp => "remoteapp",
            SessionKind::Mosh => "mosh",
            SessionKind::WinRm => "winrm",
            SessionKind::Sftp => "sftp",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "ssh" => Some(SessionKind::Ssh),
            "rdp" => Some(SessionKind::Rdp),
            "vnc" => Some(SessionKind::Vnc),
            "serial" => Some(SessionKind::Serial),
            "telnet" => Some(SessionKind::Telnet),
            "xvfb" | "virtual" => Some(SessionKind::Xvfb),
            "remoteapp" | "app" | "ferostream" => Some(SessionKind::RemoteApp),
            "mosh" => Some(SessionKind::Mosh),
            "winrm" | "wsman" => Some(SessionKind::WinRm),
            "sftp" => Some(SessionKind::Sftp),
            _ => None,
        }
    }
}

/// A `Tmux` value that means no tmux at all, not a session of that
/// name: what the connection form shows when the field is empty.
pub fn tmux_off(name: &str) -> bool {
    ["", "off", "no", "none", "false"]
        .iter()
        .any(|w| name.trim().eq_ignore_ascii_case(w))
}

/// A `Tmux` value that means tmux on, attached to `main` (what Session >
/// Attach tmux uses), not a session of that name.
pub fn tmux_on(name: &str) -> bool {
    ["on", "yes", "true"]
        .iter()
        .any(|w| name.trim().eq_ignore_ascii_case(w))
}

/// A `StartDir` value as kept: trimmed, `None` when empty or holding
/// control characters, which must never be typed at a shell.
pub fn start_dir(dir: &str) -> Option<String> {
    let dir = dir.trim().trim_matches('"').trim();
    (!dir.is_empty() && !dir.chars().any(|c| c.is_control() || c == '"')).then(|| dir.to_owned())
}

/// A named pair of folders for File transfer: one on this PC, one on a
/// server (either may be missing), for one host or (`host` unset) any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bookmark {
    pub name: String,
    pub local: Option<String>,
    pub remote: Option<String>,
    pub host: Option<String>,
}

impl Bookmark {
    /// `Bookmark "name" "local" "remote" "host"`, an empty one for none.
    fn from_args(args: &[String]) -> Option<Self> {
        let get = |i: usize| args.get(i).and_then(|a| start_dir(a));
        let name = get(0)?;
        let b = Self {
            name,
            local: get(1),
            remote: get(2),
            host: get(3),
        };
        (b.local.is_some() || b.remote.is_some()).then_some(b)
    }

    /// The same name for the same host (names are matched in any case).
    pub fn same_name(&self, other: &Self) -> bool {
        self.name.eq_ignore_ascii_case(&other.name)
            && self.host.as_deref().map(str::to_ascii_lowercase)
                == other.host.as_deref().map(str::to_ascii_lowercase)
    }
}

/// A `VirtualDesktop` command as kept: trimmed, `None` when empty or
/// holding control characters (it travels as one line). Written back as
/// is, so quotes in it stay the shell's.
pub fn virtual_desktop(command: &str) -> Option<String> {
    let command = command.trim();
    (!command.is_empty() && !command.chars().any(char::is_control)).then(|| command.to_owned())
}

/// The RDP port xrdp and GNOME Remote Desktop listen on by default.
pub const DEFAULT_DESKTOP_PORT: u16 = 3389;
/// The VNC port of display :0.
pub const DEFAULT_VNC_PORT: u16 = 5900;
/// The Telnet port.
pub const DEFAULT_TELNET_PORT: u16 = 23;
/// The program a Mosh connection starts on the server when its profile
/// names none.
pub const DEFAULT_MOSH_SERVER: &str = "mosh-server";
/// WinRM over HTTP.
pub const DEFAULT_WINRM_PORT: u16 = 5985;
/// WinRM over HTTPS.
pub const DEFAULT_WINRM_HTTPS_PORT: u16 = 5986;
/// The command a WinRM session runs when its profile names none.
pub const DEFAULT_WINRM_COMMAND: &str = "powershell -NoLogo";

impl Profile {
    fn is_empty(&self) -> bool {
        self.font.is_none()
            && self.font_size.is_none()
            && self.theme.is_none()
            && self.scrollback.is_none()
            && self.tmux.is_none()
            && self.tmux_layout.is_none()
            && self.start_dir.is_none()
            && self.local_folder.is_none()
            && self.remote_folder.is_none()
            && !self.legacy_algorithms
            && self.desktop_port.is_none()
            && self.vnc_port.is_none()
            && self.virtual_desktop.is_none()
            && self.desktop_size.is_none()
            && self.kind == SessionKind::Ssh
            && self.via.is_none()
            && self.password == PasswordSource::Ask
            && self.serial.is_none()
            && self.login_script.is_empty()
            && self.auto_reconnect.is_none()
            && self.highlight.is_none()
            && self.remote_app.is_none()
            && self.remote_app_copy_agent.is_none()
            && self.remote_app_remove_agent.is_none()
            && self.remote_app_clipboard.is_none()
            && self.desktop_clipboard.is_none()
            && self.mosh_server.is_none()
            && self.mosh_ports.is_none()
            && self.mosh_predict.is_none()
            && self.winrm_command.is_none()
            && !self.winrm_https
    }
}

/// Where the window's own colours (menu bar, menus, title bar, tab strip
/// and tools pane) come from: Settings > Window colours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowColours {
    /// The focused pane's colour theme, dark or light as the theme is.
    Theme,
    /// Windows' own light or dark app mode.
    System,
    /// Windows' light look, as before window colours existed: the
    /// default, a light window around the terminal.
    #[default]
    Light,
    /// A neutral, very light grey (Windows 11's light window grey).
    SoftGrey,
    /// A neutral light grey.
    Silver,
    /// A neutral middle grey with black text.
    MidGrey,
    /// A neutral darker grey with white text.
    Steel,
    /// A neutral dark grey, lighter than `Dark`.
    Graphite,
    /// A neutral dark grey.
    Dark,
    /// A neutral near black.
    Black,
}

impl WindowColours {
    /// In menu order: the two that follow something, then the fixed looks
    /// from light to dark. At most ten (the menu ids' room).
    pub const ALL: [Self; 10] = [
        Self::Theme,
        Self::System,
        Self::Light,
        Self::SoftGrey,
        Self::Silver,
        Self::MidGrey,
        Self::Steel,
        Self::Graphite,
        Self::Dark,
        Self::Black,
    ];

    /// The word in the settings file.
    pub fn name(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::System => "windows",
            Self::Light => "light",
            Self::SoftGrey => "softgrey",
            Self::Silver => "silver",
            Self::MidGrey => "midgrey",
            Self::Steel => "steel",
            Self::Graphite => "graphite",
            Self::Dark => "dark",
            Self::Black => "black",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.name().eq_ignore_ascii_case(word))
    }
}

/// Everything in the settings file.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub profiles: Vec<Profile>,
    pub favourites: Vec<String>,
    /// User theme rules, tried before the built-in tag rules.
    pub theme_rules: Vec<ThemeRule>,
    /// Keyword highlighting in terminal output (Session > Highlight
    /// keywords). On unless turned off.
    pub highlight: bool,
    /// Use the standard highlighting rules after the user's. On unless
    /// turned off.
    pub highlight_standard: bool,
    /// The user's highlighting rules, tried before the standard ones.
    pub highlight_rules: Vec<HighlightRule>,
    /// Dock the side pane on the right instead of the left.
    pub side_pane_right: bool,
    /// Show the tools rail (an icon per tool) on the window's edge.
    pub tools_rail: bool,
    /// Keep an opened tool open; off, the terminal covers it again once
    /// the terminal is used (a peek).
    pub tools_pinned: bool,
    /// Where the window's own colours come from: its menu bar and menus,
    /// title bar, tab strip, tools rail and tools pane.
    pub window_colours: WindowColours,
    /// Show the tab strip when there are two or more tabs.
    pub tab_strip: bool,
    /// Show the server status bar (CPU, memory, disk and network of the
    /// focused pane's server) under the panes. Off unless turned on: it
    /// runs a command on the server every few seconds.
    pub status_bar: bool,
    pub copy_on_select: bool,
    pub right_click_paste: bool,
    /// Put the time in front of each line of plain session logs.
    pub log_timestamps: bool,
    /// Lines of scrollback for panes without a profile that sets it.
    pub scrollback: Option<usize>,
    /// Where downloads go; the user's Downloads folder when unset.
    pub download_folder: Option<String>,
    /// Compare SHA-256 on both sides after each transfer.
    pub verify_transfers: bool,
    /// Connect again on its own when an SSH connection drops, keeping the
    /// tab, its panes and tmux layout. On unless turned off.
    pub auto_reconnect: bool,
    /// Files transferred at once.
    pub parallel_transfers: Option<usize>,
    /// Answer `rz` and `sz` in the terminal with ZMODEM; off unless turned
    /// on, so their output shows as it is.
    pub zmodem: bool,
    /// The file view follows the shell's working directory (OSC 7).
    pub follow_directory: bool,
    /// How the file transfer pane arranges its Local and Remote lists,
    /// such as `side 50` or `stacked swapped 40`; side by side, Local
    /// first, when unset.
    pub transfer_layout: Option<String>,
    /// How the file transfer pane sorts each list, such as `local name
    /// ascending remote size descending`; by name, A to Z, when unset.
    pub transfer_sort: Option<String>,
    /// File transfer's named folder pairs, in the order made.
    pub bookmarks: Vec<Bookmark>,
    /// The X server forwarded X11 goes to (`DISPLAY` syntax); the `DISPLAY`
    /// environment variable, or `localhost:0`, when unset.
    pub x11_display: Option<String>,
    /// Look for a new FeroSSH release once a week (Help menu); off unless
    /// turned on.
    pub update_check_weekly: bool,
    /// Password manager commands for login script secrets; the rest are
    /// in the Windows Credential Manager.
    pub secrets: Vec<SecretSource>,
    /// Command snippets. They live in the snippets folder, a file each
    /// (see [`crate::snippet_store`]); `Snippet` blocks read from this
    /// file are from an older FeroSSH, to be moved there. Until they are
    /// (their `file` is unset), the file keeps writing them, so none is
    /// lost when the folder cannot be written.
    pub snippets: Vec<Snippet>,
    /// Ask before a snippet is typed into a pane with the Production
    /// theme. On unless turned off.
    pub confirm_production: bool,
    /// F11 switches full screen (Settings > F11 switches full screen). On
    /// unless turned off; off, F11 goes to the terminal as before.
    pub full_screen_key: bool,
    /// Snippets with a `When` text answer the host by themselves. On
    /// unless turned off.
    pub auto_responses: bool,
    /// Hosts a snippet run on picked hosts logs in to at once (1 to 16);
    /// `None` is the default, 4. The last number picked is kept.
    pub run_parallel: Option<usize>,
    /// In panes of a tmux session, the tmux prefix and a window or pane
    /// key work FeroSSH's tabs and panes. Off unless turned on.
    pub tmux_keys: bool,
    /// Earlier commands on the host (and a few common ones) show greyed
    /// after the cursor while typing; Enter keeps echoed command lines in
    /// an encrypted per-host history. Off unless turned on.
    pub command_suggestions: bool,
    /// A global hotkey hides and shows the FeroSSH window (Window >
    /// Standby window). Off unless turned on; the first window that opens
    /// takes the hotkey.
    pub standby: bool,
    /// The standby window's hotkey; [`Hotkey::DEFAULT`] when unset.
    pub standby_hotkey: Option<Hotkey>,
    /// Settings > Key bindings: the actions (by menu command id) whose
    /// shortcuts were changed, with the keys they have now (none: the
    /// action has no shortcut). Actions not listed keep their defaults.
    pub key_bindings: std::collections::BTreeMap<u32, Vec<Hotkey>>,
    /// The program whose window the standby hotkey shows and hides, by
    /// file name (`notepad.exe`); the FeroSSH window itself when unset.
    pub standby_target: Option<String>,
    /// Another program's window the standby hotkey hid, as a window
    /// handle, so that a FeroSSH that ended without showing it again
    /// shows it at its next start.
    pub standby_hidden: Option<u64>,
    /// Remote apps: copy the FeroStream agent to a host that lacks it (or
    /// has another version). On unless turned off.
    pub remote_app_copy_agent: bool,
    /// Remote apps: the copied agent deletes itself once the last remote
    /// app on that host ends. Off unless turned on.
    pub remote_app_remove_agent: bool,
    /// Where the agent is copied to on hosts (`~/...` or absolute);
    /// `~/.local/bin/ferostream-agent` when unset.
    pub remote_app_agent_path: Option<String>,
    /// Which way copied text travels between remote apps and Windows:
    /// `both` (when unset), `to-host`, `from-host` or `off`.
    pub remote_app_clipboard: Option<String>,
    /// Remote apps: text selected with the mouse on the host (X PRIMARY,
    /// xterm's way of copying) goes to Windows too. Off unless turned on.
    pub remote_app_share_selection: bool,
    /// Which way copied text travels between remote desktops (RDP, VNC,
    /// virtual desktops) and Windows: `both` (when unset), `to-host`,
    /// `from-host` or `off`.
    pub desktop_clipboard: Option<String>,
    /// Virtual desktops: text selected with the mouse there (X PRIMARY)
    /// goes to Windows too. Off unless turned on.
    pub desktop_share_selection: bool,
    /// How hard the agent packs window pictures: `auto` (when unset: from
    /// the link speed it measures), `fast` (LZ4) or `strong` (Deflate).
    pub remote_app_packing: Option<String>,
}

/// Scrollback is capped here, as the terminal caps it.
const MAX_SCROLLBACK: usize = 1_000_000;

fn lines(args: &[String]) -> Option<usize> {
    args.first()?
        .parse::<usize>()
        .ok()
        .map(|n| n.min(MAX_SCROLLBACK))
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            favourites: Vec::new(),
            theme_rules: Vec::new(),
            highlight: true,
            highlight_standard: true,
            highlight_rules: Vec::new(),
            side_pane_right: false,
            tools_rail: true,
            tools_pinned: true,
            window_colours: WindowColours::default(),
            tab_strip: true,
            status_bar: false,
            copy_on_select: false,
            right_click_paste: false,
            log_timestamps: false,
            scrollback: None,
            download_folder: None,
            verify_transfers: false,
            auto_reconnect: true,
            parallel_transfers: None,
            zmodem: false,
            follow_directory: true,
            transfer_layout: None,
            transfer_sort: None,
            bookmarks: Vec::new(),
            x11_display: None,
            update_check_weekly: false,
            secrets: Vec::new(),
            snippets: Vec::new(),
            confirm_production: true,
            full_screen_key: true,
            auto_responses: true,
            run_parallel: None,
            tmux_keys: false,
            command_suggestions: false,
            standby: false,
            standby_hotkey: None,
            key_bindings: std::collections::BTreeMap::new(),
            standby_target: None,
            standby_hidden: None,
            remote_app_copy_agent: true,
            remote_app_remove_agent: false,
            remote_app_agent_path: None,
            remote_app_clipboard: None,
            remote_app_share_selection: false,
            desktop_clipboard: None,
            desktop_share_selection: false,
            remote_app_packing: None,
        }
    }
}

/// Which block the parser is in.
enum Block {
    Top,
    Profile(usize),
    Rule(usize),
    Highlight(usize),
    Secret(usize),
    Snippet(usize),
}

fn yes(args: &[String]) -> Option<bool> {
    match args.first()?.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" => Some(true),
        "no" | "off" | "false" => Some(false),
        _ => None,
    }
}

/// What follows a line's keyword, as written (for a command line, whose
/// quotes mean something later).
fn raw_rest(line: &str) -> String {
    let line = line.trim();
    let end = line
        .find(|c: char| c.is_ascii_whitespace() || c == '=')
        .unwrap_or(line.len());
    let rest = line.get(end..).unwrap_or_default().trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    rest.trim().to_owned()
}

/// A line's text with control characters dropped and the ends trimmed,
/// so it stays one line of the file.
fn clean(text: &str) -> String {
    let kept: String = text.chars().filter(|c| !c.is_control()).collect();
    kept.trim().to_owned()
}

fn hex_rgb(text: &str) -> Option<[u8; 3]> {
    let text = text.strip_prefix('#').unwrap_or(text);
    if text.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(text, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Quotes an argument that holds spaces; drops characters that would
/// break the line.
fn arg(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .collect();
    if clean.is_empty() || clean.contains([' ', '\t', '=', '#']) {
        format!("\"{clean}\"")
    } else {
        clean
    }
}

/// A snippet as a `Snippet` block of the settings syntax: what a file in
/// the snippets folder holds (see [`crate::snippet_store`]). Empty for a
/// snippet without a name.
pub fn snippet_block(snippet: &Snippet) -> String {
    let mut out = String::new();
    let name = clean(&snippet.name);
    if name.is_empty() {
        return out;
    }
    let _ = writeln!(out, "Snippet {}", arg(&name));
    let hosts: Vec<String> = snippet
        .hosts
        .iter()
        .map(|h| arg(h))
        .filter(|h| !h.starts_with('"'))
        .collect();
    if !hosts.is_empty() {
        let _ = writeln!(out, "    Hosts {}", hosts.join(" "));
    }
    for f in &snippet.folders {
        let f = clean(f);
        if !f.is_empty() {
            let _ = writeln!(out, "    Folder {f}");
        }
    }
    if let Some(n) = snippet.hotkey {
        let _ = writeln!(out, "    Hotkey F{n}");
    }
    // A text that starts with `=` gets one of its own after the keyword,
    // since the reader takes one `=` there as the separator.
    let sep = |t: &str| if t.starts_with('=') { " = " } else { " " };
    let when = clean(&snippet.when);
    if !when.is_empty() {
        let _ = writeln!(out, "    When{}{when}", sep(&when));
    }
    for l in &snippet.lines {
        let l = clean(l);
        let _ = writeln!(out, "    Line{}{l}", sep(&l));
    }
    out
}

/// A highlighting rule as a `HighlightRule` block.
fn highlight_block(r: &HighlightRule) -> String {
    let mut out = String::from("HighlightRule\n");
    let words: Vec<String> = r.words.iter().map(|w| arg(w)).collect();
    if !words.is_empty() {
        let _ = writeln!(out, "    Words {}", words.join(" "));
    }
    if let Some(p) = &r.pattern {
        let p = clean(p);
        // A pattern that starts with `=` gets one of its own after the
        // keyword, since the reader takes one `=` there as the separator.
        let sep = if p.starts_with('=') { " = " } else { " " };
        let _ = writeln!(out, "    Pattern{sep}{p}");
    }
    if r.case_sensitive {
        out.push_str("    CaseSensitive yes\n");
    }
    if let Some(c) = r.colour {
        let _ = writeln!(out, "    Colour {}", c.text());
    }
    if let Some(c) = r.background {
        let _ = writeln!(out, "    Background {}", c.text());
    }
    if r.bold {
        out.push_str("    Bold yes\n");
    }
    if r.underline {
        out.push_str("    Underline yes\n");
    }
    let hosts: Vec<String> = r.hosts.iter().map(|h| arg(h)).collect();
    if !hosts.is_empty() {
        let _ = writeln!(out, "    Hosts {}", hosts.join(" "));
    }
    if r.stop {
        out.push_str("    Stop yes\n");
    }
    out
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut s = Settings::default();
        let mut block = Block::Top;
        for line in text.lines() {
            let Some((keyword, args)) = split_line(line) else {
                continue;
            };
            let first = args.first().cloned();
            // Font and theme names may hold spaces, quoted or not.
            let joined = args.join(" ");
            match keyword.as_str() {
                "profile" => {
                    let Some(host) = first else {
                        continue;
                    };
                    s.profiles.retain(|p| !p.host.eq_ignore_ascii_case(&host));
                    s.profiles.push(Profile {
                        host,
                        ..Profile::default()
                    });
                    block = Block::Profile(s.profiles.len() - 1);
                }
                "secret" => {
                    let Some(name) = first.filter(|n| valid_secret_name(n)) else {
                        block = Block::Top;
                        continue;
                    };
                    s.secrets.retain(|x| !x.name.eq_ignore_ascii_case(&name));
                    s.secrets.push(SecretSource {
                        name,
                        command: String::new(),
                    });
                    block = Block::Secret(s.secrets.len() - 1);
                }
                "snippet" => {
                    let name = clean(&joined);
                    if name.is_empty() {
                        block = Block::Top;
                        continue;
                    }
                    s.snippets.push(Snippet {
                        name,
                        ..Snippet::default()
                    });
                    block = Block::Snippet(s.snippets.len() - 1);
                }
                "highlightrule" => {
                    s.highlight_rules.push(HighlightRule::default());
                    block = Block::Highlight(s.highlight_rules.len() - 1);
                }
                "highlight" if matches!(block, Block::Profile(_)) => {
                    if let Block::Profile(i) = block
                        && let Some(p) = s.profiles.get_mut(i)
                    {
                        p.highlight = yes(&args);
                    }
                }
                "highlight" => {
                    block = Block::Top;
                    s.highlight = yes(&args).unwrap_or(true);
                }
                "highlightstandard" => {
                    block = Block::Top;
                    s.highlight_standard = yes(&args).unwrap_or(true);
                }
                "themerule" => {
                    s.theme_rules.push(ThemeRule::new(""));
                    block = Block::Rule(s.theme_rules.len() - 1);
                }
                "favourite" | "favorite" => {
                    block = Block::Top;
                    for host in args {
                        if !s.favourites.iter().any(|f| f.eq_ignore_ascii_case(&host)) {
                            s.favourites.push(host);
                        }
                    }
                }
                "sidepane" => {
                    block = Block::Top;
                    s.side_pane_right = first.is_some_and(|a| a.eq_ignore_ascii_case("right"));
                }
                "toolsrail" => {
                    block = Block::Top;
                    s.tools_rail = yes(&args).unwrap_or(true);
                }
                "toolspinned" => {
                    block = Block::Top;
                    s.tools_pinned = yes(&args).unwrap_or(true);
                }
                "windowcolours" => {
                    block = Block::Top;
                    s.window_colours = first
                        .as_deref()
                        .and_then(WindowColours::parse)
                        .unwrap_or_default();
                }
                "tabstrip" => {
                    block = Block::Top;
                    s.tab_strip = yes(&args).unwrap_or(true);
                }
                "statusbar" => {
                    block = Block::Top;
                    s.status_bar = yes(&args).unwrap_or(false);
                }
                "copyonselect" => {
                    block = Block::Top;
                    s.copy_on_select = yes(&args).unwrap_or(false);
                }
                "rightclickpaste" => {
                    block = Block::Top;
                    s.right_click_paste = yes(&args).unwrap_or(false);
                }
                "logtimestamps" => {
                    block = Block::Top;
                    s.log_timestamps = yes(&args).unwrap_or(false);
                }
                "downloadfolder" => {
                    block = Block::Top;
                    s.download_folder = Some(joined).filter(|f| !f.is_empty());
                }
                "verifytransfers" => {
                    block = Block::Top;
                    s.verify_transfers = yes(&args).unwrap_or(false);
                }
                "paralleltransfers" => {
                    block = Block::Top;
                    s.parallel_transfers = first
                        .and_then(|v| v.parse::<usize>().ok())
                        .map(|n| n.clamp(1, 8));
                }
                "zmodem" => {
                    block = Block::Top;
                    s.zmodem = yes(&args).unwrap_or(false);
                }
                "bookmark" => {
                    block = Block::Top;
                    if let Some(b) = Bookmark::from_args(&args) {
                        s.bookmarks.retain(|x| !x.same_name(&b));
                        s.bookmarks.push(b);
                    }
                }
                "transferlayout" => {
                    block = Block::Top;
                    s.transfer_layout = Some(joined).filter(|f| !f.is_empty());
                }
                "transfersort" => {
                    block = Block::Top;
                    s.transfer_sort = Some(joined).filter(|f| !f.is_empty());
                }
                "followdirectory" => {
                    block = Block::Top;
                    s.follow_directory = yes(&args).unwrap_or(true);
                }
                "updatecheck" => {
                    block = Block::Top;
                    s.update_check_weekly = first.is_some_and(|a| a.eq_ignore_ascii_case("weekly"));
                }
                "confirmproduction" => {
                    block = Block::Top;
                    s.confirm_production = yes(&args).unwrap_or(true);
                }
                "fullscreenkey" => {
                    block = Block::Top;
                    s.full_screen_key = yes(&args).unwrap_or(true);
                }
                "autoresponses" => {
                    block = Block::Top;
                    s.auto_responses = yes(&args).unwrap_or(true);
                }
                "tmuxkeybindings" => {
                    block = Block::Top;
                    s.tmux_keys = yes(&args).unwrap_or(false);
                }
                "commandsuggestions" => {
                    block = Block::Top;
                    s.command_suggestions = yes(&args).unwrap_or(false);
                }
                "standby" => {
                    block = Block::Top;
                    s.standby = yes(&args).unwrap_or(false);
                }
                "standbyhotkey" => {
                    block = Block::Top;
                    s.standby_hotkey = first.and_then(|k| Hotkey::parse(&k).ok());
                }
                "keybinding" => {
                    block = Block::Top;
                    // `KeyBinding <id> [key...]`; a key FeroSSH does not
                    // know, or one that would stop typing (Win too), is
                    // left out and the others kept. A line whose keys are
                    // all left out keeps the default; one with no keys
                    // takes the action's keys away.
                    if let Some(id) = first.and_then(|a| a.parse::<u32>().ok()) {
                        let named = args.len() > 1;
                        let keys: Vec<Hotkey> = args
                            .iter()
                            .skip(1)
                            .filter_map(|k| Hotkey::parse_key(k).ok())
                            .filter(|k| !k.win && !k.types_alone())
                            .collect();
                        if !named || !keys.is_empty() {
                            s.key_bindings.insert(id, keys);
                        }
                    }
                }
                "standbywindow" => {
                    block = Block::Top;
                    s.standby_target = first.filter(|p| !p.is_empty());
                }
                "standbyhidden" => {
                    block = Block::Top;
                    s.standby_hidden = first.and_then(|v| v.parse::<u64>().ok());
                }
                "runparallel" => {
                    block = Block::Top;
                    s.run_parallel = first
                        .and_then(|v| v.parse::<usize>().ok())
                        .map(|n| n.clamp(1, 16));
                }
                "x11display" => {
                    block = Block::Top;
                    s.x11_display = first.filter(|d| !d.is_empty());
                }
                "scrollback" if matches!(block, Block::Profile(_)) => {
                    if let Block::Profile(i) = block
                        && let Some(p) = s.profiles.get_mut(i)
                    {
                        p.scrollback = lines(&args);
                    }
                }
                "scrollback" => {
                    block = Block::Top;
                    s.scrollback = lines(&args);
                }
                "autoreconnect" if matches!(block, Block::Profile(_)) => {
                    if let Block::Profile(i) = block
                        && let Some(p) = s.profiles.get_mut(i)
                    {
                        p.auto_reconnect = yes(&args);
                    }
                }
                "autoreconnect" => {
                    block = Block::Top;
                    s.auto_reconnect = yes(&args).unwrap_or(true);
                }
                "remoteappcopyagent" if matches!(block, Block::Profile(_)) => {
                    if let Block::Profile(i) = block
                        && let Some(p) = s.profiles.get_mut(i)
                    {
                        p.remote_app_copy_agent = yes(&args);
                    }
                }
                "remoteappcopyagent" => {
                    block = Block::Top;
                    s.remote_app_copy_agent = yes(&args).unwrap_or(true);
                }
                "remoteappremoveagent" if matches!(block, Block::Profile(_)) => {
                    if let Block::Profile(i) = block
                        && let Some(p) = s.profiles.get_mut(i)
                    {
                        p.remote_app_remove_agent = yes(&args);
                    }
                }
                "remoteappremoveagent" => {
                    block = Block::Top;
                    s.remote_app_remove_agent = yes(&args).unwrap_or(false);
                }
                "remoteappagentpath" => {
                    block = Block::Top;
                    s.remote_app_agent_path = Some(joined).filter(|p| !p.is_empty());
                }
                "remoteappclipboard" if matches!(block, Block::Profile(_)) => {
                    if let Block::Profile(i) = block
                        && let Some(p) = s.profiles.get_mut(i)
                    {
                        // "both" restricts nothing: the same as unset.
                        p.remote_app_clipboard = first
                            .map(|w| w.to_ascii_lowercase())
                            .filter(|w| ["to-host", "from-host", "off"].contains(&w.as_str()));
                    }
                }
                "desktopclipboard" if matches!(block, Block::Profile(_)) => {
                    if let Block::Profile(i) = block
                        && let Some(p) = s.profiles.get_mut(i)
                    {
                        // "both" restricts nothing: the same as unset.
                        p.desktop_clipboard = first
                            .as_deref()
                            .and_then(clipboard_way)
                            .filter(|w| w != "both");
                    }
                }
                "desktopclipboard" => {
                    block = Block::Top;
                    s.desktop_clipboard = first.as_deref().and_then(clipboard_way);
                }
                "desktopshareselection" => {
                    block = Block::Top;
                    s.desktop_share_selection = yes(&args).unwrap_or(false);
                }
                "remoteappshareselection" => {
                    block = Block::Top;
                    s.remote_app_share_selection = yes(&args).unwrap_or(false);
                }
                "remoteappclipboard" => {
                    block = Block::Top;
                    s.remote_app_clipboard = first
                        .map(|w| w.to_ascii_lowercase())
                        .filter(|w| ["both", "to-host", "from-host", "off"].contains(&w.as_str()));
                }
                "remoteapppacking" => {
                    block = Block::Top;
                    s.remote_app_packing = first
                        .map(|w| w.to_ascii_lowercase())
                        .filter(|w| ["auto", "fast", "strong"].contains(&w.as_str()));
                }
                _ => match block {
                    Block::Profile(i) => {
                        let Some(p) = s.profiles.get_mut(i) else {
                            continue;
                        };
                        match keyword.as_str() {
                            "font" => p.font = Some(joined).filter(|f| !f.is_empty()),
                            "fontsize" => {
                                p.font_size = first
                                    .and_then(|v| v.parse::<f32>().ok())
                                    .filter(|v| (4.0..=96.0).contains(v));
                            }
                            "theme" => p.theme = Some(joined).filter(|t| !t.is_empty()),
                            "tmux" => {
                                p.tmux = first
                                    .filter(|t| !tmux_off(t))
                                    .map(|t| if tmux_on(&t) { "main".to_owned() } else { t });
                            }
                            "legacyalgorithms" => {
                                p.legacy_algorithms =
                                    first.is_some_and(|v| v.eq_ignore_ascii_case("yes"));
                            }
                            "desktopport" => {
                                p.desktop_port = first
                                    .and_then(|v| v.parse::<u16>().ok())
                                    .filter(|&v| v != 0);
                            }
                            "vncport" => {
                                p.vnc_port = first
                                    .and_then(|v| v.parse::<u16>().ok())
                                    .filter(|&v| v != 0);
                            }
                            "type" => {
                                p.kind = first
                                    .as_deref()
                                    .and_then(SessionKind::parse)
                                    .unwrap_or_default();
                            }
                            "passwordsource" => {
                                p.password = match first.as_deref() {
                                    Some(v) if v.eq_ignore_ascii_case("windows") => {
                                        PasswordSource::Windows
                                    }
                                    _ => PasswordSource::Ask,
                                };
                            }
                            // The rest of the line as written: quotes group
                            // the command's arguments.
                            "passwordcommand" => {
                                let raw = raw_rest(line);
                                p.password = if raw.is_empty() {
                                    PasswordSource::Ask
                                } else {
                                    PasswordSource::Command(raw)
                                };
                            }
                            "via" => {
                                p.via = first.filter(|v| crate::saved::valid_alias(v));
                            }
                            "serial" => p.serial = LineSettings::parse(&joined),
                            // As written: an expected prompt may hold
                            // quotes and runs of spaces.
                            "loginscript" => {
                                if let Ok(step) = Step::parse(&raw_rest(line)) {
                                    p.login_script.push(step);
                                }
                            }
                            "startdir" => p.start_dir = start_dir(&raw_rest(line)),
                            "localfolder" => p.local_folder = start_dir(&raw_rest(line)),
                            "remotefolder" => p.remote_folder = start_dir(&raw_rest(line)),
                            "desktopsize" => {
                                p.desktop_size = first.as_deref().and_then(DesktopSize::parse);
                            }
                            "virtualdesktop" => {
                                p.virtual_desktop = virtual_desktop(&raw_rest(line));
                            }
                            "remoteapp" => p.remote_app = virtual_desktop(&raw_rest(line)),
                            "moshserver" => {
                                p.mosh_server = first.filter(|v| valid_mosh_server(v));
                            }
                            "moshports" => p.mosh_ports = first.as_deref().and_then(mosh_ports),
                            "moshpredict" => {
                                p.mosh_predict = first
                                    .as_deref()
                                    .and_then(mosh_predict)
                                    .filter(|w| *w != "adaptive")
                                    .map(str::to_owned);
                            }
                            "winrmcommand" => {
                                p.winrm_command = virtual_desktop(&raw_rest(line));
                            }
                            "winrmhttps" => {
                                p.winrm_https =
                                    first.is_some_and(|v| v.eq_ignore_ascii_case("yes"));
                            }
                            "tmuxlayout" => {
                                p.tmux_layout = first
                                    .map(|l| l.to_ascii_lowercase())
                                    .filter(|l| l == "ferossh" || l == "tmux");
                            }
                            _ => {}
                        }
                    }
                    Block::Rule(i) => {
                        let Some(r) = s.theme_rules.get_mut(i) else {
                            continue;
                        };
                        match keyword.as_str() {
                            "hosts" => r.hosts.extend(args),
                            "tags" => r.tags.extend(args),
                            "theme" => r.theme = joined,
                            "tint" => {
                                let rgb = first.as_deref().and_then(hex_rgb);
                                let amount = args.get(1).and_then(|a| a.parse::<u8>().ok());
                                r.tint = rgb.zip(amount);
                            }
                            _ => {}
                        }
                    }
                    Block::Highlight(i) => {
                        let Some(r) = s.highlight_rules.get_mut(i) else {
                            continue;
                        };
                        match keyword.as_str() {
                            "words" => r.words.extend(args.into_iter().map(|w| clean(&w))),
                            // As written: backslashes and quotes are the
                            // pattern's own.
                            "pattern" => {
                                r.pattern = Some(clean(&raw_rest(line))).filter(|p| !p.is_empty());
                            }
                            "casesensitive" => r.case_sensitive = yes(&args).unwrap_or(false),
                            "colour" | "color" => {
                                r.colour = first.as_deref().and_then(HighlightColour::parse);
                            }
                            "background" => {
                                r.background = first.as_deref().and_then(HighlightColour::parse);
                            }
                            "bold" => r.bold = yes(&args).unwrap_or(false),
                            "underline" => r.underline = yes(&args).unwrap_or(false),
                            "hosts" => r.hosts.extend(args),
                            "stop" => r.stop = yes(&args).unwrap_or(false),
                            _ => {}
                        }
                    }
                    Block::Secret(i) => {
                        if keyword == "passwordcommand"
                            && let Some(x) = s.secrets.get_mut(i)
                        {
                            x.command = raw_rest(line);
                        }
                    }
                    Block::Snippet(i) => {
                        let Some(x) = s.snippets.get_mut(i) else {
                            continue;
                        };
                        match keyword.as_str() {
                            "line" => x.lines.push(clean(&raw_rest(line))),
                            "hosts" => x.hosts.extend(args),
                            "folder" if !joined.is_empty() => x.folders.push(clean(&joined)),
                            "hotkey" => x.hotkey = parse_hotkey(&joined).ok().flatten(),
                            "when" => x.when = clean(&raw_rest(line)),
                            _ => {}
                        }
                    }
                    Block::Top => {}
                },
            }
        }
        s.theme_rules
            .retain(|r| !r.theme.is_empty() && !(r.hosts.is_empty() && r.tags.is_empty()));
        s.highlight_rules
            .retain(|r| r.regex().is_some() && (r.styled() || r.stop));
        s.secrets.retain(|x| !x.command.is_empty());
        s
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut out = String::from(
            "# FeroSSH settings. FeroSSH rewrites this file; comments are not kept.\n",
        );
        let flag = |b: bool| if b { "yes" } else { "no" };
        let _ = writeln!(
            out,
            "SidePane {}",
            if self.side_pane_right {
                "right"
            } else {
                "left"
            }
        );
        let _ = writeln!(out, "ToolsRail {}", flag(self.tools_rail));
        let _ = writeln!(out, "ToolsPinned {}", flag(self.tools_pinned));
        let _ = writeln!(out, "WindowColours {}", self.window_colours.name());
        let _ = writeln!(out, "TabStrip {}", flag(self.tab_strip));
        let _ = writeln!(out, "StatusBar {}", flag(self.status_bar));
        let _ = writeln!(out, "CopyOnSelect {}", flag(self.copy_on_select));
        let _ = writeln!(out, "RightClickPaste {}", flag(self.right_click_paste));
        let _ = writeln!(out, "LogTimestamps {}", flag(self.log_timestamps));
        if let Some(n) = self.scrollback {
            let _ = writeln!(out, "Scrollback {n}");
        }
        if let Some(d) = &self.download_folder {
            let _ = writeln!(out, "DownloadFolder {}", arg(d));
        }
        let _ = writeln!(out, "VerifyTransfers {}", flag(self.verify_transfers));
        let _ = writeln!(out, "AutoReconnect {}", flag(self.auto_reconnect));
        let _ = writeln!(
            out,
            "UpdateCheck {}",
            if self.update_check_weekly {
                "weekly"
            } else {
                "off"
            }
        );
        if let Some(n) = self.parallel_transfers {
            let _ = writeln!(out, "ParallelTransfers {n}");
        }
        let _ = writeln!(out, "ZModem {}", flag(self.zmodem));
        let _ = writeln!(
            out,
            "RemoteAppCopyAgent {}",
            flag(self.remote_app_copy_agent)
        );
        let _ = writeln!(
            out,
            "RemoteAppRemoveAgent {}",
            flag(self.remote_app_remove_agent)
        );
        if let Some(p) = &self.remote_app_agent_path {
            let _ = writeln!(out, "RemoteAppAgentPath {}", arg(p));
        }
        if let Some(c) = &self.remote_app_clipboard {
            let _ = writeln!(out, "RemoteAppClipboard {c}");
        }
        let _ = writeln!(
            out,
            "RemoteAppShareSelection {}",
            flag(self.remote_app_share_selection)
        );
        if let Some(p) = &self.remote_app_packing {
            let _ = writeln!(out, "RemoteAppPacking {p}");
        }
        if let Some(c) = &self.desktop_clipboard {
            let _ = writeln!(out, "DesktopClipboard {c}");
        }
        let _ = writeln!(
            out,
            "DesktopShareSelection {}",
            flag(self.desktop_share_selection)
        );
        let _ = writeln!(out, "FollowDirectory {}", flag(self.follow_directory));
        if let Some(l) = &self.transfer_layout {
            let _ = writeln!(out, "TransferLayout {}", arg(l));
        }
        if let Some(l) = &self.transfer_sort {
            let _ = writeln!(out, "TransferSort {}", arg(l));
        }
        for b in &self.bookmarks {
            let quoted = |t: &str| format!("\"{}\"", clean(t).replace('"', ""));
            let opt = |t: &Option<String>| quoted(t.as_deref().unwrap_or_default());
            let _ = writeln!(
                out,
                "Bookmark {} {} {} {}",
                quoted(&b.name),
                opt(&b.local),
                opt(&b.remote),
                opt(&b.host)
            );
        }
        let _ = writeln!(out, "ConfirmProduction {}", flag(self.confirm_production));
        let _ = writeln!(out, "AutoResponses {}", flag(self.auto_responses));
        let _ = writeln!(out, "FullScreenKey {}", flag(self.full_screen_key));
        let _ = writeln!(out, "TmuxKeybindings {}", flag(self.tmux_keys));
        let _ = writeln!(out, "CommandSuggestions {}", flag(self.command_suggestions));
        let _ = writeln!(out, "Standby {}", flag(self.standby));
        if let Some(k) = self.standby_hotkey {
            let _ = writeln!(out, "StandbyHotkey {k}");
        }
        for (id, keys) in &self.key_bindings {
            let mut line = format!("KeyBinding {id}");
            for k in keys {
                let _ = write!(line, " {k}");
            }
            let _ = writeln!(out, "{line}");
        }
        if let Some(p) = &self.standby_target {
            let _ = writeln!(out, "StandbyWindow {}", arg(p));
        }
        if let Some(h) = self.standby_hidden {
            let _ = writeln!(out, "StandbyHidden {h}");
        }
        if let Some(n) = self.run_parallel {
            let _ = writeln!(out, "RunParallel {n}");
        }
        if let Some(d) = &self.x11_display {
            let _ = writeln!(out, "X11Display {}", arg(d));
        }
        for f in &self.favourites {
            let _ = writeln!(out, "Favourite {}", arg(f));
        }
        let _ = writeln!(out, "Highlight {}", flag(self.highlight));
        let _ = writeln!(out, "HighlightStandard {}", flag(self.highlight_standard));
        for r in &self.highlight_rules {
            out.push_str(&highlight_block(r));
        }
        for r in &self.theme_rules {
            out.push_str("ThemeRule\n");
            if !r.hosts.is_empty() {
                let hosts: Vec<String> = r.hosts.iter().map(|h| arg(h)).collect();
                let _ = writeln!(out, "    Hosts {}", hosts.join(" "));
            }
            if !r.tags.is_empty() {
                let tags: Vec<String> = r.tags.iter().map(|t| arg(t)).collect();
                let _ = writeln!(out, "    Tags {}", tags.join(" "));
            }
            let _ = writeln!(out, "    Theme {}", arg(&r.theme));
            if let Some(([red, green, blue], amount)) = r.tint {
                let _ = writeln!(out, "    Tint {red:02x}{green:02x}{blue:02x} {amount}");
            }
        }
        for p in self.profiles.iter().filter(|p| !p.is_empty()) {
            let _ = writeln!(out, "Profile {}", arg(&p.host));
            if let Some(f) = &p.font {
                let _ = writeln!(out, "    Font {}", arg(f));
            }
            if let Some(size) = p.font_size {
                let _ = writeln!(out, "    FontSize {size}");
            }
            if let Some(t) = &p.theme {
                let _ = writeln!(out, "    Theme {}", arg(t));
            }
            if let Some(n) = p.scrollback {
                let _ = writeln!(out, "    Scrollback {n}");
            }
            if let Some(t) = &p.tmux {
                let _ = writeln!(out, "    Tmux {}", arg(t));
            }
            if let Some(l) = &p.tmux_layout {
                let _ = writeln!(out, "    TmuxLayout {l}");
            }
            if let Some(d) = &p.start_dir {
                let _ = writeln!(out, "    StartDir {}", arg(d));
            }
            if let Some(d) = &p.local_folder {
                let _ = writeln!(out, "    LocalFolder {}", arg(d));
            }
            if let Some(d) = &p.remote_folder {
                let _ = writeln!(out, "    RemoteFolder {}", arg(d));
            }
            if p.legacy_algorithms {
                let _ = writeln!(out, "    LegacyAlgorithms yes");
            }
            if let Some(port) = p.desktop_port {
                let _ = writeln!(out, "    DesktopPort {port}");
            }
            if let Some(port) = p.vnc_port {
                let _ = writeln!(out, "    VncPort {port}");
            }
            if let Some(c) = &p.virtual_desktop {
                let _ = writeln!(out, "    VirtualDesktop {c}");
            }
            if let Some(size) = p.desktop_size {
                let _ = writeln!(out, "    DesktopSize {size}");
            }
            if let Some(c) = &p.desktop_clipboard {
                let _ = writeln!(out, "    DesktopClipboard {c}");
            }
            if p.kind != SessionKind::Ssh {
                let _ = writeln!(out, "    Type {}", p.kind.name());
            }
            if let Some(v) = &p.via {
                let _ = writeln!(out, "    Via {}", arg(v));
            }
            if let Some(line) = &p.serial {
                let _ = writeln!(out, "    Serial {line}");
            }
            for step in &p.login_script {
                let _ = writeln!(out, "    LoginScript {}", clean(&step.to_string()));
            }
            if let Some(on) = p.auto_reconnect {
                let _ = writeln!(out, "    AutoReconnect {}", if on { "yes" } else { "no" });
            }
            if let Some(on) = p.highlight {
                let _ = writeln!(out, "    Highlight {}", if on { "yes" } else { "no" });
            }
            if let Some(c) = &p.remote_app {
                let _ = writeln!(out, "    RemoteApp {c}");
            }
            if let Some(on) = p.remote_app_copy_agent {
                let _ = writeln!(
                    out,
                    "    RemoteAppCopyAgent {}",
                    if on { "yes" } else { "no" }
                );
            }
            if let Some(on) = p.remote_app_remove_agent {
                let _ = writeln!(
                    out,
                    "    RemoteAppRemoveAgent {}",
                    if on { "yes" } else { "no" }
                );
            }
            if let Some(c) = &p.remote_app_clipboard {
                let _ = writeln!(out, "    RemoteAppClipboard {c}");
            }
            if let Some(m) = &p.mosh_server {
                let _ = writeln!(out, "    MoshServer {}", arg(m));
            }
            if let Some(ports) = p.mosh_ports {
                let _ = writeln!(out, "    MoshPorts {}", mosh_ports_text(ports));
            }
            if let Some(w) = &p.mosh_predict {
                let _ = writeln!(out, "    MoshPredict {w}");
            }
            if let Some(c) = &p.winrm_command {
                let _ = writeln!(out, "    WinrmCommand {c}");
            }
            if p.winrm_https {
                let _ = writeln!(out, "    WinrmHttps yes");
            }
            match &p.password {
                PasswordSource::Ask => {}
                PasswordSource::Windows => out.push_str("    PasswordSource windows\n"),
                PasswordSource::Command(c) => {
                    let line: String = c.chars().filter(|c| !c.is_control()).collect();
                    let _ = writeln!(out, "    PasswordCommand {}", line.trim());
                }
            }
        }
        for x in self.secrets.iter().filter(|x| valid_secret_name(&x.name)) {
            let _ = writeln!(out, "Secret {}", x.name);
            let _ = writeln!(out, "    PasswordCommand {}", clean(&x.command));
        }
        for snippet in self.snippets.iter().filter(|x| x.file.is_none()) {
            out.push_str(&snippet_block(snippet));
        }
        out
    }

    pub fn profile(&self, host: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.host.eq_ignore_ascii_case(host))
    }

    /// The host's profile, made if missing.
    pub fn profile_mut(&mut self, host: &str) -> &mut Profile {
        let i = match self
            .profiles
            .iter()
            .position(|p| p.host.eq_ignore_ascii_case(host))
        {
            Some(i) => i,
            None => {
                self.profiles.push(Profile {
                    host: host.to_owned(),
                    ..Profile::default()
                });
                self.profiles.len() - 1
            }
        };
        // `i` is in range: found above or just pushed.
        #[allow(clippy::indexing_slicing)]
        &mut self.profiles[i]
    }

    pub fn is_favourite(&self, host: &str) -> bool {
        self.favourites.iter().any(|f| f.eq_ignore_ascii_case(host))
    }

    /// Adds or removes a favourite; returns whether it is one now.
    pub fn toggle_favourite(&mut self, host: &str) -> bool {
        if self.is_favourite(host) {
            self.favourites.retain(|f| !f.eq_ignore_ascii_case(host));
            false
        } else {
            self.favourites.push(host.to_owned());
            true
        }
    }

    /// The theme rules: the user's, then the built-in tag rules.
    pub fn host_themes(&self) -> crate::HostThemes {
        let mut themes = crate::HostThemes::new();
        for r in &self.theme_rules {
            themes.push(r.clone());
        }
        themes
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const TEXT: &str = r#"
# comment
SidePane right
TabStrip no
Scrollback 50000
DownloadFolder "D:\My Transfers"
VerifyTransfers yes
ZModem yes
AutoReconnect no
ParallelTransfers 20
FollowDirectory no
TransferLayout stacked swapped 40
TransferSort local name ascending remote size descending
Bookmark "nginx config" "C:\Users\j\Documents" /etc/nginx "web01.prod"
Bookmark logs "" /var/log ""
Bookmark empty "" "" ""
X11Display 192.168.1.5:0.0
Favourite web "db one"
Favorite WEB
ThemeRule
    Hosts prod-* *.prod.example.com
    Tags production
    Theme Production
    Tint #800000 60
ThemeRule
    Theme Nord
Profile web
    Font "Cascadia Mono"
    FontSize 12.5
    Theme Dracula
    Scrollback 99999999
    Tmux work
    TmuxLayout TMUX
    StartDir "/srv/my site"
    LocalFolder "D:\Web site"
    RemoteFolder /var/www
    LegacyAlgorithms YES
    AutoReconnect yes
    DesktopPort 3390
    VncPort 5901
    VirtualDesktop dbus-run-session  "startxfce4"  
    DesktopSize 1280X720
    Type RDP
    Via jump-1
    PasswordCommand op read "op://Private/web 1/password"
    Unknown thing
Profile big
    FontSize 400
    Theme Solarized Dark
    Tmux no
    TmuxLayout split
    LegacyAlgorithms maybe
    DesktopPort 0
    Type gopher
    Via "a b"
    PasswordSource Windows
Profile console
    Type Serial
    Serial 115200,7E1,rtscts
    LoginScript expect   Username:  
    LoginScript send admin "quoted"
    LoginScript password
    LoginScript expect #
    LoginScript secret enable
    LoginScript bogus step
Secret enable
    PasswordCommand op read "op://Network/%n/password"
Secret "bad name"
    PasswordCommand never
Secret unused
Snippet Show interfaces
    Line terminal length 0
    Line show ip int brief | include up
Snippet
    Line lost
Snippet "Next page"
    Hosts sw-* core-?
    Folder Lab gear
    Hotkey f3
    When  --More-- 
    Line @key space
Snippet "Bad key"
    Hotkey F13
    Line x
AutoResponses no
RunParallel 40
Profile router
    Type Telnet
    Tmux On
Profile debian2
    Tmux OFF
    Theme Monokai
    StartDir
NewKeyword later
"#;

    #[test]
    fn parses() {
        let s = Settings::parse(TEXT);
        assert!(s.side_pane_right);
        assert!(!s.tab_strip);
        assert_eq!(s.favourites, ["web", "db one"]);
        assert_eq!(
            s.theme_rules.len(),
            1,
            "a rule without hosts or tags is dropped"
        );
        let r = &s.theme_rules[0];
        assert_eq!(r.hosts, ["prod-*", "*.prod.example.com"]);
        assert_eq!(r.tint, Some(([0x80, 0, 0], 60)));
        let web = s.profile("WEB").unwrap();
        assert_eq!(web.font.as_deref(), Some("Cascadia Mono"));
        assert_eq!(web.font_size, Some(12.5));
        assert_eq!(web.theme.as_deref(), Some("Dracula"));
        assert_eq!(web.scrollback, Some(1_000_000));
        assert_eq!(web.tmux.as_deref(), Some("work"));
        assert_eq!(web.tmux_layout.as_deref(), Some("tmux"));
        assert!(web.legacy_algorithms);
        assert_eq!(web.desktop_port, Some(3390));
        assert_eq!(web.vnc_port, Some(5901));
        assert_eq!(
            web.virtual_desktop.as_deref(),
            Some(r#"dbus-run-session  "startxfce4""#)
        );
        assert_eq!(web.desktop_size, Some(DesktopSize::Fixed(1280, 720)));
        assert_eq!(DesktopSize::parse("Follow"), Some(DesktopSize::Follow));
        assert_eq!(DesktopSize::parse("100x100"), None);
        assert_eq!(
            DesktopSize::parse("1920 x 1080"),
            Some(DesktopSize::Fixed(1920, 1080))
        );
        assert_eq!(web.kind, SessionKind::Rdp);
        assert_eq!(web.via.as_deref(), Some("jump-1"));
        assert_eq!(
            web.password,
            PasswordSource::Command(r#"op read "op://Private/web 1/password""#.into())
        );
        assert_eq!(s.scrollback, Some(50_000));
        assert_eq!(s.download_folder.as_deref(), Some("D:\\My Transfers"));
        assert!(s.zmodem);
        assert!(!Settings::default().zmodem);
        assert!(s.verify_transfers);
        assert!(!s.auto_reconnect);
        assert_eq!(web.auto_reconnect, Some(true));
        assert!(Settings::default().auto_reconnect, "on unless turned off");
        assert_eq!(s.parallel_transfers, Some(8));
        assert!(!s.follow_directory);
        assert_eq!(s.transfer_layout.as_deref(), Some("stacked swapped 40"));
        assert_eq!(
            s.transfer_sort.as_deref(),
            Some("local name ascending remote size descending")
        );
        assert_eq!(
            s.bookmarks,
            [
                Bookmark {
                    name: "nginx config".into(),
                    local: Some("C:\\Users\\j\\Documents".into()),
                    remote: Some("/etc/nginx".into()),
                    host: Some("web01.prod".into()),
                },
                Bookmark {
                    name: "logs".into(),
                    local: None,
                    remote: Some("/var/log".into()),
                    host: None,
                },
            ]
        );
        assert_eq!(Settings::parse(&s.to_text()).bookmarks, s.bookmarks);
        assert_eq!(s.x11_display.as_deref(), Some("192.168.1.5:0.0"));
        let big = s.profile("big").unwrap();
        assert_eq!(big.font_size, None);
        assert_eq!(big.theme.as_deref(), Some("Solarized Dark"));
        assert_eq!(big.tmux, None, "tmux stays off unless a session is named");
        assert_eq!(big.tmux_layout, None);
        assert_eq!(big.start_dir, None);
        assert_eq!(web.start_dir.as_deref(), Some("/srv/my site"));
        assert_eq!(web.local_folder.as_deref(), Some("D:\\Web site"));
        assert_eq!(web.remote_folder.as_deref(), Some("/var/www"));
        assert_eq!(
            s.profile("router").unwrap().tmux.as_deref(),
            Some("main"),
            "on is tmux on, not a session named on"
        );
        let debian = s.profile("debian2").unwrap();
        assert_eq!(debian.tmux, None, "off is no tmux, not a session named off");
        assert_eq!(debian.start_dir, None);
        assert!(!big.legacy_algorithms, "only `yes` turns it on");
        assert_eq!(big.desktop_port, None, "port 0 is no port");
        assert_eq!(big.kind, SessionKind::Ssh);
        assert_eq!(big.via, None);
        assert_eq!(big.password, PasswordSource::Windows);
        assert_eq!(big.auto_reconnect, None);
        let console = s.profile("console").unwrap();
        assert_eq!(console.kind, SessionKind::Serial);
        assert_eq!(
            console.serial.map(|l| l.to_string()).as_deref(),
            Some("115200,7E1,rtscts")
        );
        assert_eq!(s.profile("router").unwrap().kind, SessionKind::Telnet);
        assert_eq!(
            console.login_script,
            [
                Step::Expect("Username:".into()),
                Step::Send("admin \"quoted\"".into()),
                Step::Password,
                Step::Expect("#".into()),
                Step::Secret("enable".into()),
            ]
        );
        assert_eq!(
            s.secrets,
            [SecretSource {
                name: "enable".into(),
                command: r#"op read "op://Network/%n/password""#.into(),
            }],
            "only valid names with a command are kept"
        );
        assert_eq!(
            s.snippets,
            [
                Snippet {
                    name: "Show interfaces".into(),
                    lines: vec![
                        "terminal length 0".into(),
                        "show ip int brief | include up".into()
                    ],
                    ..Snippet::default()
                },
                Snippet {
                    name: "Next page".into(),
                    lines: vec!["@key space".into()],
                    hosts: vec!["sw-*".into(), "core-?".into()],
                    folders: vec!["Lab gear".into()],
                    hotkey: Some(3),
                    when: "--More--".into(),
                    ..Snippet::default()
                },
                Snippet {
                    name: "Bad key".into(),
                    lines: vec!["x".into()],
                    ..Snippet::default()
                },
            ]
        );
        assert!(!s.auto_responses);
        assert!(s.confirm_production, "on unless turned off");
        assert!(Settings::default().auto_responses);
        assert_eq!(s.run_parallel, Some(16), "at most 16");
        assert_eq!(Settings::default().run_parallel, None);
    }

    #[test]
    fn highlight_rules_round_trip() {
        let text = r#"
HighlightStandard no
HighlightRule
    Words error "permission denied" a#b
    Colour bright-red
    Bold yes
HighlightRule
    Pattern =\bOOM "killer"\b
    CaseSensitive yes
    Colour ff8000
    Background 4
    Underline yes
    Hosts db-* !db-test
    Stop yes
HighlightRule
    Words nothing-to-show
HighlightRule
    Words just-stop
    Stop yes
Profile lab
    Highlight no
"#;
        let s = Settings::parse(text);
        assert!(s.highlight);
        assert!(!s.highlight_standard);
        assert_eq!(
            s.highlight_rules.len(),
            3,
            "a rule that shows nothing is dropped"
        );
        let r = &s.highlight_rules[0];
        assert_eq!(r.words, ["error", "permission denied", "a#b"]);
        assert_eq!(r.colour, Some(HighlightColour::Index(9)));
        let r = &s.highlight_rules[1];
        assert_eq!(r.pattern.as_deref(), Some(r#"\bOOM "killer"\b"#));
        assert_eq!(r.background, Some(HighlightColour::Index(4)));
        assert!(r.stop && r.underline && r.case_sensitive);
        assert_eq!(s.profile("lab").unwrap().highlight, Some(false));
        let again = Settings::parse(&s.to_text());
        assert_eq!(again, s);
        let off = Settings::parse("Highlight no\n");
        assert!(!Settings::parse(&off.to_text()).highlight);
    }

    #[test]
    fn round_trips() {
        let s = Settings::parse(TEXT);
        let again = Settings::parse(&s.to_text());
        assert_eq!(again, s);
        // Snippets in the snippets folder stay out of the file.
        let mut moved = s.clone();
        for x in &mut moved.snippets {
            x.file = Some(format!("{}.snippet", x.name));
        }
        assert!(Settings::parse(&moved.to_text()).snippets.is_empty());
        assert_eq!(Settings::parse(""), Settings::default());
    }

    #[test]
    fn winrm_settings() {
        let s = Settings::parse(
            "Profile a\n    Type winrm\n    WinrmCommand cmd /k \"echo hi\"\n    WinrmHttps yes\nProfile b\n    Type WinRM\n",
        );
        let a = s.profile("a").unwrap();
        assert_eq!(a.kind, SessionKind::WinRm);
        assert_eq!(a.winrm_command.as_deref(), Some("cmd /k \"echo hi\""));
        assert!(a.winrm_https);
        let b = s.profile("b").unwrap();
        assert!(!b.winrm_https);
        assert_eq!(
            (b.kind, b.winrm_command.as_deref()),
            (SessionKind::WinRm, None)
        );
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn mosh_settings() {
        let s = Settings::parse(
            "Profile a\n    Type mosh\n    MoshServer /opt/mosh/bin/mosh-server\n    MoshPorts 60001-60010\n\
             \x20   MoshPredict Never\n\
             Profile b\n    Type mosh\n    MoshServer \"mosh-server; rm\"\n    MoshPorts 70000\n\
             \x20   MoshPredict sometimes\n",
        );
        let a = s.profile("a").unwrap();
        assert_eq!(a.kind, SessionKind::Mosh);
        assert_eq!(a.mosh_server.as_deref(), Some("/opt/mosh/bin/mosh-server"));
        assert_eq!(a.mosh_ports, Some((60001, 60010)));
        assert_eq!(a.mosh_predict.as_deref(), Some("never"));
        let b = s.profile("b").unwrap();
        assert_eq!(b.kind, SessionKind::Mosh);
        assert_eq!(b.mosh_server, None, "not a program name");
        assert_eq!(b.mosh_ports, None);
        assert_eq!(b.mosh_predict, None, "not a setting");
        assert_eq!(Settings::parse(&s.to_text()), s);
        assert_eq!(mosh_ports("60001"), Some((60001, 60001)));
        assert_eq!(mosh_ports("60010:60001"), None);
        assert_eq!(mosh_ports("0"), None);
        assert_eq!(mosh_ports_text((60001, 60010)), "60001:60010");
    }

    #[test]
    fn sftp_only_settings() {
        let s = Settings::parse("Profile up\n    Type SFTP\n    RemoteFolder /incoming\n");
        let p = s.profile("up").unwrap();
        assert_eq!(p.kind, SessionKind::Sftp);
        assert_eq!(p.remote_folder.as_deref(), Some("/incoming"));
        assert!(s.to_text().contains("    Type sftp\n"));
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn remote_app_settings_global_and_per_connection() {
        let d = Settings::default();
        assert!(d.remote_app_copy_agent, "copying is on unless turned off");
        assert!(
            !d.remote_app_remove_agent,
            "removal is off unless turned on"
        );
        let s = Settings::parse(
            "RemoteAppCopyAgent no\nRemoteAppRemoveAgent yes\n\
             RemoteAppAgentPath ~/apps/ferostream-agent\nRemoteAppClipboard to-host\n\
             RemoteAppPacking Strong\nRemoteAppShareSelection yes\n\
             Profile gimp-box\n    Type app\n    Via build\n    RemoteApp gimp \"a b.png\"\n\
             \x20   RemoteAppCopyAgent yes\n    RemoteAppRemoveAgent no\n\
             \x20   RemoteAppClipboard Off\n",
        );
        assert!(s.remote_app_share_selection);
        assert!(!d.remote_app_share_selection, "selections stay on the host");
        assert!(!s.remote_app_copy_agent);
        assert!(s.remote_app_remove_agent);
        assert_eq!(
            s.remote_app_agent_path.as_deref(),
            Some("~/apps/ferostream-agent")
        );
        assert_eq!(s.remote_app_clipboard.as_deref(), Some("to-host"));
        assert_eq!(s.remote_app_packing.as_deref(), Some("strong"));
        assert_eq!(
            Settings::parse("RemoteAppPacking hard\n").remote_app_packing,
            None
        );
        let p = s.profile("gimp-box").unwrap();
        assert_eq!(p.kind, SessionKind::RemoteApp);
        assert_eq!(p.via.as_deref(), Some("build"));
        assert_eq!(p.remote_app.as_deref(), Some("gimp \"a b.png\""));
        assert_eq!(p.remote_app_copy_agent, Some(true));
        assert_eq!(p.remote_app_remove_agent, Some(false));
        assert_eq!(p.remote_app_clipboard.as_deref(), Some("off"));
        assert_eq!(s.remote_app_clipboard.as_deref(), Some("to-host"));
        assert_eq!(Settings::parse(&s.to_text()), s);
        assert_eq!(
            Settings::parse("RemoteAppClipboard sideways\n").remote_app_clipboard,
            None
        );
    }

    #[test]
    fn desktop_clipboard_global_and_per_connection() {
        let d = Settings::default();
        assert_eq!(d.desktop_clipboard, None, "off unless changed");
        assert!(!d.desktop_share_selection);
        assert_eq!(
            global_clipboard_ways(d.desktop_clipboard.as_deref()),
            (false, false)
        );
        // A connection without a limit does not narrow it.
        assert_eq!(clipboard_ways(None), (true, true));
        let s = Settings::parse(
            "DesktopClipboard Off
DesktopShareSelection yes
             Profile lab
    Type xvfb
    Via build
    DesktopClipboard TO-HOST
             Profile other
    DesktopClipboard both
",
        );
        assert_eq!(s.desktop_clipboard.as_deref(), Some("off"));
        assert!(s.desktop_share_selection);
        assert_eq!(
            s.profile("lab").unwrap().desktop_clipboard.as_deref(),
            Some("to-host")
        );
        // "both" limits nothing: not kept on a connection.
        assert!(
            s.profile("other")
                .is_none_or(|p| p.desktop_clipboard.is_none())
        );
        let again = Settings::parse(&s.to_text());
        assert_eq!(again.desktop_clipboard, s.desktop_clipboard);
        assert_eq!(again.profile("lab"), s.profile("lab"));
        assert!(again.desktop_share_selection);
        assert_eq!(
            Settings::parse(
                "DesktopClipboard sideways
"
            )
            .desktop_clipboard,
            None
        );
        assert_eq!(clipboard_ways(Some("to-host")), (true, false));
        assert_eq!(clipboard_ways(Some("from-host")), (false, true));
        assert_eq!(clipboard_ways(Some("off")), (false, false));
        assert_eq!(clipboard_ways(Some("nonsense")), (true, true));
        assert_eq!(global_clipboard_ways(Some("Both")), (true, true));
        assert_eq!(global_clipboard_ways(Some("to-host")), (true, false));
        assert_eq!(global_clipboard_ways(Some("nonsense")), (false, false));
        // Chosen "both" is kept globally, so it survives a save.
        let both = Settings::parse("DesktopClipboard both\nRemoteAppClipboard both\n");
        assert_eq!(both.desktop_clipboard.as_deref(), Some("both"));
        assert_eq!(Settings::parse(&both.to_text()), both);
    }

    #[test]
    fn update_checks_are_off_unless_weekly() {
        assert!(!Settings::default().update_check_weekly);
        assert!(Settings::parse("UpdateCheck weekly\n").update_check_weekly);
        assert!(!Settings::parse("UpdateCheck off\n").update_check_weekly);
        assert!(!Settings::parse("UpdateCheck daily\n").update_check_weekly);
        let s = Settings {
            update_check_weekly: true,
            ..Settings::default()
        };
        assert!(Settings::parse(&s.to_text()).update_check_weekly);
    }

    #[test]
    fn tools_rail_and_pin_are_on_unless_turned_off() {
        let d = Settings::default();
        assert!(d.tools_rail && d.tools_pinned);
        // An older settings file without them keeps both on.
        let old = Settings::parse("SidePane left\n");
        assert!(old.tools_rail && old.tools_pinned);
        let s = Settings::parse("ToolsRail no\nToolsPinned no\n");
        assert!(!s.tools_rail && !s.tools_pinned);
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn status_bar_is_off_unless_turned_on() {
        assert!(!Settings::default().status_bar);
        assert!(!Settings::parse("SidePane left\n").status_bar);
        let s = Settings::parse("StatusBar yes\n");
        assert!(s.status_bar);
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn window_colours_are_light_unless_set() {
        assert_eq!(Settings::default().window_colours, WindowColours::Light);
        assert_eq!(
            Settings::parse("SidePane left\n").window_colours,
            WindowColours::Light
        );
        for c in WindowColours::ALL {
            let s = Settings {
                window_colours: c,
                ..Settings::default()
            };
            assert_eq!(Settings::parse(&s.to_text()), s);
        }
        // An unknown word (a newer FeroSSH's) falls back to the default.
        assert_eq!(
            Settings::parse("WindowColours purple\n").window_colours,
            WindowColours::Light
        );
        assert_eq!(
            Settings::parse("WindowColours theme\n").window_colours,
            WindowColours::Theme
        );
        assert_eq!(
            Settings::parse("WindowColours Dark\n").window_colours,
            WindowColours::Dark
        );
        assert_eq!(
            Settings::parse("WindowColours midgrey\n").window_colours,
            WindowColours::MidGrey
        );
        // Every look has its own word.
        let mut names: Vec<_> = WindowColours::ALL.iter().map(|c| c.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), WindowColours::ALL.len());
    }

    #[test]
    fn key_bindings_round_trip() {
        assert!(Settings::default().key_bindings.is_empty());
        let s = Settings::parse(
            "KeyBinding 213 Ctrl+K Ctrl+Shift+A\nKeyBinding 500\nKeyBinding 501 Bogus+Q F5\n\
             KeyBinding x Ctrl+A\nKeyBinding 502 Bogus+Q\nKeyBinding 503 A Num1 Enter\n\
             KeyBinding 504 Shift+PageUp F11 Shift+A\n",
        );
        let key = |t| Hotkey::parse_key(t).unwrap();
        assert_eq!(
            s.key_bindings.get(&213),
            Some(&vec![key("Ctrl+K"), key("Ctrl+Shift+A")])
        );
        // No keys: the action has none.
        assert_eq!(s.key_bindings.get(&500), Some(&vec![]));
        // An unknown key is left out, the others kept.
        assert_eq!(s.key_bindings.get(&501), Some(&vec![key("F5")]));
        // Keys all unknown or typing keys: the default stays.
        assert!(!s.key_bindings.contains_key(&502));
        assert!(!s.key_bindings.contains_key(&503));
        assert_eq!(
            s.key_bindings.get(&504),
            Some(&vec![key("Shift+PageUp"), key("F11")])
        );
        assert_eq!(s.key_bindings.len(), 4);
        assert_eq!(Settings::parse(&s.to_text()).key_bindings, s.key_bindings);
    }

    #[test]
    fn log_timestamps_are_off_unless_turned_on() {
        assert!(!Settings::default().log_timestamps);
        assert!(Settings::parse("LogTimestamps yes\n").log_timestamps);
        let s = Settings {
            log_timestamps: true,
            ..Settings::default()
        };
        assert!(Settings::parse(&s.to_text()).log_timestamps);
    }

    #[test]
    fn standby_is_off_unless_turned_on_and_keeps_its_hotkey() {
        let d = Settings::default();
        assert!(!d.standby);
        assert_eq!(d.standby_hotkey, None);
        let s = Settings::parse("Standby yes\nStandbyHotkey ctrl+alt+space\n");
        assert!(s.standby);
        let key = Hotkey::parse("Ctrl+Alt+Space").ok();
        assert_eq!(s.standby_hotkey, key);
        let back = Settings::parse(&s.to_text());
        assert!(back.standby);
        assert_eq!(back.standby_hotkey, key);
        // A hotkey FeroSSH refuses is dropped, so the default applies.
        assert_eq!(Settings::parse("StandbyHotkey A\n").standby_hotkey, None);
        assert!(!Settings::parse("Standby maybe\n").standby);
        let s = Settings::parse("StandbyWindow \"my app.exe\"\nStandbyHidden 132456\n");
        assert_eq!(s.standby_target.as_deref(), Some("my app.exe"));
        assert_eq!(s.standby_hidden, Some(132_456));
        let back = Settings::parse(&s.to_text());
        assert_eq!(back.standby_target, s.standby_target);
        assert_eq!(back.standby_hidden, s.standby_hidden);
        assert_eq!(d.standby_target, None);
    }

    #[test]
    fn tmux_keybindings_are_off_unless_turned_on() {
        assert!(!Settings::default().tmux_keys);
        assert!(Settings::parse("TmuxKeybindings yes\n").tmux_keys);
        assert!(!Settings::parse("TmuxKeybindings maybe\n").tmux_keys);
        let s = Settings {
            tmux_keys: true,
            ..Settings::default()
        };
        assert!(Settings::parse(&s.to_text()).tmux_keys);
    }

    #[test]
    fn command_suggestions_are_off_unless_turned_on() {
        assert!(!Settings::default().command_suggestions);
        assert!(Settings::parse("CommandSuggestions yes\n").command_suggestions);
        assert!(!Settings::parse("CommandSuggestions maybe\n").command_suggestions);
        let s = Settings {
            command_suggestions: true,
            ..Settings::default()
        };
        assert!(Settings::parse(&s.to_text()).command_suggestions);
    }

    #[test]
    fn full_screen_key_is_on_unless_turned_off() {
        assert!(Settings::default().full_screen_key);
        assert!(Settings::parse("").full_screen_key);
        assert!(!Settings::parse("FullScreenKey no\n").full_screen_key);
        let s = Settings {
            full_screen_key: false,
            ..Settings::default()
        };
        assert!(!Settings::parse(&s.to_text()).full_screen_key);
    }

    #[test]
    fn writes_safe_lines() {
        let mut s = Settings::default();
        s.profile_mut("evil\nProfile x").theme = Some("a\"b".into());
        let text = s.to_text();
        assert!(!text.contains("\nProfile x"));
        s.profile_mut("cmd").password = PasswordSource::Command("bw get\nProfile y".into());
        let text = s.to_text();
        assert!(!text.contains("\nProfile y"));
        s.profile_mut("cmd").login_script = vec![Step::Send("a\nProfile z".into())];
        s.snippets.push(Snippet {
            name: "x\nProfile w".into(),
            lines: vec!["ls\nProfile v".into()],
            hosts: vec!["a\nProfile u".into()],
            folders: vec!["b\nProfile t".into()],
            when: "c\nProfile s".into(),
            ..Snippet::default()
        });
        let text = s.to_text();
        assert!(!text.contains("\nProfile z"));
        assert!(!text.contains("\nProfile w"));
        assert!(!text.contains("\nProfile v"));
        for p in ["u", "t", "s"] {
            assert!(!text.contains(&format!("\nProfile {p}")));
        }
        let back = Settings::parse(&text);
        assert_eq!(back.profiles.len(), 2);
        assert_eq!(back.profiles[0].theme.as_deref(), Some("ab"));
    }

    #[test]
    fn favourites_toggle() {
        let mut s = Settings::default();
        assert!(s.toggle_favourite("web"));
        assert!(s.is_favourite("Web"));
        assert!(!s.toggle_favourite("WEB"));
        assert!(s.favourites.is_empty());
    }
}
