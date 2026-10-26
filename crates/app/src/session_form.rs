//! The session form: the side pane's page for making and changing a saved
//! session, so nobody has to remember ssh_config keywords.
//!
//! The form is drawn into the side pane's [`Grid`] in place of the list,
//! in three folding sections (Connection, Look, Extras) with Save,
//! Connect, Cancel and Delete at the bottom. Empty fields show their
//! default in grey. Keys come in as [`SideKey`]s; what the user chose
//! comes out as a [`FormAction`].
//!
//! The connection settings of a host from `~/.ssh/config` or the PuTTY
//! import are shown read-only: FeroSSH never writes those files. Giving
//! the session a new name saves an editable copy.

use ferossh_config::forward::parse_option;
use ferossh_config::password::{PasswordSource, TEMPLATES};
use ferossh_config::saved::{self, SavedHost};
use ferossh_config::{Profile, SessionKind, Settings};
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
    pub via: String,
    /// Empty: the theme rules decide.
    pub theme: String,
    pub font: String,
    pub font_size: String,
    pub scrollback: String,
    pub tmux: String,
    /// Empty, `ferossh` or `tmux`.
    pub tmux_layout: String,
    /// One forward per entry, as `L 8080 localhost:80`.
    pub forwards: Vec<String>,
    pub desktop_port: String,
    pub vnc_port: String,
    pub legacy: bool,
    /// Where the password comes from: see [`PASSWORD_CHOICES`].
    pub password: usize,
    /// The password manager command, for a template or a custom one.
    pub password_command: String,
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
            forwards: h.forwards.iter().map(forward_text).collect(),
            ..Self::default()
        }
    }

    /// Connection settings as ssh_config resolves them, for a host the
    /// form shows read-only.
    pub fn from_resolved(
        name: &str,
        folder: Option<&str>,
        host: &str,
        user: &str,
        port: u16,
        key: Option<&str>,
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
        self.tmux = p.tmux.clone().unwrap_or_default();
        self.tmux_layout = p.tmux_layout.clone().unwrap_or_default();
        self.desktop_port = opt_text(&p.desktop_port);
        self.vnc_port = opt_text(&p.vnc_port);
        self.legacy = p.legacy_algorithms;
        (self.password, self.password_command) = password_choice(&p.password);
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
    Via,
    Theme,
    Font,
    FontSize,
    Scrollback,
    Tmux,
    TmuxLayout,
    Forward(usize),
    DesktopPort,
    VncPort,
    Legacy,
    Password,
    PasswordCommand,
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
            Field::Via => "Via SSH",
            Field::Theme => "Theme",
            Field::Font => "Font",
            Field::FontSize => "Font size",
            Field::Scrollback => "Scrollback",
            Field::Tmux => "tmux",
            Field::TmuxLayout => "tmux panes",
            Field::Forward(_) => "Forward",
            Field::DesktopPort => "RDP port",
            Field::VncPort => "VNC port",
            Field::Legacy => "Old crypto",
            Field::Password => "Password",
            Field::PasswordCommand => "Command",
        }
    }

    /// Picked from a list rather than typed.
    fn is_choice(self) -> bool {
        matches!(
            self,
            Field::Type | Field::Theme | Field::TmuxLayout | Field::Legacy | Field::Password
        )
    }
}

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
                    | Field::Forward(_)
            )
    }

    /// Keeps one empty forward line at the end to type a new one in.
    fn tidy_forwards(&mut self) {
        let fw = &mut self.values.forwards;
        if fw.last().is_none_or(|t| !t.trim().is_empty()) {
            fw.push(String::new());
        }
    }

    fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        let kind = self.values.kind;
        out.push(Item::Section(0));
        if self.open[0] {
            out.push(Item::Field(Field::Name));
            if let Source::ReadOnly(file) = &self.source {
                if self.locked() {
                    out.push(Item::Note(format!("From {file}: read-only.")));
                    out.push(Item::Note("A new name saves a copy.".into()));
                } else {
                    out.push(Item::Note(format!("Saves a copy; {file} stays.")));
                }
            }
            out.extend(
                [
                    Field::Folder,
                    Field::Type,
                    Field::Host,
                    Field::User,
                    Field::Port,
                ]
                .map(Item::Field),
            );
            match kind {
                SessionKind::Ssh => {
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
                            out.push(Item::Note("%n is the session's name.".into()));
                        }
                    }
                }
                SessionKind::Rdp | SessionKind::Vnc => out.push(Item::Field(Field::Via)),
            }
        }
        if kind == SessionKind::Ssh {
            out.push(Item::Section(1));
            if self.open[1] {
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
            out.push(Item::Section(2));
            if self.open[2] {
                out.push(Item::Field(Field::Tmux));
                out.push(Item::Field(Field::TmuxLayout));
                let n = self.values.forwards.len();
                let locked = self.field_locked(Field::Forward(0));
                for i in 0..n {
                    let empty = self.values.forwards.get(i).is_none_or(|t| t.is_empty());
                    if !(locked && empty) {
                        out.push(Item::Field(Field::Forward(i)));
                    }
                }
                out.extend([Field::DesktopPort, Field::VncPort, Field::Legacy].map(Item::Field));
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
            Field::Via => &v.via,
            Field::Font => &v.font,
            Field::FontSize => &v.font_size,
            Field::Scrollback => &v.scrollback,
            Field::Tmux => &v.tmux,
            Field::Forward(i) => return v.forwards.get(i),
            Field::DesktopPort => &v.desktop_port,
            Field::VncPort => &v.vnc_port,
            Field::PasswordCommand => &v.password_command,
            Field::Type | Field::Theme | Field::TmuxLayout | Field::Legacy | Field::Password => {
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
            Field::Via => &mut v.via,
            Field::Font => &mut v.font,
            Field::FontSize => &mut v.font_size,
            Field::Scrollback => &mut v.scrollback,
            Field::Tmux => &mut v.tmux,
            Field::Forward(i) => return v.forwards.get_mut(i),
            Field::DesktopPort => &mut v.desktop_port,
            Field::VncPort => &mut v.vnc_port,
            Field::PasswordCommand => &mut v.password_command,
            Field::Type | Field::Theme | Field::TmuxLayout | Field::Legacy | Field::Password => {
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
            }
            .to_owned(),
            Field::Theme if v.theme.is_empty() => "by theme rules".to_owned(),
            Field::Theme => v.theme.clone(),
            Field::TmuxLayout => match v.tmux_layout.as_str() {
                "tmux" => "tmux's own splits",
                _ => "one window per pane",
            }
            .to_owned(),
            Field::Legacy => if v.legacy { "allowed" } else { "off" }.to_owned(),
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
                let kinds = [SessionKind::Ssh, SessionKind::Rdp, SessionKind::Vnc];
                let i = kinds.iter().position(|k| *k == v.kind).unwrap_or(0);
                v.kind = kinds.get(step(i, 3)).copied().unwrap_or_default();
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
            Field::Host if v.kind != SessionKind::Ssh && !v.via.trim().is_empty() => {
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
                SessionKind::Ssh => "22".into(),
                SessionKind::Rdp => "3389".into(),
                SessionKind::Vnc => "5900".into(),
            },
            Field::Key => "default keys and agent".into(),
            Field::Via if v.kind == SessionKind::Vnc => "an SSH session (required)".into(),
            Field::Via => "none: connect directly".into(),
            Field::Font => h.font.clone(),
            Field::FontSize => format!("{}", h.font_size),
            Field::Scrollback => h.scrollback.to_string(),
            Field::Tmux => "off".into(),
            Field::Forward(_) => "e.g. L 8080 web:80".into(),
            Field::DesktopPort => "3389".into(),
            Field::VncPort => "5900".into(),
            Field::PasswordCommand => "program and arguments".into(),
            Field::Type | Field::Theme | Field::TmuxLayout | Field::Legacy | Field::Password => {
                String::new()
            }
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

    /// Checks what was typed; the first problem, in words.
    pub fn check(&self) -> Result<(), String> {
        let v = &self.values;
        let name = v.name.trim();
        if name.is_empty() {
            return Err("Give the session a name.".into());
        }
        if !saved::valid_alias(name) {
            return Err("Names use letters, digits, . _ and - only.".into());
        }
        let renamed = self
            .original
            .as_deref()
            .is_none_or(|o| !o.eq_ignore_ascii_case(name));
        if renamed && self.names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            return Err(format!("A session named {name} already exists."));
        }
        if self.locked() {
            // Only the profile is written.
        } else {
            let host = v.host.trim();
            if !host.is_empty() && !saved::valid_hostname(host) {
                return Err("The host is not a host name or IP address.".into());
            }
            if v.user
                .trim()
                .contains(|c: char| c.is_whitespace() || c.is_control() || c == '@')
            {
                return Err("The user name has a space or @ in it.".into());
            }
            port(&v.port, "The port")?;
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
        let via = v.via.trim();
        match v.kind {
            SessionKind::Ssh => {}
            SessionKind::Vnc if via.is_empty() => {
                return Err("A VNC desktop goes through an SSH session: fill in Via SSH.".into());
            }
            _ if via.is_empty() => {}
            _ if via.eq_ignore_ascii_case(name) => {
                return Err("Via SSH names another session, not this one.".into());
            }
            _ if !self.names.iter().any(|n| n.eq_ignore_ascii_case(via)) => {
                return Err(format!("There is no session named {via} to go through."));
            }
            _ => {}
        }
        if let Some(size) = number::<f32>(&v.font_size, "The font size")?
            && !(4.0..=96.0).contains(&size)
        {
            return Err("The font size goes from 4 to 96.".into());
        }
        number::<usize>(&v.scrollback, "Scrollback")?;
        if !v
            .tmux
            .trim()
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err("tmux session names use letters, digits, . _ and - only.".into());
        }
        port(&v.desktop_port, "The RDP port")?;
        port(&v.vnc_port, "The VNC port")?;
        if v.kind == SessionKind::Ssh
            && v.password > PASSWORD_WINDOWS
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
            (None, _) => " New session".to_owned(),
            (Some(n), _) => format!(" Session {n}"),
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
            Field::Forward(i) => format!("{} {}", f.label(), i + 1),
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
        if !self.locked() {
            let host = SavedHost {
                alias: name.clone(),
                folder: trimmed(&v.folder),
                hostname: trimmed(&v.host).filter(|h| *h != name),
                user: trimmed(&v.user),
                port: v.port.trim().parse().ok().filter(|&p| p != 0),
                identity_file: trimmed(&v.key),
                forwards: v.forwards.iter().filter_map(|f| parse_option(f)).fold(
                    Vec::new(),
                    |mut all, f| {
                        if !all.contains(&f) {
                            all.push(f);
                        }
                        all
                    },
                ),
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
        }
        let p = settings.profile_mut(&name);
        p.kind = v.kind;
        p.via = trimmed(&v.via).filter(|_| v.kind != SessionKind::Ssh);
        p.theme = trimmed(&v.theme);
        p.font = trimmed(&v.font);
        p.font_size = v.font_size.trim().parse().ok();
        p.scrollback = v.scrollback.trim().parse().ok();
        p.tmux = trimmed(&v.tmux);
        p.tmux_layout = trimmed(&v.tmux_layout);
        p.desktop_port = v.desktop_port.trim().parse().ok().filter(|&p| p != 0);
        p.vnc_port = v.vnc_port.trim().parse().ok().filter(|&p| p != 0);
        p.legacy_algorithms = v.legacy;
        p.password = match (v.kind, v.password) {
            (SessionKind::Ssh, PASSWORD_WINDOWS) => PasswordSource::Windows,
            (SessionKind::Ssh, i) if i > PASSWORD_WINDOWS => {
                PasswordSource::Command(v.password_command.trim().to_owned())
            }
            _ => PasswordSource::Ask,
        };
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
struct Line {
    cells: Vec<Cell>,
    width: usize,
    col: usize,
    bg: Rgb,
}

impl Line {
    fn new(width: usize, bg: Rgb) -> Self {
        Self {
            cells: Vec::with_capacity(width),
            width,
            col: 0,
            bg,
        }
    }

    fn put(&mut self, text: &str, fg: Rgb, flags: Flags) -> &mut Self {
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

    fn cells(&mut self, bg: Rgb) -> Vec<Cell> {
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
        assert_eq!(t[0], " New session");
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
        assert_eq!(t[11], " ▸ Look");
        assert_eq!(t[12], " ▸ Extras");
        assert!(t[13].contains("Save") && t[13].contains("Connect") && !t[13].contains("Delete"));

        assert_eq!(f.key(SideKey::Enter { split: false }, 30), None);
        assert!(
            texts(&mut f)
                .iter()
                .any(|l| l.contains("Give the session a name"))
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
        assert!(settings.profile("db").unwrap().theme.is_none());
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
            (Field::Tmux, "a;b", "tmux"),
        ] {
            f.values = Values {
                name: "ok".into(),
                forwards: vec![String::new()],
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
        assert!(f.check().unwrap_err().contains("no session named nowhere"));
        f.values.via = "bastion".into();
        assert_eq!(f.check(), Ok(()));
        f.values.kind = SessionKind::Rdp;
        f.values.via.clear();
        assert_eq!(f.check(), Ok(()), "RDP may go straight to the machine");
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
    fn forwards_grow_a_new_line() {
        let mut f = new_form();
        typed(&mut f, "web2");
        // Open Extras: Name, Folder, Type, Host, User, Port, Key, Browse,
        // Password, Look, Extras.
        down(&mut f, 10);
        f.key(SideKey::Enter { split: false }, 30);
        down(&mut f, 3);
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
    fn read_only_hosts_save_only_the_profile_or_a_copy() {
        let values = Values::from_resolved(
            "web",
            Some("Work"),
            "web.example.org",
            "admin",
            2222,
            None,
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
        // The theme can: past user, port, key and password to Look.
        down(&mut f, 5);
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
        delete("new", &mut hosts, &mut settings);
        assert!(hosts.is_empty() && settings.favourites.is_empty());
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
