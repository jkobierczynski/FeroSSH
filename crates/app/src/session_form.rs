//! The session form: the side pane's page for making and changing a saved
//! session, so nobody has to remember ssh_config keywords.
//!
//! The form is drawn into the side pane's [`Grid`] in place of the list,
//! in three folding sections (Connection, Look, Extras) with Save,
//! Connect, Cancel and Delete at the bottom. Empty fields show their
//! default in grey. Keys come in as [`SideKey`]s; what the user chose
//! comes out as a [`FormAction`].
//!
//! The connection settings of a host from `~/.ssh/config` or an
//! import are shown read-only: FeroSSH never writes those files. Giving
//! the session a new name saves an editable copy.

use ferossh_config::forward::parse_option;
use ferossh_config::login::Step;
use ferossh_config::password::{PasswordSource, TEMPLATES};
use ferossh_config::saved::{self, SavedHost};
use ferossh_config::serial::{self, FRAMINGS, Flow, LineSettings};
use ferossh_config::{Profile, ProxyServer, SessionKind, Settings};
use ferossh_render::{Cell, Color, Flags, Grid, Rgb};
use unicode_width::UnicodeWidthChar;

use crate::sessions::SideKey;
use crate::tabstrip::{Palette, fit};

/// Where the session's connection settings live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Not saved yet.
    New,
    /// FeroSSH's own `sessions.conf`: everything can change.
    Saved,
    /// A file FeroSSH does not write, named for the user.
    ReadOnly(String),
}

/// The defaults the empty fields show.
#[derive(Clone, Debug, Default)]
pub struct Hints {
    pub local_user: String,
    pub font: String,
    pub font_size: f32,
    pub scrollback: usize,
}

/// What the form holds, as typed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Values {
    pub name: String,
    pub folder: String,
    pub kind: SessionKind,
    pub host: String,
    pub user: String,
    pub port: String,
    pub key: String,
    /// Jump hosts, as a `ProxyJump` line: `bastion` or `me@gw:2222,inner`.
    pub jump: String,
    /// The SOCKS 5 or HTTP proxy, as a URL: `socks5://me@proxy:1080`.
    pub proxy: String,
    pub via: String,
    /// Empty: the theme rules decide.
    pub theme: String,
    pub font: String,
    pub font_size: String,
    pub scrollback: String,
    /// Attach tmux after each login.
    pub tmux_on: bool,
    /// The tmux session to attach to; empty is `main`.
    pub tmux: String,
    /// Empty, `ferossh` or `tmux`.
    pub tmux_layout: String,
    /// The server directory new shells and tmux panes start in.
    pub start_dir: String,
    /// The folders File transfer opens for this host: on this PC, and on
    /// the server.
    pub local_folder: String,
    pub remote_folder: String,
    /// One forward per entry, as `L 8080 localhost:80`.
    pub forwards: Vec<String>,
    pub desktop_port: String,
    pub vnc_port: String,
    /// What Open virtual desktop runs; empty picks a desktop session.
    pub virtual_desktop: String,
    /// `follow`, empty (the same) or `WIDTHxHEIGHT`.
    pub desktop_size: String,
    /// What Run remote app offers first, or a remote app session runs.
    pub remote_app: String,
    /// Copy the FeroStream agent to the host: the global setting when
    /// unset.
    pub copy_agent: Option<bool>,
    /// Remove the copied agent when done: the global setting when unset.
    pub remove_agent: Option<bool>,
    /// Narrows the remote apps' clipboard: empty, `to-host`, `from-host`
    /// or `off`.
    pub clipboard: String,
    /// Narrows the desktops' clipboard: empty, `to-host`, `from-host` or
    /// `off`.
    pub desk_clipboard: String,
    pub legacy: bool,
    /// Let the server use the agent's keys (`ForwardAgent yes`).
    pub forward_agent: bool,
    /// Kerberos login (`GSSAPIAuthentication`): FeroSSH's choice when
    /// unset.
    pub kerberos: Option<bool>,
    /// Hand the server a Kerberos ticket (`GSSAPIDelegateCredentials`).
    pub kerberos_delegate: bool,
    /// Where the password comes from: see [`PASSWORD_CHOICES`].
    pub password: usize,
    /// The password manager command, for a template or a custom one.
    pub password_command: String,
    /// A serial session's speed, as typed.
    pub speed: String,
    /// A serial session's framing, such as `8N1`; empty is 8N1.
    pub framing: String,
    pub flow: Flow,
    /// The login script, one step per entry, as `expect Password:`.
    pub login_script: Vec<String>,
    /// A Mosh session's server program; empty is `mosh-server`.
    pub mosh_server: String,
    /// A Mosh session's UDP port or range, as `60001:60010`.
    pub mosh_ports: String,
}

/// Asking, Windows, the password manager templates, then a command of
/// one's own.
const PASSWORD_ASK: usize = 0;
const PASSWORD_WINDOWS: usize = 1;
const PASSWORD_CUSTOM: usize = 2 + TEMPLATES.len();

/// The choice for a profile's password source.
fn password_choice(source: &PasswordSource) -> (usize, String) {
    match source {
        PasswordSource::Ask => (PASSWORD_ASK, String::new()),
        PasswordSource::Windows => (PASSWORD_WINDOWS, String::new()),
        PasswordSource::Command(c) => {
            let i = TEMPLATES
                .iter()
                .position(|t| t.command == c)
                .map_or(PASSWORD_CUSTOM, |i| i + 2);
            (i, c.clone())
        }
    }
}

fn opt_text(v: &Option<impl ToString>) -> String {
    v.as_ref().map(ToString::to_string).unwrap_or_default()
}

/// A forward as the form writes it: `L 8080 localhost:80`.
fn forward_text(spec: &ferossh_config::ForwardSpec) -> String {
    // `-L 8080:db:5432` without the dash.
    spec.to_string().trim_start_matches('-').to_owned()
}

impl Values {
    /// A saved session's connection settings.
    pub fn from_saved(h: &SavedHost) -> Self {
        Self {
            name: h.alias.clone(),
            folder: h.folder.clone().unwrap_or_default(),
            host: h.hostname.clone().unwrap_or_default(),
            user: h.user.clone().unwrap_or_default(),
            port: opt_text(&h.port),
            key: h.identity_file.clone().unwrap_or_default(),
            jump: h.proxy_jump.clone().unwrap_or_default(),
            proxy: h.proxy_server.clone().unwrap_or_default(),
            forwards: h.forwards.iter().map(forward_text).collect(),
            forward_agent: h.forward_agent,
            kerberos: h.gssapi,
            kerberos_delegate: h.gssapi_delegate,
            ..Self::default()
        }
    }

    /// Connection settings as ssh_config resolves them, for a host the
    /// form shows read-only. `jump` lists its jump hosts.
    #[allow(clippy::too_many_arguments)]
    pub fn from_resolved(
        name: &str,
        folder: Option<&str>,
        host: &str,
        user: &str,
        port: u16,
        key: Option<&str>,
        jump: &str,
        forwards: &[ferossh_config::ForwardSpec],
    ) -> Self {
        Self {
            name: name.to_owned(),
            folder: folder.unwrap_or_default().to_owned(),
            host: host.to_owned(),
            user: user.to_owned(),
            port: if port == 22 {
                String::new()
            } else {
                port.to_string()
            },
            key: key.unwrap_or_default().to_owned(),
            jump: jump.to_owned(),
            forwards: forwards.iter().map(forward_text).collect(),
            ..Self::default()
        }
    }

    /// Takes the look and extras from the host's profile.
    pub fn with_profile(mut self, p: Option<&Profile>) -> Self {
        let Some(p) = p else {
            return self;
        };
        self.kind = p.kind;
        self.via = p.via.clone().unwrap_or_default();
        self.theme = p.theme.clone().unwrap_or_default();
        self.font = p.font.clone().unwrap_or_default();
        self.font_size = opt_text(&p.font_size);
        self.scrollback = opt_text(&p.scrollback);
        self.tmux_on = p.tmux.is_some();
        self.tmux = p
            .tmux
            .clone()
            .filter(|t| t != DEFAULT_TMUX)
            .unwrap_or_default();
        self.tmux_layout = p.tmux_layout.clone().unwrap_or_default();
        self.start_dir = p.start_dir.clone().unwrap_or_default();
        self.local_folder = p.local_folder.clone().unwrap_or_default();
        self.remote_folder = p.remote_folder.clone().unwrap_or_default();
        self.desktop_port = opt_text(&p.desktop_port);
        self.vnc_port = opt_text(&p.vnc_port);
        self.virtual_desktop = p.virtual_desktop.clone().unwrap_or_default();
        self.remote_app = p.remote_app.clone().unwrap_or_default();
        self.copy_agent = p.remote_app_copy_agent;
        self.remove_agent = p.remote_app_remove_agent;
        self.clipboard = p.remote_app_clipboard.clone().unwrap_or_default();
        self.desk_clipboard = p.desktop_clipboard.clone().unwrap_or_default();
        self.desktop_size = p
            .desktop_size
            .and_then(|d| d.fixed())
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_default();
        self.legacy = p.legacy_algorithms;
        (self.password, self.password_command) = password_choice(&p.password);
        self.login_script = p.login_script.iter().map(ToString::to_string).collect();
        self.mosh_server = p.mosh_server.clone().unwrap_or_default();
        self.mosh_ports = p
            .mosh_ports
            .map(ferossh_config::settings::mosh_ports_text)
            .unwrap_or_default();
        if let Some(line) = &p.serial {
            self.speed = line.speed.to_string();
            self.framing = line.framing();
            self.flow = line.flow;
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Folder,
    Type,
    Host,
    User,
    Port,
    Key,
    Jump,
    Proxy,
    Via,
    Theme,
    Font,
    FontSize,
    Scrollback,
    Tmux,
    TmuxSession,
    TmuxLayout,
    StartDir,
    LocalFolder,
    RemoteFolder,
    Forward(usize),
    DesktopPort,
    VncPort,
    VirtualDesktop,
    DesktopSize,
    RemoteApp,
    CopyAgent,
    RemoveAgent,
    Clipboard,
    DeskClipboard,
    Legacy,
    ForwardAgent,
    Kerberos,
    KerberosDelegate,
    Password,
    PasswordCommand,
    Speed,
    Framing,
    Flow,
    MoshServer,
    MoshPorts,
    LoginStep(usize),
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Folder => "Folder",
            Field::Type => "Type",
            Field::Host => "Host",
            Field::User => "User",
            Field::Port => "Port",
            Field::Key => "Key file",
            Field::Jump => "Jump host",
            Field::Proxy => "Proxy",
            Field::Via => "Via SSH",
            Field::Theme => "Theme",
            Field::Font => "Font",
            Field::FontSize => "Font size",
            Field::Scrollback => "Scrollback",
            Field::Tmux => "tmux",
            Field::TmuxSession => "tmux name",
            Field::TmuxLayout => "tmux panes",
            Field::StartDir => "Start dir",
            Field::LocalFolder => "Local folder",
            Field::RemoteFolder => "Remote folder",
            Field::Forward(_) => "Forward",
            Field::DesktopPort => "RDP port",
            Field::VncPort => "VNC port",
            Field::VirtualDesktop => "Virtual desk",
            Field::DesktopSize => "Desk size",
            Field::RemoteApp => "Remote app",
            Field::CopyAgent => "Copy agent",
            Field::RemoveAgent => "Clean up",
            Field::Clipboard => "App clip",
            Field::DeskClipboard => "Desk clip",
            Field::Legacy => "Old crypto",
            Field::ForwardAgent => "Fwd agent",
            Field::Kerberos => "Kerberos",
            Field::KerberosDelegate => "Fwd ticket",
            Field::Password => "Password",
            Field::PasswordCommand => "Command",
            Field::Speed => "Speed",
            Field::Framing => "Framing",
            Field::Flow => "Flow ctrl",
            Field::MoshServer => "Mosh server",
            Field::MoshPorts => "UDP ports",
            Field::LoginStep(_) => "Login",
        }
    }

    /// Picked from a list rather than typed.
    fn is_choice(self) -> bool {
        matches!(
            self,
            Field::Type
                | Field::Theme
                | Field::Tmux
                | Field::TmuxLayout
                | Field::Legacy
                | Field::ForwardAgent
                | Field::Kerberos
                | Field::KerberosDelegate
                | Field::CopyAgent
                | Field::RemoveAgent
                | Field::Clipboard
                | Field::DeskClipboard
                | Field::Password
                | Field::Framing
                | Field::Flow
        )
    }
}

/// Steps an unset / yes / no choice.
fn tri(v: Option<bool>, forward: bool) -> Option<bool> {
    match (v, forward) {
        (None, true) | (Some(true), false) => Some(true),
        (Some(true), true) | (None, false) => Some(false),
        (Some(false), _) => None,
    }
}

/// The tmux session attached to when none is named, as Session > Attach
/// tmux does.
const DEFAULT_TMUX: &str = "main";

const SECTIONS: [&str; 3] = ["Connection", "Look", "Extras"];
const BUTTONS: [&str; 4] = ["Save", "Connect", "Cancel", "Delete"];
const LABEL: usize = 11;
/// Longest value one can type.
const MAX_TEXT: usize = 260;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    Section(usize),
    Field(Field),
    Note(String),
    /// "Browse..." under the key file.
    Browse,
    /// One line of the problem with what was typed.
    Error(String),
    Buttons,
}

/// A field of the form, for the native controls to name it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldId(Field);

/// One row of the native form.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeRow {
    /// 0 Connection, 1 Look, 2 Extras.
    pub section: usize,
    pub field: Option<FieldId>,
    pub label: String,
    pub kind: NativeKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NativeKind {
    /// Typed; `hint` shows while it is empty, `locked` is read-only.
    Text {
        value: String,
        hint: String,
        locked: bool,
    },
    /// Picked from `options`.
    Choice {
        options: Vec<String>,
        selected: usize,
    },
    /// A line of explanation.
    Note(String),
    /// A Browse... button for the key file.
    Browse,
}

/// What the window should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormAction {
    /// The values check out: save them, and with `connect` open the
    /// session too.
    Save {
        connect: bool,
    },
    Cancel,
    /// Remove the saved session (the window asks first).
    Delete,
    /// Show a file picker for the key file.
    BrowseKey,
}

pub struct SessionForm {
    /// The name the session had when the form opened.
    pub original: Option<String>,
    pub source: Source,
    pub values: Values,
    hints: Hints,
    /// Every session name, to refuse a duplicate and check Via.
    names: Vec<String>,
    themes: Vec<String>,
    open: [bool; 3],
    focus: usize,
    button: usize,
    cursor: usize,
    scroll: usize,
    error: Option<String>,
    /// Where each button was drawn: (first column, past the last).
    button_cols: Vec<(usize, usize)>,
    /// The grid's width at the last draw, for wrapping the error.
    width: usize,
}

impl SessionForm {
    /// A form for a new session (`values` may hold a name to start from)
    /// or for editing one.
    pub fn new(
        original: Option<String>,
        source: Source,
        values: Values,
        hints: Hints,
        names: Vec<String>,
        themes: Vec<String>,
    ) -> Self {
        let mut f = Self {
            original,
            source,
            values,
            hints,
            names,
            themes,
            open: [true, false, false],
            focus: 0,
            button: 0,
            cursor: 0,
            scroll: 0,
            error: None,
            button_cols: Vec::new(),
            width: 34,
        };
        f.values.forwards.retain(|t| !t.trim().is_empty());
        f.values.login_script.retain(|t| !t.trim().is_empty());
        f.tidy_forwards();
        f.focus = f.first_field();
        f.cursor = f.text(Field::Name).map_or(0, |t| t.chars().count());
        f
    }

    /// The connection fields come from a file FeroSSH does not write, and
    /// the name is still the same, so saving would not make a copy.
    fn locked(&self) -> bool {
        matches!(self.source, Source::ReadOnly(_))
            && self
                .original
                .as_deref()
                .is_some_and(|o| o.eq_ignore_ascii_case(self.values.name.trim()))
    }

    fn field_locked(&self, f: Field) -> bool {
        self.locked()
            && matches!(
                f,
                Field::Folder
                    | Field::Host
                    | Field::User
                    | Field::Port
                    | Field::Key
                    | Field::Jump
                    | Field::Proxy
                    | Field::Forward(_)
                    // Kept with the connection, which a read-only host
                    // cannot save.
                    | Field::ForwardAgent
                    | Field::Kerberos
                    | Field::KerberosDelegate
            )
    }

    /// Keeps one empty forward line at the end to type a new one in.
    /// Keeps one empty forward line and one empty login step at the end
    /// to type a new one in.
    fn tidy_forwards(&mut self) {
        for list in [&mut self.values.forwards, &mut self.values.login_script] {
            if list.last().is_none_or(|t| !t.trim().is_empty()) {
                list.push(String::new());
            }
        }
    }

    fn items(&self) -> Vec<Item> {
        self.items_with(self.open)
    }

    /// The items with the sections `open` says.
    fn items_with(&self, open: [bool; 3]) -> Vec<Item> {
        let mut out = Vec::new();
        let kind = self.values.kind;
        out.push(Item::Section(0));
        if open[0] {
            out.push(Item::Field(Field::Name));
            if let Source::ReadOnly(file) = &self.source {
                if self.locked() {
                    out.push(Item::Note(format!("From {file}: read-only.")));
                    out.push(Item::Note("A new name saves a copy.".into()));
                } else {
                    out.push(Item::Note(format!("Saves a copy; {file} stays.")));
                }
            }
            out.extend([Field::Folder, Field::Type].map(Item::Field));
            // A virtual desktop or remote app runs on its Via SSH host:
            // no address.
            if !matches!(kind, SessionKind::Xvfb | SessionKind::RemoteApp) {
                out.push(Item::Field(Field::Host));
            }
            match kind {
                SessionKind::Xvfb | SessionKind::RemoteApp => {}
                SessionKind::Serial => {
                    out.extend([Field::Speed, Field::Framing, Field::Flow].map(Item::Field));
                }
                // Telnet logs in at the host's own prompt.
                SessionKind::Telnet => out.push(Item::Field(Field::Port)),
                _ => out.extend([Field::User, Field::Port].map(Item::Field)),
            }
            match kind {
                SessionKind::Ssh | SessionKind::Mosh | SessionKind::Sftp => {
                    out.push(Item::Field(Field::Key));
                    if !self.field_locked(Field::Key) {
                        out.push(Item::Browse);
                    }
                    out.push(Item::Field(Field::Password));
                    match self.values.password {
                        PASSWORD_ASK => {}
                        PASSWORD_WINDOWS => {
                            out.push(Item::Note("Saved after a login that works.".into()));
                        }
                        _ => {
                            out.push(Item::Field(Field::PasswordCommand));
                            out.push(Item::Note("%n is the connection's name.".into()));
                        }
                    }
                    out.push(Item::Field(Field::Jump));
                    out.push(Item::Field(Field::Proxy));
                    if kind == SessionKind::Mosh {
                        out.extend([Field::MoshServer, Field::MoshPorts].map(Item::Field));
                        out.push(Item::Note("SSH logs in and starts the server;".into()));
                        out.push(Item::Note("then UDP to the host's ports.".into()));
                    }
                    if kind == SessionKind::Sftp {
                        out.push(Item::Note("Logs in without a shell and".into()));
                        out.push(Item::Note("opens File transfer on the host.".into()));
                    }
                }
                SessionKind::Rdp | SessionKind::Vnc => {
                    out.extend(
                        [Field::Via, Field::DesktopSize, Field::DeskClipboard].map(Item::Field),
                    );
                }
                SessionKind::Xvfb => out.extend(
                    [
                        Field::Via,
                        Field::VirtualDesktop,
                        Field::DesktopSize,
                        Field::DeskClipboard,
                    ]
                    .map(Item::Field),
                ),
                SessionKind::RemoteApp => {
                    out.extend(
                        [
                            Field::Via,
                            Field::RemoteApp,
                            Field::CopyAgent,
                            Field::RemoveAgent,
                            Field::Clipboard,
                        ]
                        .map(Item::Field),
                    );
                    out.push(Item::Note("Its windows open as Windows windows.".into()));
                }
                SessionKind::Serial => {}
                SessionKind::Telnet => {
                    out.push(Item::Note("Unencrypted: anyone on the way".into()));
                    out.push(Item::Note("can read it, passwords too.".into()));
                }
            }
        }
        if matches!(
            kind,
            SessionKind::Ssh | SessionKind::Mosh | SessionKind::Serial | SessionKind::Telnet
        ) {
            out.push(Item::Section(1));
            if open[1] {
                out.extend(
                    [
                        Field::Theme,
                        Field::Font,
                        Field::FontSize,
                        Field::Scrollback,
                    ]
                    .map(Item::Field),
                );
            }
        }
        if kind == SessionKind::Sftp {
            out.push(Item::Section(2));
            if open[2] {
                out.extend(
                    [
                        Field::RemoteFolder,
                        Field::LocalFolder,
                        Field::Kerberos,
                        Field::Legacy,
                    ]
                    .map(Item::Field),
                );
            }
        }
        if matches!(kind, SessionKind::Ssh | SessionKind::Mosh) {
            out.push(Item::Section(2));
            if open[2] {
                // tmux control mode needs SSH's byte stream; Mosh sends
                // screens.
                if kind == SessionKind::Ssh {
                    out.push(Item::Field(Field::Tmux));
                    if self.values.tmux_on {
                        out.push(Item::Field(Field::TmuxSession));
                    }
                    out.push(Item::Field(Field::TmuxLayout));
                }
                out.push(Item::Field(Field::StartDir));
                out.push(Item::Field(Field::LocalFolder));
                out.push(Item::Field(Field::RemoteFolder));
                let n = self.values.forwards.len();
                let locked = self.field_locked(Field::Forward(0));
                for i in 0..n {
                    let empty = self.values.forwards.get(i).is_none_or(|t| t.is_empty());
                    if !(locked && empty) {
                        out.push(Item::Field(Field::Forward(i)));
                    }
                }
                if kind == SessionKind::Mosh {
                    out.extend(
                        [
                            Field::ForwardAgent,
                            Field::Kerberos,
                            Field::KerberosDelegate,
                            Field::Legacy,
                        ]
                        .map(Item::Field),
                    );
                } else {
                    out.extend(
                        [
                            Field::DesktopPort,
                            Field::VncPort,
                            Field::VirtualDesktop,
                            Field::DesktopSize,
                            Field::DeskClipboard,
                            Field::RemoteApp,
                            Field::CopyAgent,
                            Field::RemoveAgent,
                            Field::Clipboard,
                            Field::ForwardAgent,
                            Field::Kerberos,
                            Field::KerberosDelegate,
                            Field::Legacy,
                        ]
                        .map(Item::Field),
                    );
                }
            }
        }
        if matches!(
            kind,
            SessionKind::Ssh | SessionKind::Mosh | SessionKind::Serial | SessionKind::Telnet
        ) {
            if !matches!(kind, SessionKind::Ssh | SessionKind::Mosh) {
                out.push(Item::Section(2));
            }
            if open[2] {
                let n = self.values.login_script.len();
                out.extend((0..n).map(|i| Item::Field(Field::LoginStep(i))));
                out.push(Item::Note(
                    "expect, send, type, key, password, secret, pause".into(),
                ));
            }
        }
        if let Some(e) = &self.error {
            out.extend(
                wrap(e, self.width.saturating_sub(2))
                    .into_iter()
                    .map(Item::Error),
            );
        }
        out.push(Item::Buttons);
        out
    }

    fn focusable(item: &Item) -> bool {
        !matches!(item, Item::Note(_) | Item::Error(_))
    }

    fn first_field(&self) -> usize {
        self.items()
            .iter()
            .position(|i| matches!(i, Item::Field(_)))
            .unwrap_or(0)
    }

    fn focused_field(&self) -> Option<Field> {
        match self.items().get(self.focus) {
            Some(Item::Field(f)) => Some(*f),
            _ => None,
        }
    }

    fn buttons(&self) -> Vec<&'static str> {
        BUTTONS
            .into_iter()
            .filter(|b| *b != "Delete" || self.source == Source::Saved)
            .collect()
    }

    fn text(&self, f: Field) -> Option<&String> {
        let v = &self.values;
        Some(match f {
            Field::Name => &v.name,
            Field::Folder => &v.folder,
            Field::Host => &v.host,
            Field::User => &v.user,
            Field::Port => &v.port,
            Field::Key => &v.key,
            Field::Jump => &v.jump,
            Field::Proxy => &v.proxy,
            Field::Via => &v.via,
            Field::Font => &v.font,
            Field::FontSize => &v.font_size,
            Field::Scrollback => &v.scrollback,
            Field::TmuxSession => &v.tmux,
            Field::StartDir => &v.start_dir,
            Field::LocalFolder => &v.local_folder,
            Field::RemoteFolder => &v.remote_folder,
            Field::Forward(i) => return v.forwards.get(i),
            Field::LoginStep(i) => return v.login_script.get(i),
            Field::DesktopPort => &v.desktop_port,
            Field::VncPort => &v.vnc_port,
            Field::VirtualDesktop => &v.virtual_desktop,
            Field::DesktopSize => &v.desktop_size,
            Field::RemoteApp => &v.remote_app,
            Field::PasswordCommand => &v.password_command,
            Field::Speed => &v.speed,
            Field::MoshServer => &v.mosh_server,
            Field::MoshPorts => &v.mosh_ports,
            Field::Type
            | Field::Theme
            | Field::Tmux
            | Field::TmuxLayout
            | Field::Legacy
            | Field::ForwardAgent
            | Field::Kerberos
            | Field::KerberosDelegate
            | Field::CopyAgent
            | Field::RemoveAgent
            | Field::Clipboard
            | Field::DeskClipboard
            | Field::Password
            | Field::Framing
            | Field::Flow => {
                return None;
            }
        })
    }

    fn text_mut(&mut self, f: Field) -> Option<&mut String> {
        let v = &mut self.values;
        Some(match f {
            Field::Name => &mut v.name,
            Field::Folder => &mut v.folder,
            Field::Host => &mut v.host,
            Field::User => &mut v.user,
            Field::Port => &mut v.port,
            Field::Key => &mut v.key,
            Field::Jump => &mut v.jump,
            Field::Proxy => &mut v.proxy,
            Field::Via => &mut v.via,
            Field::Font => &mut v.font,
            Field::FontSize => &mut v.font_size,
            Field::Scrollback => &mut v.scrollback,
            Field::TmuxSession => &mut v.tmux,
            Field::StartDir => &mut v.start_dir,
            Field::LocalFolder => &mut v.local_folder,
            Field::RemoteFolder => &mut v.remote_folder,
            Field::Forward(i) => return v.forwards.get_mut(i),
            Field::LoginStep(i) => return v.login_script.get_mut(i),
            Field::DesktopPort => &mut v.desktop_port,
            Field::VncPort => &mut v.vnc_port,
            Field::VirtualDesktop => &mut v.virtual_desktop,
            Field::RemoteApp => &mut v.remote_app,
            Field::DesktopSize => &mut v.desktop_size,
            Field::PasswordCommand => &mut v.password_command,
            Field::Speed => &mut v.speed,
            Field::MoshServer => &mut v.mosh_server,
            Field::MoshPorts => &mut v.mosh_ports,
            Field::Type
            | Field::Theme
            | Field::Tmux
            | Field::TmuxLayout
            | Field::Legacy
            | Field::ForwardAgent
            | Field::Kerberos
            | Field::KerberosDelegate
            | Field::CopyAgent
            | Field::RemoveAgent
            | Field::Clipboard
            | Field::DeskClipboard
            | Field::Password
            | Field::Framing
            | Field::Flow => {
                return None;
            }
        })
    }

    /// What a choice field shows.
    fn choice(&self, f: Field) -> String {
        let v = &self.values;
        match f {
            Field::Type => match v.kind {
                SessionKind::Ssh => "SSH terminal",
                SessionKind::Rdp => "Remote desktop (RDP)",
                SessionKind::Vnc => "VNC desktop",
                SessionKind::Serial => "Serial console",
                SessionKind::Telnet => "Telnet (unencrypted)",
                SessionKind::Mosh => "Mosh terminal (SSH + UDP)",
                SessionKind::Sftp => "SFTP file transfer only",
                SessionKind::Xvfb => "Virtual desktop (Xvfb)",
                SessionKind::RemoteApp => "Remote app (FeroStream)",
            }
            .to_owned(),
            Field::CopyAgent => match v.copy_agent {
                None => "as in Session > Remote apps",
                Some(true) => "yes, when missing or old",
                Some(false) => "no: the host has one",
            }
            .to_owned(),
            Field::RemoveAgent => match v.remove_agent {
                None => "as in Session > Remote apps",
                Some(true) => "remove the agent when done",
                Some(false) => "keep the agent",
            }
            .to_owned(),
            Field::Clipboard => match v.clipboard.as_str() {
                "to-host" => "only Windows to host",
                "from-host" => "only host to Windows",
                "off" => "off for this host",
                _ => "as in Session > Remote apps",
            }
            .to_owned(),
            Field::DeskClipboard => match v.desk_clipboard.as_str() {
                "to-host" => "only Windows to host",
                "from-host" => "only host to Windows",
                "off" => "off for this host",
                _ => "as in Session > Desktop clipboard",
            }
            .to_owned(),
            Field::Framing if v.framing.is_empty() => "8N1".to_owned(),
            Field::Framing => v.framing.clone(),
            Field::Flow => v.flow.label().to_owned(),
            Field::Theme if v.theme.is_empty() => "by theme rules".to_owned(),
            Field::Theme => v.theme.clone(),
            Field::Tmux => if v.tmux_on {
                "on: attach after login"
            } else {
                "off"
            }
            .to_owned(),
            Field::TmuxLayout => match v.tmux_layout.as_str() {
                "tmux" => "tmux's own splits",
                _ => "one window per pane",
            }
            .to_owned(),
            Field::Legacy => if v.legacy { "allowed" } else { "off" }.to_owned(),
            Field::ForwardAgent => if v.forward_agent {
                "on, asking before each use"
            } else {
                "off"
            }
            .to_owned(),
            Field::Kerberos => match v.kerberos {
                None => "auto: when Windows is in a domain",
                Some(true) => "on",
                Some(false) => "off",
            }
            .to_owned(),
            Field::KerberosDelegate => if v.kerberos_delegate {
                "on: the server gets a ticket of yours"
            } else {
                "off"
            }
            .to_owned(),
            Field::Password => match v.password {
                PASSWORD_ASK => "ask every time".to_owned(),
                PASSWORD_WINDOWS => "Windows Credential Manager".to_owned(),
                PASSWORD_CUSTOM => "a command of my own".to_owned(),
                i => TEMPLATES
                    .get(i - 2)
                    .map_or_else(String::new, |t| t.name.to_owned()),
            },
            _ => String::new(),
        }
    }

    /// Steps a choice field forwards or back.
    fn cycle(&mut self, f: Field, forward: bool) {
        if self.field_locked(f) {
            return;
        }
        let step = |i: usize, n: usize| {
            if forward {
                (i + 1) % n
            } else {
                (i + n - 1) % n
            }
        };
        let v = &mut self.values;
        match f {
            Field::Type => {
                let kinds = [
                    SessionKind::Ssh,
                    SessionKind::Rdp,
                    SessionKind::Vnc,
                    SessionKind::Xvfb,
                    SessionKind::RemoteApp,
                    SessionKind::Mosh,
                    SessionKind::Sftp,
                    SessionKind::Telnet,
                    SessionKind::Serial,
                ];
                let i = kinds.iter().position(|k| *k == v.kind).unwrap_or(0);
                v.kind = kinds.get(step(i, kinds.len())).copied().unwrap_or_default();
            }
            Field::Framing => {
                let current = if v.framing.is_empty() {
                    "8N1"
                } else {
                    &v.framing
                };
                let i = FRAMINGS.iter().position(|f| *f == current).unwrap_or(0);
                v.framing = FRAMINGS
                    .get(step(i, FRAMINGS.len()))
                    .copied()
                    .unwrap_or_default()
                    .to_owned();
            }
            Field::Flow => {
                let i = Flow::ALL.iter().position(|f| *f == v.flow).unwrap_or(0);
                v.flow = Flow::ALL
                    .get(step(i, Flow::ALL.len()))
                    .copied()
                    .unwrap_or_default();
            }
            Field::Theme => {
                let mut all = vec![String::new()];
                all.extend(self.themes.iter().cloned());
                let i = all
                    .iter()
                    .position(|t| t.eq_ignore_ascii_case(&v.theme))
                    .unwrap_or(0);
                v.theme = all.get(step(i, all.len())).cloned().unwrap_or_default();
            }
            Field::TmuxLayout => {
                v.tmux_layout = if v.tmux_layout == "tmux" {
                    String::new()
                } else {
                    "tmux".into()
                };
            }
            Field::Legacy => v.legacy = !v.legacy,
            Field::ForwardAgent => v.forward_agent = !v.forward_agent,
            Field::Kerberos => v.kerberos = tri(v.kerberos, forward),
            Field::KerberosDelegate => v.kerberos_delegate = !v.kerberos_delegate,
            Field::CopyAgent => v.copy_agent = tri(v.copy_agent, forward),
            Field::RemoveAgent => v.remove_agent = tri(v.remove_agent, forward),
            Field::Clipboard | Field::DeskClipboard => {
                let all = ["", "to-host", "from-host", "off"];
                let way = if f == Field::Clipboard {
                    &mut v.clipboard
                } else {
                    &mut v.desk_clipboard
                };
                let i = all.iter().position(|c| *c == way.as_str()).unwrap_or(0);
                *way = all
                    .get(step(i, all.len()))
                    .copied()
                    .unwrap_or("")
                    .to_owned();
            }
            Field::Tmux => v.tmux_on = !v.tmux_on,
            Field::Password => {
                v.password = step(v.password, PASSWORD_CUSTOM + 1);
                // A template fills in its command; a command of one's own
                // keeps what was typed unless it was a template's.
                if let Some(t) = v.password.checked_sub(2).and_then(|i| TEMPLATES.get(i)) {
                    v.password_command = t.command.to_owned();
                } else if TEMPLATES.iter().any(|t| t.command == v.password_command) {
                    v.password_command.clear();
                }
            }
            _ => {}
        }
    }

    /// The grey text an empty field shows.
    fn hint(&self, f: Field) -> String {
        let v = &self.values;
        let h = &self.hints;
        match f {
            Field::Name => "required, e.g. web-prod".into(),
            Field::Folder => "none".into(),
            Field::Host if v.kind == SessionKind::Serial => "COM port, e.g. COM3".into(),
            Field::Host
                if !matches!(
                    v.kind,
                    SessionKind::Ssh | SessionKind::Mosh | SessionKind::Sftp | SessionKind::Telnet
                ) && !v.via.trim().is_empty() =>
            {
                "the Via SSH host itself".into()
            }
            Field::Host if v.name.trim().is_empty() => "host name or IP address".into(),
            Field::Host => v.name.trim().to_owned(),
            Field::User => match (v.kind, v.via.is_empty()) {
                (SessionKind::Vnc, _) => "asked by the server".into(),
                (SessionKind::Rdp, true) => "asked by Windows".into(),
                _ => h.local_user.clone(),
            },
            Field::Port => match v.kind {
                SessionKind::Ssh | SessionKind::Mosh | SessionKind::Sftp => "22".into(),
                SessionKind::Rdp => "3389".into(),
                SessionKind::Vnc => "5900".into(),
                SessionKind::Telnet => "23".into(),
                SessionKind::Serial | SessionKind::Xvfb | SessionKind::RemoteApp => String::new(),
            },
            Field::Speed => "9600 bits/s".into(),
            Field::Key => "default keys and agent".into(),
            Field::Jump => "none, or e.g. bastion".into(),
            Field::Proxy => "none, or socks5:// or http://host".into(),
            Field::Via
                if matches!(
                    v.kind,
                    SessionKind::Vnc | SessionKind::Xvfb | SessionKind::RemoteApp
                ) =>
            {
                "an SSH connection (required)".into()
            }
            Field::Via => "none: connect directly".into(),
            Field::Font => h.font.clone(),
            Field::FontSize => format!("{}", h.font_size),
            Field::Scrollback => h.scrollback.to_string(),
            Field::TmuxSession => DEFAULT_TMUX.into(),
            Field::StartDir => "home folder, or e.g. /srv/www".into(),
            Field::LocalFolder => "File transfer, e.g. D:\\Sites".into(),
            Field::RemoteFolder => "File transfer, e.g. /var/www".into(),
            Field::Forward(_) => "e.g. L 8080 web:80".into(),
            Field::LoginStep(0) => "e.g. expect Password:".into(),
            Field::LoginStep(_) => "next step".into(),
            Field::DesktopPort => "3389".into(),
            Field::VncPort => "5900".into(),
            Field::VirtualDesktop => "a desktop session, or e.g. firefox".into(),
            Field::DesktopSize => "follows the window, or e.g. 1920x1080".into(),
            Field::RemoteApp => "xterm, or e.g. gimp ~/photo.png".into(),
            Field::PasswordCommand => "program and arguments".into(),
            Field::MoshServer => "mosh-server, or a path to it".into(),
            Field::MoshPorts => "any in 60001-60999, or e.g. 60001:60010".into(),
            Field::Type
            | Field::Theme
            | Field::Tmux
            | Field::TmuxLayout
            | Field::Legacy
            | Field::ForwardAgent
            | Field::Kerberos
            | Field::KerberosDelegate
            | Field::CopyAgent
            | Field::RemoveAgent
            | Field::Clipboard
            | Field::DeskClipboard
            | Field::Password
            | Field::Framing
            | Field::Flow => String::new(),
        }
    }

    fn set_focus(&mut self, i: usize) {
        let items = self.items();
        let i = i.min(items.len().saturating_sub(1));
        self.focus = i;
        self.cursor = match items.get(i) {
            Some(Item::Field(f)) => self.text(*f).map_or(0, |t| t.chars().count()),
            _ => 0,
        };
    }

    fn move_focus(&mut self, down: bool, wrap: bool) {
        let items = self.items();
        let n = items.len();
        let mut i = self.focus;
        for _ in 0..n {
            i = if down {
                if i + 1 >= n {
                    if !wrap {
                        return;
                    }
                    0
                } else {
                    i + 1
                }
            } else if i == 0 {
                if !wrap {
                    return;
                }
                n - 1
            } else {
                i - 1
            };
            if items.get(i).is_some_and(Self::focusable) {
                self.set_focus(i);
                return;
            }
        }
    }

    /// Every section's rows, for the native form; the error and the
    /// buttons are separate ([`Self::error`], [`Self::button_names`]).
    pub fn native_rows(&self) -> Vec<NativeRow> {
        let mut section = 0;
        let mut out = Vec::new();
        for item in self.items_with([true; 3]) {
            let (field, label, kind) = match item {
                Item::Section(s) => {
                    section = s;
                    continue;
                }
                Item::Error(_) | Item::Buttons => continue,
                Item::Note(text) => (None, String::new(), NativeKind::Note(text)),
                Item::Browse => (None, String::new(), NativeKind::Browse),
                Item::Field(f) => {
                    let label = match f {
                        Field::Forward(i) => format!("Forward {}", i + 1),
                        Field::LoginStep(i) => format!("Login {}", i + 1),
                        _ => f.label().to_owned(),
                    };
                    let kind = if f.is_choice() {
                        let options = self.options(f);
                        let current = self.choice(f);
                        let selected = options.iter().position(|o| *o == current).unwrap_or(0);
                        NativeKind::Choice { options, selected }
                    } else {
                        NativeKind::Text {
                            value: self.text(f).cloned().unwrap_or_default(),
                            hint: self.hint(f),
                            locked: self.field_locked(f),
                        }
                    };
                    (Some(FieldId(f)), label, kind)
                }
            };
            out.push(NativeRow {
                section,
                field,
                label,
                kind,
            });
        }
        out
    }

    /// The sections the form has for its type, by number (0 Connection,
    /// 1 Look, 2 Extras), with their names.
    pub fn sections(&self) -> Vec<(usize, &'static str)> {
        self.items_with([false; 3])
            .into_iter()
            .filter_map(|i| match i {
                Item::Section(s) => SECTIONS.get(s).map(|n| (s, *n)),
                _ => None,
            })
            .collect()
    }

    /// A choice field's options in order, starting from its default.
    fn options(&self, f: Field) -> Vec<String> {
        let mut probe = SessionForm::new(
            None,
            Source::New,
            Values::default(),
            self.hints.clone(),
            Vec::new(),
            self.themes.clone(),
        );
        let mut out: Vec<String> = Vec::new();
        for _ in 0..256 {
            let c = probe.choice(f);
            if out.contains(&c) {
                break;
            }
            out.push(c);
            probe.cycle(f, true);
        }
        out
    }

    /// Text typed into a native field. A new empty forward or login line
    /// appears below the last one once it has text.
    pub fn set_text(&mut self, field: FieldId, text: &str) {
        if self.field_locked(field.0) {
            return;
        }
        if let Some(t) = self.text_mut(field.0) {
            *t = text.chars().take(MAX_TEXT).collect();
        }
        self.tidy_forwards();
    }

    /// Option `index` picked in a native choice field.
    pub fn set_choice(&mut self, field: FieldId, index: usize) {
        let f = field.0;
        if self.field_locked(f) {
            return;
        }
        let options = self.options(f);
        let Some(want) = options.get(index) else {
            return;
        };
        for _ in 0..options.len() {
            if self.choice(f) == *want {
                return;
            }
            self.cycle(f, true);
        }
    }

    /// Save or Connect pressed: checks, and on a problem keeps it for
    /// [`Self::error`].
    pub fn submit(&mut self, connect: bool) -> Option<FormAction> {
        self.checked(connect)
    }

    /// The problem the last Save or Connect found.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The buttons the form has: Save, Connect, Cancel, and Delete for a
    /// saved connection.
    pub fn button_names(&self) -> Vec<&'static str> {
        self.buttons()
    }

    /// Checks what was typed; the first problem, in words.
    pub fn check(&self) -> Result<(), String> {
        let v = &self.values;
        let name = v.name.trim();
        if name.is_empty() {
            return Err("Give the connection a name.".into());
        }
        if !saved::valid_alias(name) {
            return Err("Names use letters, digits, . _ and - only.".into());
        }
        let renamed = self
            .original
            .as_deref()
            .is_none_or(|o| !o.eq_ignore_ascii_case(name));
        if renamed && self.names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            return Err(format!("A connection named {name} already exists."));
        }
        if self.locked() {
            // Only the profile is written.
        } else {
            let host = v.host.trim();
            if !host.is_empty() && !saved::valid_hostname(host) {
                return Err("The host is not a host name or IP address.".into());
            }
            // Telnet has no user field: whatever an SSH session left there
            // is not saved.
            if v.kind != SessionKind::Telnet
                && v.user
                    .trim()
                    .contains(|c: char| c.is_whitespace() || c.is_control() || c == '@')
            {
                return Err("The user name has a space or @ in it.".into());
            }
            port(&v.port, "The port")?;
            let jump = v.jump.trim();
            if matches!(
                v.kind,
                SessionKind::Ssh | SessionKind::Mosh | SessionKind::Sftp
            ) && !jump.is_empty()
            {
                if !saved::valid_proxy_jump(jump) {
                    return Err(
                        "Jump host: a connection name or user@host:port; several go \
                                in turn, separated by commas."
                            .into(),
                    );
                }
                if jump.split(',').any(|h| h.trim().eq_ignore_ascii_case(name)) {
                    return Err("The jump host is another connection, not this one.".into());
                }
            }
            let proxy = v.proxy.trim();
            if matches!(
                v.kind,
                SessionKind::Ssh | SessionKind::Mosh | SessionKind::Sftp
            ) && !proxy.is_empty()
            {
                if ProxyServer::parse(proxy).is_none() {
                    let login = proxy
                        .split_once("://")
                        .and_then(|(_, rest)| rest.rsplit_once('@'))
                        .map(|(login, _)| login);
                    return Err(if login.is_some_and(|l| l.contains(':')) {
                        "Proxy: no password in the address; FeroSSH asks for it and keeps \
                         it in the Windows Credential Manager."
                            .into()
                    } else {
                        "Proxy: socks5://[user@]host[:port] or http://[user@]host[:port].".into()
                    });
                }
                if !jump.is_empty() {
                    return Err("A proxy and jump hosts: put the proxy on the first jump \
                                host's own connection."
                        .into());
                }
            }
            for (i, f) in v.forwards.iter().enumerate() {
                if !f.trim().is_empty() && parse_option(f).is_none() {
                    return Err(format!(
                        "Forward {}: write it like L 8080 localhost:80, \
                         R 9000 localhost:3000 or D 1080.",
                        i + 1
                    ));
                }
            }
        }
        if v.kind == SessionKind::Serial && !self.locked() {
            if serial::port_name(&v.host).is_none() {
                return Err(
                    "Fill in the COM port, such as COM3 (Device Manager lists them \
                            under Ports)."
                        .into(),
                );
            }
            if let Some(speed) = number::<u32>(&v.speed, "The speed")?
                && !(50..=serial::MAX_SPEED).contains(&speed)
            {
                return Err(format!(
                    "The speed goes from 50 to {} bits/s.",
                    serial::MAX_SPEED
                ));
            }
        }
        let via = v.via.trim();
        match v.kind {
            SessionKind::Ssh
            | SessionKind::Mosh
            | SessionKind::Sftp
            | SessionKind::Serial
            | SessionKind::Telnet => {}
            SessionKind::Vnc if via.is_empty() => {
                return Err(
                    "A VNC desktop goes through an SSH connection: fill in Via SSH.".into(),
                );
            }
            SessionKind::Xvfb if via.is_empty() => {
                return Err("A virtual desktop runs on an SSH connection: fill in Via SSH.".into());
            }
            SessionKind::RemoteApp if via.is_empty() => {
                return Err("A remote app runs on an SSH connection: fill in Via SSH.".into());
            }
            _ if via.is_empty() => {}
            _ if via.eq_ignore_ascii_case(name) => {
                return Err("Via SSH names another connection, not this one.".into());
            }
            _ if !self.names.iter().any(|n| n.eq_ignore_ascii_case(via)) => {
                return Err(format!("There is no connection named {via} to go through."));
            }
            _ => {}
        }
        if let Some(size) = number::<f32>(&v.font_size, "The font size")?
            && !(4.0..=96.0).contains(&size)
        {
            return Err("The font size goes from 4 to 96.".into());
        }
        number::<usize>(&v.scrollback, "Scrollback")?;
        if v.tmux_on
            && !v
                .tmux
                .trim()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err("tmux session names use letters, digits, . _ and - only.".into());
        }
        if v.start_dir.contains('"') || v.start_dir.chars().any(char::is_control) {
            return Err("The start directory cannot hold \" or control characters.".into());
        }
        for (text, what) in [
            (&v.local_folder, "local folder"),
            (&v.remote_folder, "remote folder"),
        ] {
            if text.contains('"') || text.chars().any(char::is_control) {
                return Err(format!("The {what} cannot hold \" or control characters."));
            }
        }
        if matches!(
            v.kind,
            SessionKind::Ssh | SessionKind::Mosh | SessionKind::Serial | SessionKind::Telnet
        ) {
            let mut steps = Vec::new();
            let mut at = Vec::new();
            for (i, step) in v.login_script.iter().enumerate() {
                if !step.trim().is_empty() {
                    steps
                        .push(Step::parse(step).map_err(|e| format!("Login step {}: {e}", i + 1))?);
                    at.push(i);
                }
            }
            if let Some(i) = steps.iter().position(Step::moves) {
                return Err(format!(
                    "Login step {}: a login script types into its own pane; pane and tab \
                     moves are for scripts.",
                    at.get(i).copied().unwrap_or(0) + 1
                ));
            }
            ferossh_config::login::check_flow(&steps).map_err(|(i, e)| {
                format!("Login step {}: {e}", at.get(i).copied().unwrap_or(0) + 1)
            })?;
        }
        port(&v.desktop_port, "The RDP port")?;
        port(&v.vnc_port, "The VNC port")?;
        let size = v.desktop_size.trim();
        if !size.is_empty() && ferossh_config::settings::DesktopSize::parse(size).is_none() {
            return Err(
                "The desktop size is follow, or a width and height such as 1920x1080 \
                 (320x200 to 8192x8192)."
                    .into(),
            );
        }
        if v.virtual_desktop.chars().any(char::is_control) {
            return Err("The virtual desktop program cannot hold control characters.".into());
        }
        if v.remote_app.chars().any(char::is_control) {
            return Err("The remote app cannot hold control characters.".into());
        }
        if v.kind == SessionKind::Mosh {
            let server = v.mosh_server.trim();
            if !server.is_empty() && !ferossh_config::settings::valid_mosh_server(server) {
                return Err(
                    "The Mosh server is a program name or path: letters, digits and / . _ - ~ + only."
                        .into(),
                );
            }
            if !v.mosh_ports.trim().is_empty()
                && ferossh_config::settings::mosh_ports(&v.mosh_ports).is_none()
            {
                return Err(
                    "UDP ports: one port such as 60001, or a range such as 60001:60010.".into(),
                );
            }
        }
        if matches!(
            v.kind,
            SessionKind::Ssh | SessionKind::Mosh | SessionKind::Sftp
        ) && v.password > PASSWORD_WINDOWS
            && v.password_command.trim().is_empty()
        {
            return Err("Fill in the password manager's command, or pick another source.".into());
        }
        Ok(())
    }

    /// Checks, and on a problem shows it and moves to its field.
    fn checked(&mut self, connect: bool) -> Option<FormAction> {
        match self.check() {
            Ok(()) => {
                self.error = None;
                Some(FormAction::Save { connect })
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    /// Handles a key; `rows` is the grid's height.
    pub fn key(&mut self, key: SideKey, rows: usize) -> Option<FormAction> {
        let action = self.key_inner(key);
        self.clamp(rows);
        action
    }

    fn key_inner(&mut self, key: SideKey) -> Option<FormAction> {
        let items = self.items();
        let item = items.get(self.focus).cloned();
        let field = match &item {
            Some(Item::Field(f)) => Some(*f),
            _ => None,
        };
        let editable = field.is_some_and(|f| !f.is_choice() && !self.field_locked(f));
        match key {
            SideKey::Escape => return Some(FormAction::Cancel),
            SideKey::Up => self.move_focus(false, false),
            SideKey::Down => self.move_focus(true, false),
            SideKey::Tab => self.move_focus(true, true),
            SideKey::PageUp => {
                for _ in 0..8 {
                    self.move_focus(false, false);
                }
            }
            SideKey::PageDown => {
                for _ in 0..8 {
                    self.move_focus(true, false);
                }
            }
            SideKey::Enter { split } => match item {
                Some(Item::Section(s)) => {
                    if let Some(o) = self.open.get_mut(s) {
                        *o = !*o;
                    }
                }
                Some(Item::Buttons) => return self.press(),
                Some(Item::Browse) => return Some(FormAction::BrowseKey),
                _ => return self.checked(split),
            },
            SideKey::Left | SideKey::Right => {
                let right = key == SideKey::Right;
                match item {
                    Some(Item::Section(s)) => {
                        if let Some(o) = self.open.get_mut(s) {
                            *o = right;
                        }
                    }
                    Some(Item::Buttons) => {
                        let n = self.buttons().len();
                        self.button = if right {
                            (self.button + 1).min(n - 1)
                        } else {
                            self.button.saturating_sub(1)
                        };
                    }
                    Some(Item::Field(f)) if f.is_choice() => self.cycle(f, right),
                    Some(Item::Field(f)) => {
                        let len = self.text(f).map_or(0, |t| t.chars().count());
                        self.cursor = if right {
                            (self.cursor + 1).min(len)
                        } else {
                            self.cursor.saturating_sub(1)
                        };
                    }
                    _ => {}
                }
            }
            SideKey::Home | SideKey::End => match field {
                Some(f) if editable => {
                    self.cursor = if key == SideKey::Home {
                        0
                    } else {
                        self.text(f).map_or(0, |t| t.chars().count())
                    };
                }
                _ => {
                    let i = if key == SideKey::Home {
                        self.first_field()
                    } else {
                        items.len().saturating_sub(1)
                    };
                    self.set_focus(i);
                }
            },
            SideKey::Char(' ') if field.is_some_and(Field::is_choice) => {
                if let Some(f) = field {
                    self.cycle(f, true);
                }
            }
            SideKey::Char(' ') if item == Some(Item::Buttons) => return self.press(),
            SideKey::Char(' ') if item == Some(Item::Browse) => {
                return Some(FormAction::BrowseKey);
            }
            SideKey::Char(c) if editable && !c.is_control() => {
                let at = self.cursor;
                if let Some(t) = field.and_then(|f| self.text_mut(f))
                    && t.chars().count() < MAX_TEXT
                {
                    let byte = t.char_indices().nth(at).map_or(t.len(), |(b, _)| b);
                    t.insert(byte, c);
                    self.cursor += 1;
                }
                // A new empty line appears below the one being typed.
                self.tidy_forwards();
            }
            SideKey::Backspace | SideKey::Delete if editable => {
                let at = self.cursor;
                let back = key == SideKey::Backspace;
                if let Some(t) = field.and_then(|f| self.text_mut(f)) {
                    let gone = if back { at.checked_sub(1) } else { Some(at) };
                    if let Some(i) = gone
                        && let Some((b, _)) = t.char_indices().nth(i)
                    {
                        t.remove(b);
                        if back {
                            self.cursor -= 1;
                        }
                    }
                }
            }
            SideKey::Char(_)
            | SideKey::Backspace
            | SideKey::Delete
            | SideKey::ToggleFavourite
            | SideKey::Refresh
            | SideKey::Edit
            | SideKey::New => {}
        }
        None
    }

    fn press(&mut self) -> Option<FormAction> {
        match self.buttons().get(self.button).copied() {
            Some("Save") => self.checked(false),
            Some("Connect") => self.checked(true),
            Some("Cancel") => Some(FormAction::Cancel),
            Some("Delete") => Some(FormAction::Delete),
            _ => None,
        }
    }

    /// The key file the picker chose.
    pub fn set_key(&mut self, path: &str) {
        if !self.field_locked(Field::Key) {
            self.values.key = path.to_owned();
            if self.focused_field() == Some(Field::Key) {
                self.cursor = path.chars().count();
            }
        }
    }

    /// A click on grid row `y`, column `x`.
    pub fn click(&mut self, x: usize, y: usize, rows: usize) -> Option<FormAction> {
        let i = y.checked_sub(1)? + self.scroll;
        let items = self.items();
        let item = items.get(i)?.clone();
        if !Self::focusable(&item) {
            return None;
        }
        self.set_focus(i);
        let action = match item {
            Item::Section(_) | Item::Browse => self.key_inner(SideKey::Enter { split: false }),
            Item::Buttons => {
                let hit = self
                    .button_cols
                    .iter()
                    .position(|&(a, b)| (a..b).contains(&x));
                match hit {
                    Some(b) => {
                        self.button = b;
                        self.press()
                    }
                    None => None,
                }
            }
            Item::Field(f) if f.is_choice() => {
                self.cycle(f, true);
                None
            }
            Item::Field(f) => {
                let len = self.text(f).map_or(0, |t| t.chars().count());
                self.cursor = x.saturating_sub(LABEL + 2).min(len);
                None
            }
            _ => None,
        };
        self.clamp(rows);
        action
    }

    pub fn scroll_by(&mut self, lines: i32, rows: usize) {
        let visible = rows.saturating_sub(1).max(1);
        let max = self.items().len().saturating_sub(visible);
        self.scroll = if lines > 0 {
            self.scroll.saturating_sub(lines as usize)
        } else {
            (self.scroll + lines.unsigned_abs() as usize).min(max)
        };
    }

    fn clamp(&mut self, rows: usize) {
        let n = self.items().len();
        self.focus = self.focus.min(n.saturating_sub(1));
        let visible = rows.saturating_sub(1).max(1);
        // The error shows above the buttons; keep both in view.
        let items = self.items();
        let bottom = if matches!(items.get(self.focus), Some(Item::Buttons)) {
            items
                .iter()
                .position(|i| matches!(i, Item::Error(_)))
                .unwrap_or(self.focus)
        } else {
            self.focus
        };
        if bottom < self.scroll {
            self.scroll = bottom;
        }
        if self.focus >= self.scroll + visible {
            self.scroll = self.focus + 1 - visible;
        }
    }

    /// Draws the form into `grid`.
    pub fn draw(&mut self, grid: &mut Grid, palette: &Palette, focused: bool) {
        let width = grid.cols();
        self.width = width;
        let rows = grid.rows();
        self.clamp(rows);
        let bg = palette.strip;
        let red = Rgb::new(0xd0, 0x6a, 0x6a);
        let title = match (&self.original, &self.source) {
            (None, _) => " New connection".to_owned(),
            (Some(n), _) => format!(" Connection {n}"),
        };
        grid.set_row(
            0,
            Line::new(width, bg)
                .put(&title, palette.text, Flags::BOLD)
                .cells(bg),
        );
        let items = self.items();
        let visible = rows.saturating_sub(1);
        self.button_cols.clear();
        for y in 0..visible {
            let i = self.scroll + y;
            let here = focused && i == self.focus;
            let row_bg = if here { palette.active } else { bg };
            let mut line = Line::new(width, row_bg);
            match items.get(i) {
                Some(Item::Section(s)) => {
                    let open = self.open.get(*s).copied().unwrap_or(false);
                    let name = SECTIONS.get(*s).copied().unwrap_or_default();
                    line.put(
                        &format!(" {} {name}", if open { "▾" } else { "▸" }),
                        palette.dim,
                        Flags::BOLD,
                    );
                }
                Some(Item::Field(f)) => self.draw_field(&mut line, *f, here, palette),
                Some(Item::Browse) => {
                    line.put(&format!(" {:LABEL$} ", ""), palette.dim, Flags::empty());
                    line.put("Browse…", palette.text, Flags::UNDERLINE);
                }
                Some(Item::Note(text)) => {
                    line.put(
                        &format!(" {:LABEL$} {text}", ""),
                        palette.dim,
                        Flags::ITALIC,
                    );
                }
                Some(Item::Error(text)) => {
                    line.put(&format!(" {text}"), red, Flags::empty());
                }
                Some(Item::Buttons) => {
                    line.put(" ", palette.text, Flags::empty());
                    for (b, label) in self.buttons().into_iter().enumerate() {
                        let on = here && b == self.button;
                        let start = line.col;
                        let flags = if on {
                            Flags::BOLD.union(Flags::INVERSE)
                        } else {
                            Flags::BOLD
                        };
                        let fg = if label == "Delete" { red } else { palette.text };
                        line.put(&format!(" {label} "), fg, flags);
                        self.button_cols.push((start, line.col));
                        line.put(" ", palette.text, Flags::empty());
                    }
                }
                None => {}
            }
            grid.set_row(1 + y, line.cells(row_bg));
        }
    }

    fn draw_field(&self, line: &mut Line, f: Field, here: bool, palette: &Palette) {
        let label = match f {
            Field::Forward(i) | Field::LoginStep(i) => format!("{} {}", f.label(), i + 1),
            Field::Host if self.values.kind == SessionKind::Serial => "COM port".to_owned(),
            _ => f.label().to_owned(),
        };
        line.put(&format!(" {label:LABEL$} "), palette.dim, Flags::empty());
        let room = line.width.saturating_sub(line.col + 1);
        let locked = self.field_locked(f);
        if f.is_choice() {
            let text = self.choice(f);
            let text = if here {
                format!("‹ {text} ›")
            } else {
                text
            };
            line.put(&fit(&text, room), palette.text, Flags::empty());
            return;
        }
        let value = self.text(f).cloned().unwrap_or_default();
        if value.is_empty() && !(here && !locked) {
            line.put(&fit(&self.hint(f), room), palette.dim, Flags::ITALIC);
            return;
        }
        let fg = if locked { palette.dim } else { palette.text };
        if !here || locked {
            line.put(&fit(&value, room), fg, Flags::empty());
            return;
        }
        // The text around the cursor, with the cursor drawn as a bar.
        let chars: Vec<char> = value.chars().collect();
        let at = self.cursor.min(chars.len());
        let start = at.saturating_sub(room.saturating_sub(2));
        let before: String = chars.get(start..at).unwrap_or_default().iter().collect();
        let after: String = chars.get(at..).unwrap_or_default().iter().collect();
        line.put(&before, fg, Flags::empty());
        line.put("▏", palette.accent, Flags::empty());
        let left = line.width.saturating_sub(line.col);
        line.put(&fit(&after, left), fg, Flags::empty());
        if value.is_empty() {
            let left = line.width.saturating_sub(line.col);
            line.put(&fit(&self.hint(f), left), palette.dim, Flags::ITALIC);
        }
    }

    /// Saves the form into the saved sessions and the settings; returns
    /// the session's name. Call after [`Self::check`] passed.
    pub fn apply(&self, hosts: &mut Vec<SavedHost>, settings: &mut Settings) -> String {
        let v = &self.values;
        let name = v.name.trim().to_owned();
        let trimmed = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
        let renamed_from = self
            .original
            .clone()
            .filter(|o| !o.eq_ignore_ascii_case(&name));
        let is_serial = v.kind == SessionKind::Serial;
        if !self.locked() {
            let host = if is_serial {
                // The port is the host name; nothing of SSH applies.
                SavedHost {
                    alias: name.clone(),
                    folder: trimmed(&v.folder),
                    hostname: serial::port_name(&v.host).filter(|h| *h != name),
                    ..SavedHost::default()
                }
            } else if v.kind == SessionKind::Telnet {
                // A host and a port; no user, keys or forwards.
                SavedHost {
                    alias: name.clone(),
                    folder: trimmed(&v.folder),
                    hostname: trimmed(&v.host).filter(|h| *h != name),
                    port: v.port.trim().parse().ok().filter(|&p| p != 0),
                    ..SavedHost::default()
                }
            } else {
                let ssh = matches!(v.kind, SessionKind::Ssh | SessionKind::Mosh);
                // An SFTP-only login has no shell to forward an agent to
                // or delegate tickets for.
                let login = ssh || v.kind == SessionKind::Sftp;
                SavedHost {
                    alias: name.clone(),
                    folder: trimmed(&v.folder),
                    hostname: trimmed(&v.host).filter(|h| *h != name),
                    user: trimmed(&v.user),
                    port: v.port.trim().parse().ok().filter(|&p| p != 0),
                    identity_file: trimmed(&v.key),
                    proxy_jump: trimmed(&v.jump).filter(|_| login),
                    proxy_server: ProxyServer::parse(&v.proxy)
                        .filter(|_| login)
                        .map(|p| p.to_string()),
                    forward_agent: v.forward_agent && ssh,
                    gssapi: v.kerberos.filter(|_| login),
                    gssapi_delegate: v.kerberos_delegate && ssh,
                    forwards: v.forwards.iter().filter_map(|f| parse_option(f)).fold(
                        Vec::new(),
                        |mut all, f| {
                            if !all.contains(&f) {
                                all.push(f);
                            }
                            all
                        },
                    ),
                }
            };
            let at = match (&self.source, &self.original) {
                (Source::Saved, Some(o)) => {
                    hosts.iter().position(|h| h.alias.eq_ignore_ascii_case(o))
                }
                _ => None,
            };
            match at.and_then(|i| hosts.get_mut(i)) {
                Some(slot) => *slot = host,
                None => hosts.push(host),
            }
        }
        // A renamed saved session takes its favourite star along; a copy
        // leaves the original as it was.
        if self.source == Source::Saved
            && let Some(old) = &renamed_from
        {
            settings
                .profiles
                .retain(|p| !p.host.eq_ignore_ascii_case(old));
            if settings.is_favourite(old) {
                settings.toggle_favourite(old);
                if !settings.is_favourite(&name) {
                    settings.toggle_favourite(&name);
                }
            }
            // File transfer bookmarks for this connection follow it.
            for b in &mut settings.bookmarks {
                if b.host
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(old))
                {
                    b.host = Some(name.clone());
                }
            }
        }
        let p = settings.profile_mut(&name);
        p.kind = v.kind;
        p.via = trimmed(&v.via).filter(|_| {
            matches!(
                v.kind,
                SessionKind::Rdp | SessionKind::Vnc | SessionKind::Xvfb | SessionKind::RemoteApp
            )
        });
        p.theme = trimmed(&v.theme);
        p.font = trimmed(&v.font);
        p.font_size = v.font_size.trim().parse().ok();
        p.scrollback = v.scrollback.trim().parse().ok();
        p.tmux =
            (v.tmux_on && !matches!(v.kind, SessionKind::Mosh | SessionKind::Sftp)).then(|| {
                trimmed(&v.tmux)
                    .filter(|t| {
                        !ferossh_config::settings::tmux_off(t)
                            && !ferossh_config::settings::tmux_on(t)
                    })
                    .unwrap_or_else(|| DEFAULT_TMUX.to_owned())
            });
        p.start_dir = ferossh_config::settings::start_dir(&v.start_dir);
        p.local_folder = ferossh_config::settings::start_dir(&v.local_folder);
        p.remote_folder = ferossh_config::settings::start_dir(&v.remote_folder);
        p.tmux_layout = trimmed(&v.tmux_layout);
        p.desktop_port = v.desktop_port.trim().parse().ok().filter(|&p| p != 0);
        p.vnc_port = v.vnc_port.trim().parse().ok().filter(|&p| p != 0);
        p.virtual_desktop = ferossh_config::settings::virtual_desktop(&v.virtual_desktop);
        p.remote_app = ferossh_config::settings::virtual_desktop(&v.remote_app);
        p.remote_app_copy_agent = v.copy_agent;
        p.remote_app_remove_agent = v.remove_agent;
        p.remote_app_clipboard = Some(v.clipboard.clone()).filter(|c| !c.is_empty());
        p.desktop_clipboard = Some(v.desk_clipboard.clone()).filter(|c| !c.is_empty());
        p.desktop_size = ferossh_config::settings::DesktopSize::parse(&v.desktop_size)
            .filter(|d| *d != ferossh_config::settings::DesktopSize::Follow);
        p.legacy_algorithms = v.legacy;
        p.password = match (v.kind, v.password) {
            (SessionKind::Ssh | SessionKind::Mosh | SessionKind::Sftp, PASSWORD_WINDOWS) => {
                PasswordSource::Windows
            }
            (SessionKind::Ssh | SessionKind::Mosh | SessionKind::Sftp, i)
                if i > PASSWORD_WINDOWS =>
            {
                PasswordSource::Command(v.password_command.trim().to_owned())
            }
            _ => PasswordSource::Ask,
        };
        p.login_script = if matches!(
            v.kind,
            SessionKind::Ssh | SessionKind::Mosh | SessionKind::Serial | SessionKind::Telnet
        ) {
            v.login_script
                .iter()
                .filter_map(|t| Step::parse(t).ok())
                .collect()
        } else {
            Vec::new()
        };
        let mosh = v.kind == SessionKind::Mosh;
        p.mosh_server = trimmed(&v.mosh_server)
            .filter(|s| mosh && s != ferossh_config::settings::DEFAULT_MOSH_SERVER);
        p.mosh_ports = ferossh_config::settings::mosh_ports(&v.mosh_ports).filter(|_| mosh);
        p.serial = is_serial.then(|| {
            let mut line = LineSettings {
                flow: v.flow,
                ..LineSettings::default()
            };
            if let Ok(speed) = v.speed.trim().parse() {
                line.speed = speed;
            }
            if !v.framing.is_empty() {
                line.set_framing(&v.framing);
            }
            line
        });
        name
    }
}

/// Removes saved session `name` with its profile and favourite star.
pub fn delete(name: &str, hosts: &mut Vec<SavedHost>, settings: &mut Settings) {
    hosts.retain(|h| !h.alias.eq_ignore_ascii_case(name));
    settings
        .profiles
        .retain(|p| !p.host.eq_ignore_ascii_case(name));
    if settings.is_favourite(name) {
        settings.toggle_favourite(name);
    }
    // Its File transfer bookmarks go with it: no other connection shows
    // them, and their server folders are its server's.
    settings.bookmarks.retain(|b| {
        !b.host
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(name))
    });
}

/// Breaks `text` into lines of at most `width` characters at spaces.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let need = line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
        if !line.is_empty() && need > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn number<T: std::str::FromStr>(text: &str, what: &str) -> Result<Option<T>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse()
        .map(Some)
        .map_err(|_| format!("{what} is not a number."))
}

fn port(text: &str, what: &str) -> Result<(), String> {
    match number::<u64>(text, what)? {
        Some(1..=65535) | None => Ok(()),
        Some(_) => Err(format!("{what} goes from 1 to 65535.")),
    }
}

/// A row being filled from the left.
pub(crate) struct Line {
    cells: Vec<Cell>,
    pub(crate) width: usize,
    pub(crate) col: usize,
    bg: Rgb,
}

impl Line {
    pub(crate) fn new(width: usize, bg: Rgb) -> Self {
        Self {
            cells: Vec::with_capacity(width),
            width,
            col: 0,
            bg,
        }
    }

    pub(crate) fn put(&mut self, text: &str, fg: Rgb, flags: Flags) -> &mut Self {
        for c in text.chars().filter(|c| !c.is_control()) {
            let w = c.width().unwrap_or(0);
            if w == 0 || self.col + w > self.width {
                continue;
            }
            let mut f = flags;
            if w == 2 {
                f.insert(Flags::WIDE);
            }
            self.cells
                .push(Cell::new(c, Color::Rgb(fg), Color::Rgb(self.bg), f));
            if w == 2 {
                self.cells.push(Cell::new(
                    ' ',
                    Color::Rgb(fg),
                    Color::Rgb(self.bg),
                    Flags::WIDE_SPACER,
                ));
            }
            self.col += w;
        }
        self
    }

    pub(crate) fn cells(&mut self, bg: Rgb) -> Vec<Cell> {
        let mut cells = std::mem::take(&mut self.cells);
        cells.resize(
            self.width,
            Cell::new(' ', Color::Rgb(bg), Color::Rgb(bg), Flags::empty()),
        );
        cells.truncate(self.width);
        cells
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_render::Theme;

    fn hints() -> Hints {
        Hints {
            local_user: "jurgen".into(),
            font: "Cascadia Mono".into(),
            font_size: 11.0,
            scrollback: 10_000,
        }
    }

    fn new_form() -> SessionForm {
        SessionForm::new(
            None,
            Source::New,
            Values::default(),
            hints(),
            vec!["web".into(), "bastion".into()],
            vec!["Nord".into(), "Dracula".into()],
        )
    }

    fn typed(f: &mut SessionForm, text: &str) {
        for c in text.chars() {
            f.key(SideKey::Char(c), 30);
        }
    }

    fn texts(f: &mut SessionForm) -> Vec<String> {
        let mut grid = Grid::new(48, 30);
        f.draw(&mut grid, &Palette::of(&Theme::default()), true);
        (0..30)
            .map(|y| {
                grid.row(y)
                    .unwrap()
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn down(f: &mut SessionForm, n: usize) {
        for _ in 0..n {
            f.key(SideKey::Down, 30);
        }
    }

    #[test]
    fn new_session_shows_defaults_and_saves() {
        let mut f = new_form();
        let t = texts(&mut f);
        assert_eq!(t[0], " New connection");
        assert_eq!(t[1], " ▾ Connection");
        assert!(t[2].starts_with(" Name        ▏required"), "{:?}", t[2]);
        assert!(
            t[6].contains("User") && t[6].contains("jurgen"),
            "{:?}",
            t[6]
        );
        assert!(t[7].contains("22"));
        assert!(t[8].contains("default keys and agent"));
        assert_eq!(t[9].trim(), "Browse…");
        assert!(t[10].contains("ask every time"));
        assert!(t[11].contains("Jump host") && t[11].contains("none, or e.g. bastion"));
        assert!(t[12].contains("Proxy") && t[12].contains("socks5://"));
        assert_eq!(t[13], " ▸ Look");
        assert_eq!(t[14], " ▸ Extras");
        assert!(t[15].contains("Save") && t[15].contains("Connect") && !t[15].contains("Delete"));

        assert_eq!(f.key(SideKey::Enter { split: false }, 30), None);
        assert!(
            texts(&mut f)
                .iter()
                .any(|l| l.contains("Give the connection a name"))
        );
        typed(&mut f, "db");
        down(&mut f, 3);
        typed(&mut f, "10.0.0.5");
        assert!(texts(&mut f)[5].contains("10.0.0.5▏"));
        assert_eq!(
            f.key(SideKey::Enter { split: true }, 30),
            Some(FormAction::Save { connect: true })
        );
        let mut hosts = Vec::new();
        let mut settings = Settings::default();
        assert_eq!(f.apply(&mut hosts, &mut settings), "db");
        assert_eq!(hosts[0].hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(hosts[0].user, None);
        assert_eq!(hosts[0].proxy_jump, None);
        assert!(settings.profile("db").unwrap().theme.is_none());

        // A jump host is saved as the session's ProxyJump.
        f.values.jump = " admin@gw:2222,bastion ".into();
        let mut hosts = Vec::new();
        f.apply(&mut hosts, &mut settings);
        assert_eq!(
            hosts[0].proxy_jump.as_deref(),
            Some("admin@gw:2222,bastion")
        );
        // A proxy too: that belongs on the jump host.
        f.values.proxy = "socks5://proxy".into();
        assert!(f.check().unwrap_err().contains("first jump host"));
        f.values.jump.clear();
        assert_eq!(f.check(), Ok(()));
        let mut hosts = Vec::new();
        f.apply(&mut hosts, &mut settings);
        assert_eq!(
            hosts[0].proxy_server.as_deref(),
            Some("socks5://proxy:1080")
        );
        assert_eq!(Values::from_saved(&hosts[0]).proxy, "socks5://proxy:1080");
    }

    #[test]
    fn kerberos_is_auto_on_or_off_and_saved() {
        let mut f = new_form();
        typed(&mut f, "ad");
        f.values.host = "srv.corp.example".into();
        assert_eq!(
            f.options(Field::Kerberos),
            ["auto: when Windows is in a domain", "on", "off"]
        );
        let mut hosts = Vec::new();
        let mut settings = Settings::default();
        f.apply(&mut hosts, &mut settings);
        assert_eq!((hosts[0].gssapi, hosts[0].gssapi_delegate), (None, false));
        f.cycle(Field::Kerberos, true);
        f.cycle(Field::KerberosDelegate, true);
        let mut hosts = Vec::new();
        f.apply(&mut hosts, &mut settings);
        assert_eq!(
            (hosts[0].gssapi, hosts[0].gssapi_delegate),
            (Some(true), true)
        );
        f.cycle(Field::Kerberos, true);
        let mut hosts = Vec::new();
        f.apply(&mut hosts, &mut settings);
        assert_eq!(hosts[0].gssapi, Some(false));
        let v = Values::from_saved(&hosts[0]);
        assert_eq!((v.kerberos, v.kerberos_delegate), (Some(false), true));
    }

    #[test]
    fn refuses_bad_values() {
        let mut f = new_form();
        for (field, value, problem) in [
            (Field::Name, "web", "already exists"),
            (Field::Name, "my web", "letters, digits"),
            (Field::Host, "a b", "not a host name"),
            (Field::Port, "70000", "1 to 65535"),
            (Field::Port, "0", "1 to 65535"),
            (Field::Forward(0), "L 8080", "Forward 1"),
            (Field::FontSize, "200", "4 to 96"),
            (Field::TmuxSession, "a;b", "tmux"),
            (Field::StartDir, "/srv/\"x\"", "start directory"),
            (Field::VirtualDesktop, "xterm\u{7}", "virtual desktop"),
            (Field::DesktopSize, "100x100", "desktop size"),
            (Field::Jump, "gw host", "Jump host"),
            (Field::Jump, "gw,ok", "not this one"),
            (Field::Proxy, "proxy:1080", "socks5://[user@]host"),
            (Field::Proxy, "socks5://me:pw@proxy", "no password"),
        ] {
            f.values = Values {
                name: "ok".into(),
                forwards: vec![String::new()],
                tmux_on: true,
                ..Values::default()
            };
            *f.text_mut(field).unwrap() = value.into();
            let e = f.check().unwrap_err();
            assert!(e.contains(problem), "{value}: {e}");
        }
        f.values = Values {
            name: "desk".into(),
            kind: SessionKind::Vnc,
            ..Values::default()
        };
        assert!(f.check().unwrap_err().contains("Via SSH"));
        f.values.via = "nowhere".into();
        assert!(
            f.check()
                .unwrap_err()
                .contains("no connection named nowhere")
        );
        f.values.via = "bastion".into();
        assert_eq!(f.check(), Ok(()));
        f.values.kind = SessionKind::Rdp;
        f.values.via.clear();
        assert_eq!(f.check(), Ok(()), "RDP may go straight to the machine");
    }

    #[test]
    fn a_virtual_desktop_connection_needs_via_and_keeps_its_size() {
        let mut f = new_form();
        f.values = Values {
            name: "lab-desk".into(),
            kind: SessionKind::Xvfb,
            ..Values::default()
        };
        assert!(f.check().unwrap_err().contains("Via SSH"));
        f.values.via = "bastion".into();
        f.values.desktop_size = "1920x1080".into();
        f.values.virtual_desktop = "firefox".into();
        assert_eq!(f.check(), Ok(()));
        let items = f.items();
        for field in [
            Field::Via,
            Field::VirtualDesktop,
            Field::DesktopSize,
            Field::DeskClipboard,
        ] {
            assert!(items.contains(&Item::Field(field)), "{field:?}");
        }
        assert!(!items.contains(&Item::Field(Field::Host)));
        // The clipboard only narrows: unset, to-host, from-host, off.
        assert_eq!(
            f.choice(Field::DeskClipboard),
            "as in Session > Desktop clipboard"
        );
        f.cycle(Field::DeskClipboard, true);
        f.cycle(Field::DeskClipboard, true);
        assert_eq!(f.choice(Field::DeskClipboard), "only host to Windows");
        assert_eq!(f.values.clipboard, "", "the remote apps' stays");
        let mut hosts = Vec::new();
        let mut settings = Settings::default();
        f.apply(&mut hosts, &mut settings);
        let p = settings.profile("lab-desk").unwrap();
        assert_eq!(p.desktop_clipboard.as_deref(), Some("from-host"));
        assert_eq!(p.kind, SessionKind::Xvfb);
        assert_eq!(p.via.as_deref(), Some("bastion"));
        assert_eq!(p.virtual_desktop.as_deref(), Some("firefox"));
        assert_eq!(
            p.desktop_size,
            Some(ferossh_config::settings::DesktopSize::Fixed(1920, 1080))
        );
    }

    #[test]
    fn a_remote_app_connection_needs_via_and_keeps_its_agent_choices() {
        let mut f = new_form();
        f.values = Values {
            name: "gimp-box".into(),
            kind: SessionKind::RemoteApp,
            ..Values::default()
        };
        assert!(f.check().unwrap_err().contains("Via SSH"));
        f.values.via = "bastion".into();
        f.values.remote_app = "gimp ~/a.png".into();
        assert_eq!(f.check(), Ok(()));
        let items = f.items();
        for field in [
            Field::Via,
            Field::RemoteApp,
            Field::CopyAgent,
            Field::RemoveAgent,
            Field::Clipboard,
        ] {
            assert!(items.contains(&Item::Field(field)), "{field:?}");
        }
        assert!(!items.contains(&Item::Field(Field::Host)));
        assert!(!items.contains(&Item::Field(Field::User)));
        // Unset, yes, no, and round again.
        assert_eq!(f.choice(Field::RemoveAgent), "as in Session > Remote apps");
        f.cycle(Field::RemoveAgent, true);
        assert_eq!(f.values.remove_agent, Some(true));
        f.cycle(Field::CopyAgent, true);
        f.cycle(Field::CopyAgent, true);
        assert_eq!(f.values.copy_agent, Some(false));
        let mut hosts = Vec::new();
        let mut settings = Settings::default();
        f.apply(&mut hosts, &mut settings);
        let p = settings.profile("gimp-box").unwrap();
        assert_eq!(p.kind, SessionKind::RemoteApp);
        assert_eq!(p.via.as_deref(), Some("bastion"));
        assert_eq!(p.remote_app.as_deref(), Some("gimp ~/a.png"));
        assert_eq!(p.remote_app_copy_agent, Some(false));
        assert_eq!(p.remote_app_remove_agent, Some(true));
        assert_eq!(p.remote_app_clipboard, None);
        // The clipboard only narrows: unset, to-host, from-host, off.
        assert_eq!(f.choice(Field::Clipboard), "as in Session > Remote apps");
        f.cycle(Field::Clipboard, false);
        assert_eq!(f.choice(Field::Clipboard), "off for this host");
        f.apply(&mut hosts, &mut settings);
        let p = settings.profile("gimp-box").unwrap();
        assert_eq!(p.remote_app_clipboard.as_deref(), Some("off"));
        f.values.remote_app = "a\u{7}".into();
        assert!(f.check().is_err());
    }

    #[test]
    fn desktop_sessions_have_no_terminal_sections() {
        let mut f = new_form();
        typed(&mut f, "office");
        down(&mut f, 2);
        f.key(SideKey::Right, 30);
        let t = texts(&mut f);
        assert!(t[4].contains("‹ Remote desktop (RDP) ›"), "{:?}", t[4]);
        assert!(t[7].contains("3389"));
        assert!(t[8].contains("Via SSH") && t[8].contains("connect directly"));
        assert!(!t.iter().any(|l| l.contains("Look") || l.contains("Extras")));
        f.key(SideKey::Char(' '), 30);
        assert!(texts(&mut f)[6].contains("asked by the server"));
    }

    #[test]
    fn serial_sessions_save_the_port_and_line() {
        let mut f = new_form();
        typed(&mut f, "core");
        down(&mut f, 2);
        // Type: SSH -> RDP -> VNC -> Telnet -> Serial.
        f.key(SideKey::Left, 30);
        let t = texts(&mut f);
        assert!(t[4].contains("‹ Serial console ›"), "{:?}", t[4]);
        assert!(
            t[5].contains("COM port") && t[5].contains("e.g. COM3"),
            "{:?}",
            t[5]
        );
        assert!(t[6].contains("Speed") && t[6].contains("9600"));
        assert!(t[7].contains("8N1") && t[8].contains("none"));
        assert!(
            !t.iter()
                .any(|l| l.contains("User") || l.contains("Forward"))
        );
        assert!(
            t.iter().any(|l| l.contains("Extras")),
            "for the login script"
        );
        assert!(t.iter().any(|l| l.contains("Look")));
        assert!(f.check().unwrap_err().contains("COM port"));
        f.values.host = "com3".into();
        f.values.speed = "9".into();
        assert!(f.check().unwrap_err().contains("speed"));
        f.values.speed = "115200".into();
        f.cycle(Field::Framing, true);
        f.cycle(Field::Flow, false);
        assert_eq!(f.check(), Ok(()));
        let (mut hosts, mut settings) = (Vec::new(), Settings::default());
        f.apply(&mut hosts, &mut settings);
        assert_eq!(hosts[0].hostname.as_deref(), Some("COM3"));
        assert_eq!(hosts[0].user, None);
        let p = settings.profile("core").unwrap();
        assert_eq!(p.kind, SessionKind::Serial);
        assert_eq!(
            p.serial.map(|l| l.to_string()).as_deref(),
            Some("115200,8E1,dsrdtr")
        );
        // And back into the form.
        let v = Values::from_saved(&hosts[0]).with_profile(Some(p));
        assert_eq!((v.speed.as_str(), v.framing.as_str()), ("115200", "8E1"));
        assert_eq!(v.flow, Flow::DsrDtr);
    }

    #[test]
    fn telnet_sessions_save_a_host_and_port_and_warn() {
        let mut f = new_form();
        typed(&mut f, "router");
        down(&mut f, 2);
        // Type: SSH -> Serial -> Telnet, going back.
        f.key(SideKey::Left, 30);
        f.key(SideKey::Left, 30);
        let t = texts(&mut f);
        assert!(t[4].contains("‹ Telnet (unencrypted) ›"), "{:?}", t[4]);
        assert!(t[6].contains("Port") && t[6].contains("23"), "{:?}", t[6]);
        assert!(t.iter().any(|l| l.contains("Unencrypted")));
        assert!(!t.iter().any(|l| l.contains("User")));
        // Look, and Extras for the login script.
        assert!(t.iter().any(|l| l.contains("Look")));
        assert!(t.iter().any(|l| l.contains("Extras")));
        f.values.host = "10.0.0.1".into();
        f.values.port = "2003".into();
        // A user typed before switching to Telnet is not checked or kept.
        f.values.user = "left over".into();
        assert_eq!(f.check(), Ok(()));
        let (mut hosts, mut settings) = (Vec::new(), Settings::default());
        f.apply(&mut hosts, &mut settings);
        assert_eq!(hosts[0].hostname.as_deref(), Some("10.0.0.1"));
        assert_eq!(
            (hosts[0].port, hosts[0].user.as_deref()),
            (Some(2003), None)
        );
        let p = settings.profile("router").unwrap();
        assert_eq!((p.kind, p.via.as_deref()), (SessionKind::Telnet, None));
        // Login steps are kept for Telnet sessions.
        f.values.login_script = vec!["expect Username:".into(), "send admin".into()];
        assert_eq!(f.check(), Ok(()));
        f.apply(&mut hosts, &mut settings);
        assert_eq!(settings.profile("router").unwrap().login_script.len(), 2);
    }

    #[test]
    fn mosh_sessions_log_in_like_ssh_without_tmux() {
        let mut f = new_form();
        typed(&mut f, "laptop");
        down(&mut f, 2);
        // Type: SSH -> Serial -> Telnet -> SFTP -> Mosh, going back.
        for _ in 0..4 {
            f.key(SideKey::Left, 30);
        }
        assert_eq!(f.values.kind, SessionKind::Mosh);
        let t = texts(&mut f);
        assert!(t[4].contains("‹ Mosh terminal (SSH + UDP) ›"), "{:?}", t[4]);
        assert!(t.iter().any(|l| l.contains("Key file")));
        assert!(
            t.iter()
                .any(|l| l.contains("Mosh server") && l.contains("mosh-server"))
        );
        assert!(t.iter().any(|l| l.contains("UDP ports")));
        let rows = f.native_rows();
        assert!(rows.iter().any(|r| r.label == "Start dir"));
        assert!(!rows.iter().any(|r| r.label == "tmux"), "no tmux over Mosh");
        assert!(!rows.iter().any(|r| r.label == "RDP port"));
        f.values.host = "box.example.org".into();
        f.values.user = "me".into();
        f.values.tmux_on = true;
        f.values.mosh_server = "mosh-server; reboot".into();
        assert!(f.check().is_err());
        f.values.mosh_server = "~/bin/mosh-server".into();
        f.values.mosh_ports = "60001-".into();
        assert!(f.check().is_err());
        f.values.mosh_ports = "60001-60010".into();
        assert_eq!(f.check(), Ok(()));
        let (mut hosts, mut settings) = (Vec::new(), Settings::default());
        f.apply(&mut hosts, &mut settings);
        assert_eq!(hosts[0].user.as_deref(), Some("me"));
        let p = settings.profile("laptop").unwrap();
        assert_eq!(p.kind, SessionKind::Mosh);
        assert_eq!(p.mosh_server.as_deref(), Some("~/bin/mosh-server"));
        assert_eq!(p.mosh_ports, Some((60001, 60010)));
        assert_eq!(p.tmux, None);
        // Back as it was saved.
        let again = Values::default().with_profile(Some(p));
        assert_eq!(again.mosh_ports, "60001:60010");
        // Another type keeps no Mosh settings.
        f.values.kind = SessionKind::Ssh;
        f.apply(&mut hosts, &mut settings);
        let p = settings.profile("laptop").unwrap();
        assert_eq!((p.mosh_server.as_deref(), p.mosh_ports), (None, None));
    }

    #[test]
    fn sftp_sessions_log_in_without_a_terminal() {
        let mut f = new_form();
        typed(&mut f, "uploads");
        down(&mut f, 2);
        // Type: SSH -> Serial -> Telnet -> SFTP, going back.
        for _ in 0..3 {
            f.key(SideKey::Left, 30);
        }
        assert_eq!(f.values.kind, SessionKind::Sftp);
        let t = texts(&mut f);
        assert!(t[4].contains("‹ SFTP file transfer only ›"), "{:?}", t[4]);
        assert!(t.iter().any(|l| l.contains("Key file")));
        assert!(t.iter().any(|l| l.contains("opens File transfer")));
        let rows = f.native_rows();
        for label in ["Jump host", "Remote folder", "Kerberos"] {
            assert!(rows.iter().any(|r| r.label == label), "{label}");
        }
        for label in ["Theme", "tmux", "Start dir", "RDP port"] {
            assert!(!rows.iter().any(|r| r.label == label), "{label}");
        }
        f.values.host = "files.example.org".into();
        f.values.user = "up".into();
        f.values.jump = "bastion".into();
        f.values.remote_folder = "/incoming".into();
        f.values.tmux_on = true;
        assert_eq!(f.check(), Ok(()));
        let (mut hosts, mut settings) = (Vec::new(), Settings::default());
        f.apply(&mut hosts, &mut settings);
        assert_eq!(hosts[0].proxy_jump.as_deref(), Some("bastion"));
        let p = settings.profile("uploads").unwrap();
        assert_eq!(p.kind, SessionKind::Sftp);
        assert_eq!(p.remote_folder.as_deref(), Some("/incoming"));
        assert_eq!(p.tmux, None, "no shell for tmux");
        // Edited again, it is still an SFTP-only connection.
        let v = Values::from_saved(&hosts[0]).with_profile(Some(p));
        assert_eq!(v.kind, SessionKind::Sftp);
        assert_eq!(v.jump, "bastion");
        assert_eq!(v.remote_folder, "/incoming");
    }

    #[test]
    fn forwards_grow_a_new_line() {
        let mut f = new_form();
        typed(&mut f, "web2");
        // Open Extras: Name, Folder, Type, Host, User, Port, Key, Browse,
        // Password, Jump host, Proxy, Look, Extras.
        down(&mut f, 12);
        f.key(SideKey::Enter { split: false }, 30);
        // tmux, tmux panes, Start dir, Local folder, Remote folder.
        down(&mut f, 6);
        assert_eq!(f.focused_field(), Some(Field::Forward(0)));
        typed(&mut f, "L 8080 localhost:80");
        let t = texts(&mut f);
        assert!(t.iter().any(|l| l.starts_with(" Forward 2")), "{t:#?}");
        f.key(SideKey::Down, 30);
        typed(&mut f, "D 1080");
        f.key(SideKey::Up, 30);
        f.key(SideKey::Home, 30);
        for _ in 0..30 {
            f.key(SideKey::Delete, 30);
        }
        f.key(SideKey::Down, 30);
        assert_eq!(f.values.forwards, ["", "D 1080", ""]);
        assert_eq!(f.focused_field(), Some(Field::Forward(1)));
        assert_eq!(
            f.key(SideKey::Enter { split: false }, 30),
            Some(FormAction::Save { connect: false })
        );
        let mut hosts = Vec::new();
        f.apply(&mut hosts, &mut Settings::default());
        assert_eq!(hosts[0].forwards[0].to_string(), "-D 1080");
    }

    #[test]
    fn login_steps_are_checked_and_saved_in_the_profile() {
        let mut f = new_form();
        typed(&mut f, "sw1");
        down(&mut f, 12);
        f.key(SideKey::Enter { split: false }, 30);
        // tmux, tmux panes, Start dir, Local folder, Remote folder,
        // Forward 1, RDP port, VNC port, Virtual desk, Desk size, Desk
        // clip, Remote app, Copy agent, Clean up, App clip, Fwd agent,
        // Kerberos, Fwd ticket, Old crypto.
        down(&mut f, 20);
        assert_eq!(f.focused_field(), Some(Field::LoginStep(0)));
        let t = texts(&mut f);
        assert!(
            t.iter().any(|l| l.contains("e.g. expect Password:")),
            "{t:#?}"
        );
        typed(&mut f, "expect >");
        f.key(SideKey::Down, 30);
        typed(&mut f, "secret en able");
        let err = f.check().unwrap_err();
        assert!(err.starts_with("Login step 2:"), "{err}");
        f.key(SideKey::Backspace, 30);
        f.key(SideKey::Backspace, 30);
        f.key(SideKey::Backspace, 30);
        f.key(SideKey::Backspace, 30);
        f.key(SideKey::Backspace, 30);
        typed(&mut f, "able");
        assert_eq!(f.check(), Ok(()));
        assert_eq!(f.values.login_script, ["expect >", "secret enable", ""]);
        let (mut hosts, mut settings) = (Vec::new(), Settings::default());
        f.apply(&mut hosts, &mut settings);
        let p = settings.profile("sw1").unwrap();
        assert_eq!(
            p.login_script,
            [Step::Expect(">".into()), Step::Secret("enable".into())]
        );
        let v = Values::from_saved(&hosts[0]).with_profile(Some(p));
        assert_eq!(v.login_script, ["expect >", "secret enable"]);
    }

    #[test]
    fn tmux_off_is_no_tmux_and_the_start_dir_is_kept() {
        let mut f = new_form();
        f.values = Values {
            name: "debian2".into(),
            host: "192.168.1.159".into(),
            tmux: " Off ".into(),
            start_dir: " /srv/my site ".into(),
            local_folder: r"D:\Sites\debian".into(),
            remote_folder: "/var/www ".into(),
            forwards: vec![String::new()],
            ..Values::default()
        };
        assert_eq!(f.check(), Ok(()));
        let mut hosts = Vec::new();
        let mut settings = Settings::default();
        f.apply(&mut hosts, &mut settings);
        let p = settings.profile("debian2").unwrap();
        assert_eq!(p.tmux, None, "typing off turns tmux off");
        assert_eq!(p.start_dir.as_deref(), Some("/srv/my site"));
        let again = Settings::parse(&settings.to_text());
        let back = Values::default().with_profile(again.profile("debian2"));
        assert!(!back.tmux_on);
        assert_eq!(back.tmux, "");
        assert_eq!(back.start_dir, "/srv/my site");
        assert_eq!(back.local_folder, r"D:\Sites\debian");
        assert_eq!(back.remote_folder, "/var/www");

        // On with no name, or a name of on or off, is session main.
        for name in ["", "on", "off", " ON "] {
            f.values.tmux_on = true;
            f.values.tmux = name.into();
            assert_eq!(f.check(), Ok(()), "{name}");
            f.apply(&mut hosts, &mut settings);
            let p = settings.profile("debian2").unwrap();
            assert_eq!(p.tmux.as_deref(), Some("main"), "{name:?}");
        }
        f.values.tmux = "work".into();
        f.apply(&mut hosts, &mut settings);
        assert_eq!(
            settings.profile("debian2").unwrap().tmux.as_deref(),
            Some("work")
        );
        let back = Values::default().with_profile(settings.profile("debian2"));
        assert!(back.tmux_on);
        assert_eq!(back.tmux, "work");
        // Turned off, the name goes.
        f.values.tmux_on = false;
        f.apply(&mut hosts, &mut settings);
        assert_eq!(settings.profile("debian2").unwrap().tmux, None);
    }

    #[test]
    fn tmux_is_switched_on_and_off_not_typed() {
        let mut f = new_form();
        typed(&mut f, "web2");
        // Open Extras: Name, Folder, Type, Host, User, Port, Key, Browse,
        // Password, Jump host, Proxy, Look, Extras.
        down(&mut f, 12);
        f.key(SideKey::Enter { split: false }, 30);
        f.key(SideKey::Down, 30);
        assert_eq!(f.focused_field(), Some(Field::Tmux));
        typed(&mut f, "off");
        assert!(!f.values.tmux_on, "typing does not change a switch");
        assert!(f.values.tmux.is_empty());
        f.key(SideKey::Right, 30);
        assert!(f.values.tmux_on);
        let t = texts(&mut f);
        assert!(
            t.iter().any(|l| l.contains("on: attach after login")),
            "{t:#?}"
        );
        assert!(
            t.iter()
                .any(|l| l.contains("tmux name") && l.contains("main")),
            "{t:#?}"
        );
        f.key(SideKey::Down, 30);
        assert_eq!(f.focused_field(), Some(Field::TmuxSession));
        typed(&mut f, "work");
        assert_eq!(f.values.tmux, "work");
        f.key(SideKey::Up, 30);
        f.key(SideKey::Left, 30);
        assert!(!f.values.tmux_on);
        let t = texts(&mut f);
        assert!(!t.iter().any(|l| l.contains("tmux name")), "{t:#?}");
    }

    #[test]
    fn read_only_hosts_save_only_the_profile_or_a_copy() {
        let values = Values::from_resolved(
            "web",
            Some("Work"),
            "web.example.org",
            "admin",
            2222,
            None,
            "",
            &[],
        );
        let mut f = SessionForm::new(
            Some("web".into()),
            Source::ReadOnly("~/.ssh/config".into()),
            values,
            hints(),
            vec!["web".into()],
            vec!["Nord".into(), "Dracula".into()],
        );
        let t = texts(&mut f);
        assert!(t[3].contains("From ~/.ssh/config: read-only."));
        // Folder, host, user, port cannot change.
        down(&mut f, 3);
        assert_eq!(f.focused_field(), Some(Field::Host));
        typed(&mut f, "x");
        assert_eq!(f.values.host, "web.example.org");
        // The theme can: past user, port, key, password, jump host and
        // proxy to Look.
        down(&mut f, 7);
        f.key(SideKey::Enter { split: false }, 30);
        f.key(SideKey::Down, 30);
        f.key(SideKey::Right, 30);
        f.key(SideKey::Right, 30);
        assert_eq!(f.values.theme, "Dracula");
        let mut hosts = Vec::new();
        let mut settings = Settings::default();
        assert_eq!(f.check(), Ok(()));
        f.apply(&mut hosts, &mut settings);
        assert!(hosts.is_empty(), "nothing goes to sessions.conf");
        assert_eq!(
            settings.profile("web").unwrap().theme.as_deref(),
            Some("Dracula")
        );

        // A new name makes an editable copy.
        f.key(SideKey::Home, 30);
        f.key(SideKey::End, 30);
        typed(&mut f, "-copy");
        assert!(texts(&mut f)[3].contains("Saves a copy"));
        down(&mut f, 3);
        f.key(SideKey::End, 30);
        f.key(SideKey::Backspace, 30);
        typed(&mut f, "g.lan");
        assert_eq!(f.values.host, "web.example.org.lan");
        f.apply(&mut hosts, &mut settings);
        assert_eq!(hosts[0].alias, "web-copy");
        assert_eq!(hosts[0].port, Some(2222));
        assert_eq!(hosts[0].folder.as_deref(), Some("Work"));
        assert!(
            settings.profile("web").is_some(),
            "the original keeps its look"
        );
    }

    #[test]
    fn renaming_a_saved_session_moves_it_and_its_star() {
        let saved_host = SavedHost {
            alias: "old".into(),
            hostname: Some("h".into()),
            ..SavedHost::default()
        };
        let mut hosts = vec![saved_host.clone()];
        let mut settings = Settings::default();
        settings.toggle_favourite("old");
        settings.profile_mut("old").theme = Some("Nord".into());
        let mark = |name: &str, host: Option<&str>| ferossh_config::settings::Bookmark {
            name: name.into(),
            local: None,
            remote: Some("/srv".into()),
            host: host.map(Into::into),
        };
        settings.bookmarks = vec![
            mark("logs", Some("OLD")),
            mark("everywhere", None),
            mark("other", Some("other")),
        ];
        let values = Values::from_saved(&saved_host).with_profile(settings.profile("old"));
        let mut f = SessionForm::new(
            Some("old".into()),
            Source::Saved,
            values,
            hints(),
            vec!["old".into()],
            vec![],
        );
        assert!(texts(&mut f).iter().any(|l| l.contains("Delete")));
        f.values.name = "new".into();
        assert_eq!(f.check(), Ok(()));
        f.apply(&mut hosts, &mut settings);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "new");
        assert!(settings.is_favourite("new") && !settings.is_favourite("old"));
        assert_eq!(
            settings.profile("new").unwrap().theme.as_deref(),
            Some("Nord")
        );
        assert!(settings.profile("old").is_none());
        // Its bookmarks follow the new name; the others stay as they were.
        let hosts_of = |s: &Settings| -> Vec<Option<String>> {
            s.bookmarks.iter().map(|b| b.host.clone()).collect()
        };
        assert_eq!(
            hosts_of(&settings),
            [Some("new".into()), None, Some("other".into())]
        );
        delete("new", &mut hosts, &mut settings);
        assert!(hosts.is_empty() && settings.favourites.is_empty());
        // And go with it when it is deleted.
        assert_eq!(hosts_of(&settings), [None, Some("other".into())]);
    }

    /// A choice row's options and selection; none for another row.
    fn choice(r: &NativeRow) -> (Vec<String>, usize) {
        match &r.kind {
            NativeKind::Choice { options, selected } => (options.clone(), *selected),
            _ => (Vec::new(), usize::MAX),
        }
    }

    fn row<'a>(rows: &'a [NativeRow], label: &str) -> &'a NativeRow {
        rows.iter().find(|r| r.label == label).unwrap()
    }

    #[test]
    fn native_rows_cover_every_section_and_follow_the_type() {
        let mut f = new_form();
        assert_eq!(
            f.sections(),
            vec![(0, "Connection"), (1, "Look"), (2, "Extras")]
        );
        let rows = f.native_rows();
        assert_eq!(row(&rows, "Name").section, 0);
        assert_eq!(row(&rows, "Theme").section, 1);
        assert_eq!(row(&rows, "tmux").section, 2);
        assert!(rows.iter().any(|r| r.kind == NativeKind::Browse));
        let (options, selected) = choice(row(&rows, "Type"));
        assert_eq!(options.len(), 9);
        assert_eq!(options[selected], "SSH terminal");
        let (options, _) = choice(row(&rows, "Theme"));
        assert_eq!(options, ["by theme rules", "Nord", "Dracula"]);
        // An RDP desktop: no Look or Extras, a Via field.
        let ty = row(&rows, "Type").field.unwrap();
        f.set_choice(ty, 1);
        assert_eq!(f.values.kind, SessionKind::Rdp);
        assert_eq!(f.sections(), vec![(0, "Connection")]);
        let rows = f.native_rows();
        assert!(rows.iter().all(|r| r.section == 0));
        assert!(rows.iter().any(|r| r.label == "Via SSH"));
        assert!(!rows.iter().any(|r| r.label == "Key file"));
    }

    #[test]
    fn native_typing_grows_forwards_and_submit_checks() {
        let mut f = new_form();
        let rows = f.native_rows();
        let fwd = row(&rows, "Forward 1").field.unwrap();
        f.set_text(fwd, "L 8080 web:80");
        let rows = f.native_rows();
        assert!(rows.iter().any(|r| r.label == "Forward 2"));
        assert_eq!(f.submit(false), None);
        assert!(f.error().unwrap().contains("name"));
        let name = row(&rows, "Name").field.unwrap();
        f.set_text(name, "db1");
        assert_eq!(f.submit(true), Some(FormAction::Save { connect: true }));
        assert_eq!(f.error(), None);
        assert_eq!(f.button_names(), vec!["Save", "Connect", "Cancel"]);
        // The password source fills in a template's command.
        let pw = row(&rows, "Password").field.unwrap();
        f.set_choice(pw, 2);
        assert_eq!(f.values.password_command, TEMPLATES[0].command);
        let rows = f.native_rows();
        assert!(rows.iter().any(|r| r.label == "Command"));
    }

    #[test]
    fn password_sources_offer_templates() {
        let mut f = new_form();
        typed(&mut f, "web3");
        // Name, Folder, Type, Host, User, Port, Key, Browse, Password.
        down(&mut f, 8);
        assert_eq!(f.focused_field(), Some(Field::Password));
        assert!(texts(&mut f)[10].contains("‹ ask every time ›"));
        f.key(SideKey::Right, 30);
        let t = texts(&mut f);
        assert!(t[10].contains("Windows Credential Manager"));
        assert!(t[11].contains("Saved after a login that works."));
        f.key(SideKey::Right, 30);
        assert!(texts(&mut f)[10].contains("1Password"));
        assert_eq!(f.values.password_command, TEMPLATES[0].command);
        let mut settings = Settings::default();
        assert_eq!(f.check(), Ok(()));
        f.apply(&mut Vec::new(), &mut settings);
        assert_eq!(
            settings.profile("web3").unwrap().password,
            PasswordSource::Command(TEMPLATES[0].command.into())
        );
        // Back to the form: the template is recognised.
        let v = Values::default().with_profile(settings.profile("web3"));
        assert_eq!(v.password, 2);
        // A command of one's own starts empty and must be filled in.
        for _ in 0..TEMPLATES.len() {
            f.key(SideKey::Right, 30);
        }
        assert_eq!(f.values.password, PASSWORD_CUSTOM);
        assert!(f.values.password_command.is_empty());
        assert!(f.check().unwrap_err().contains("command"));
        f.key(SideKey::Down, 30);
        typed(&mut f, "pass show ssh/%n");
        f.apply(&mut Vec::new(), &mut settings);
        assert_eq!(
            settings.profile("web3").unwrap().password,
            PasswordSource::Command("pass show ssh/%n".into())
        );
        f.key(SideKey::Up, 30);
        f.key(SideKey::Right, 30);
        f.apply(&mut Vec::new(), &mut settings);
        assert_eq!(
            settings.profile("web3").unwrap().password,
            PasswordSource::Ask
        );
    }

    #[test]
    fn buttons_and_clicks() {
        let mut f = new_form();
        let t = texts(&mut f);
        let browse = t.iter().position(|l| l.contains("Browse")).unwrap();
        assert_eq!(f.click(14, browse, 30), Some(FormAction::BrowseKey));
        f.set_key(r"C:\Keys\id_ed25519");
        assert_eq!(f.values.key, r"C:\Keys\id_ed25519");
        let row = t.iter().position(|l| l.contains("Cancel")).unwrap();
        let col = t[row].find("Cancel").unwrap();
        assert_eq!(f.click(col, row, 30), Some(FormAction::Cancel));
        // A click on the Look section opens it.
        let look = t.iter().position(|l| l.contains("Look")).unwrap();
        f.click(2, look, 30);
        assert!(texts(&mut f).iter().any(|l| l.contains("by theme rules")));
        assert_eq!(f.key(SideKey::Escape, 30), Some(FormAction::Cancel));
    }
}
