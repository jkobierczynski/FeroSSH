//! The UI side of one WinRM session: a Windows Remote Shell running
//! PowerShell or cmd on a Windows server.
//!
//! The connection runs in a sandboxed worker of its own
//! ([`ferossh_worker::winrm`]), which speaks HTTP and SOAP. The login and
//! the encryption of every shell request and reply happen here, through the signing
//! broker's SSPI thread ([`ferossh_auth::negotiate`]), so the password
//! and the session keys never reach the worker: it asks for each step
//! with [`Message::SspiRequest`].
//!
//! WinRS has no terminal. The remote command reads lines, so this side
//! edits the line being typed (echo, Backspace, Up and Down for earlier
//! lines) and sends it whole on Enter; Ctrl+C interrupts the command.
//!
//! A WinRM worker never has a reason to ask for host keys, keys,
//! signatures or SSH passwords. One that does is treated as compromised
//! and the connection is closed.

use std::path::{Path, PathBuf};

use ferossh_auth::Job;
use ferossh_auth::negotiate::{Credentials, ServerCert, SspiJob};
use ferossh_config::saved::SavedHost;
use ferossh_config::ssh_config;
use ferossh_config::{DEFAULT_WINRM_COMMAND, DEFAULT_WINRM_HTTPS_PORT, DEFAULT_WINRM_PORT};
use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message, Secrets, SspiOp, SspiResult, Timing};
use zeroize::Zeroizing;

use crate::ssh::{Effect, Env, State, sanitize};

const DIM: &str = "\x1b[0;90m";
const RED: &str = "\x1b[0;31m";
const YELLOW: &str = "\x1b[0;33m";
const RESET: &str = "\x1b[0m";
/// How the worker starts its message when the server refuses the login
/// (`crates/worker/src/winrm`).
const ACCESS_DENIED: &str = "Access denied: the server refused the login";

/// Earlier lines kept for Up and Down.
const HISTORY: usize = 200;
/// Logins the worker may start: one per HTTP connection, and a spare.
const MAX_STARTS: u32 = 3;
/// The longest line kept while typing.
const MAX_LINE: usize = 64 * 1024;

/// Where a WinRM session goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinrmTarget {
    /// The saved connection's name, or the host as typed.
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    /// `DOMAIN\user`, `user@domain` or a local user; none logs in as the
    /// Windows user FeroSSH runs as.
    pub user: Option<String>,
    /// What the shell runs.
    pub command: String,
    /// ssh_config's `Tag`, which picks a colour theme.
    pub tag: Option<String>,
    pub connect_timeout: u32,
    /// HTTPS (TLS through SChannel) instead of HTTP with encrypted
    /// messages.
    pub https: bool,
}

fn valid_user(user: &str) -> bool {
    !user.is_empty() && user.len() <= 256 && !user.chars().any(char::is_control)
}

impl WinrmTarget {
    /// What was typed at the connect prompt, if it asks for WinRM:
    /// `winrm://[user@]host[:port]`, `winrms://` for HTTPS, or
    /// `winrm [user@]host [port]`. Port 5986 is HTTPS too.
    pub fn typed(text: &str) -> Option<Self> {
        let text = text.trim();
        let mut https = false;
        let rest = match strip_prefix_ci(text, "winrm://") {
            Some(r) => r.trim_end_matches('/').to_owned(),
            None if strip_prefix_ci(text, "winrms://").is_some() => {
                https = true;
                strip_prefix_ci(text, "winrms://")?
                    .trim_end_matches('/')
                    .to_owned()
            }
            None => {
                let mut words = strip_prefix_ci(text, "winrm ")?.split_whitespace();
                let host = words.next()?;
                match (words.next(), words.next()) {
                    (None, _) => host.to_owned(),
                    (Some(port), None) if !host.contains(':') => format!("{host}:{port}"),
                    _ => return None,
                }
            }
        };
        let (user, hostname, port) = ssh_config::parse_destination(&rest)?;
        if user.as_deref().is_some_and(|u| !valid_user(u)) {
            return None;
        }
        let https = https || port == Some(DEFAULT_WINRM_HTTPS_PORT);
        Some(Self {
            alias: hostname.clone(),
            hostname,
            port: port.unwrap_or(default_port(https)),
            user,
            command: DEFAULT_WINRM_COMMAND.to_owned(),
            tag: None,
            connect_timeout: Timing::default().connect_timeout,
            https,
        })
    }

    /// A saved WinRM connection `name`: its `HostName`, `Port` (5985,
    /// or 5986 with `https`, when unset), `User`, `Tag` and
    /// `ConnectTimeout`, and the command and transport its profile names.
    ///
    /// The host, user and port come from the saved connection itself when
    /// there is one (`saved`), so an SSH `Host *` block's `User` or `Port`
    /// cannot override them, and a blank user still means the Windows
    /// logon.
    pub fn resolve(
        name: &str,
        env: &Env,
        command: Option<&str>,
        https: bool,
        saved: Option<&SavedHost>,
    ) -> Option<Self> {
        let includes = ssh_config::DirIncludes {
            home: env.home.clone(),
        };
        let cfg = ssh_config::resolve_in(&env.config, name, &includes);
        let (hostname, port, user) = match saved {
            Some(h) => (h.hostname.clone(), h.port, h.user.clone()),
            None => (cfg.hostname, cfg.port, cfg.user),
        };
        let hostname = hostname.unwrap_or_else(|| name.to_owned());
        if !ferossh_config::saved::valid_hostname(&hostname) {
            return None;
        }
        Some(Self {
            alias: name.to_owned(),
            hostname,
            port: port.unwrap_or(default_port(https)),
            user: user.filter(|u| valid_user(u)),
            command: command
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .unwrap_or(DEFAULT_WINRM_COMMAND)
                .to_owned(),
            tag: cfg.tag,
            connect_timeout: cfg
                .connect_timeout
                .unwrap_or(Timing::default().connect_timeout),
            https,
        })
    }

    /// `host`, or `host:port` off the WinRM port, for titles and logs.
    pub fn label(&self) -> String {
        if self.port == default_port(self.https) {
            self.alias.clone()
        } else {
            format!("{}:{}", self.alias, self.port)
        }
    }
}

/// WinRM's port for HTTP or HTTPS.
pub fn default_port(https: bool) -> u16 {
    if https {
        DEFAULT_WINRM_HTTPS_PORT
    } else {
        DEFAULT_WINRM_PORT
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| text.get(prefix.len()..))
        .flatten()
}

fn dim(text: &str) -> String {
    format!("{DIM}{text}{RESET}\r\n")
}

/// Where typed bytes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keys {
    /// Into the line.
    Text,
    /// After ESC: a CSI or SS3 sequence follows.
    Esc,
    /// Inside `ESC [` or `ESC O`, up to the final byte.
    Seq,
}

/// One WinRM session, from the UI's side.
pub struct WinrmSession {
    pub channel: ChannelId,
    pub target: WinrmTarget,
    state: State,
    /// Typing the password, before connecting.
    asking: bool,
    typed_password: Zeroizing<String>,
    /// The password for the logins; dropped once logged in.
    password: Option<Zeroizing<String>>,
    starts: u32,
    /// `Kerberos` or `NTLM`, once a login has been made.
    package: Option<String>,
    line: String,
    keys: Keys,
    seq: Vec<u8>,
    /// The last byte typed was CR: a LF after it is the same Enter.
    after_cr: bool,
    history: Vec<String>,
    /// Where Up and Down are in `history`; `history.len()` is the line
    /// being typed.
    recall: usize,
    /// The line being typed when Up was first pressed.
    draft: String,
    /// Closed from this side while the shell is open: what still seals
    /// the worker's goodbye (terminate, delete) until the window takes it.
    closer: Option<WinrmCloser>,
    /// The connection has a password source (Credential Manager or a
    /// command): asked before the prompt.
    pub password_source: bool,
    /// The password typed goes to the Windows Credential Manager once
    /// the login works.
    pub remember_password: bool,
    /// Waiting for the password source.
    fetching: bool,
    /// The password came from the source.
    from_source: bool,
    /// The typed password, kept to store once logged in.
    to_store: Option<Zeroizing<String>>,
    /// The shell opened at least once.
    was_connected: bool,
    /// The server refused the login (HTTP 401 after the last token), as
    /// opposed to a network failure or a disconnect from this side.
    login_refused: bool,
    /// TLS connections the worker started.
    tls_starts: u32,
    /// Where certificates the user trusted are kept (`host:port` and the
    /// SHA-256 fingerprint, a line each); none keeps them for the
    /// session only.
    pub cert_store: Option<PathBuf>,
    /// Fingerprints trusted for this session.
    trusted_certs: Vec<String>,
    /// Asking whether to trust the server's certificate.
    cert_prompt: Option<CertPrompt>,
}

/// The question about a certificate Windows does not trust, and the
/// TLS answer held for the worker until the user says yes.
struct CertPrompt {
    conn: u8,
    held: SspiResult,
    sha256: String,
    answer: String,
}

/// A closed session's last errand: the worker tells the server to end the
/// command and delete the shell, and those requests (and the replies)
/// still need the login's keys. It relays only sealing and unsealing,
/// and forgets the keys once the worker says it is done.
#[derive(Debug)]
pub struct WinrmCloser {
    pub channel: ChannelId,
    host: String,
}

impl WinrmCloser {
    /// The worker's connection: the session's own.
    pub fn connection(&self) -> ChannelId {
        self.channel
    }

    /// What the worker sent; true once the closer is done.
    pub fn on_worker(&mut self, msg: Message) -> (Vec<Effect>, bool) {
        match msg {
            Message::SspiRequest {
                conn,
                op:
                    op @ (SspiOp::Seal(_)
                    | SspiOp::Unseal { .. }
                    | SspiOp::TlsSend(_)
                    | SspiOp::TlsReceived(_)),
                ..
            } if conn <= 1 => (
                vec![Effect::Broker(Job::Sspi(SspiJob {
                    login: self.channel,
                    conn,
                    host: self.host.clone(),
                    credentials: None,
                    op,
                }))],
                false,
            ),
            Message::Disconnected { .. } => (self.end(), true),
            // Late output and the like: the pane is gone.
            Message::SspiRequest { .. } => (self.end(), true),
            _ => (Vec::new(), false),
        }
    }

    /// The broker's answer to a seal or unseal.
    pub fn on_broker(&self, reply: ferossh_auth::Reply) -> Vec<Effect> {
        match reply {
            ferossh_auth::Reply::Sspi {
                login,
                conn,
                output,
            } if login == self.channel => vec![Effect::Send(Message::SspiReply {
                channel: self.channel,
                conn,
                result: output.result,
            })],
            _ => Vec::new(),
        }
    }

    /// Forgets the login's keys.
    pub fn end(&self) -> Vec<Effect> {
        vec![Effect::Broker(Job::SspiEnd(self.channel))]
    }
}

impl std::fmt::Debug for WinrmSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinrmSession")
            .field("channel", &self.channel)
            .field("target", &self.target)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl WinrmSession {
    pub fn new(channel: ChannelId, target: WinrmTarget) -> Self {
        Self {
            channel,
            target,
            state: State::Connecting,
            asking: false,
            typed_password: Zeroizing::new(String::new()),
            password: None,
            starts: 0,
            package: None,
            line: String::new(),
            keys: Keys::Text,
            seq: Vec::new(),
            after_cr: false,
            history: Vec::new(),
            recall: 0,
            draft: String::new(),
            closer: None,
            password_source: false,
            remember_password: false,
            fetching: false,
            from_source: false,
            to_store: None,
            was_connected: false,
            login_refused: false,
            tls_starts: 0,
            cert_store: None,
            trusted_certs: Vec::new(),
            cert_prompt: None,
        }
    }

    /// The closer [`close`](Self::close) left, for the window to keep
    /// until the worker is done.
    pub fn take_closer(&mut self) -> Option<WinrmCloser> {
        self.closer.take()
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The worker's connection: a WinRM session has its own.
    pub fn connection(&self) -> ChannelId {
        self.channel
    }

    /// Starts: asks for the password first when the session names a
    /// user.
    pub fn start(&mut self) -> Vec<Effect> {
        match &self.target.user {
            Some(_) if self.password_source => {
                self.fetching = true;
                vec![Effect::FetchPassword]
            }
            Some(_) => self.ask(),
            None => self.connect(),
        }
    }

    /// Asks for the password at the prompt.
    fn ask(&mut self) -> Vec<Effect> {
        let user = self.target.user.as_deref().unwrap_or_default();
        self.asking = true;
        vec![Effect::Show(format!(
            "{}'s password on {}: ",
            sanitize(user),
            sanitize(&self.target.label())
        ))]
    }

    /// Whether the session waits for its password source.
    pub fn fetching_password(&self) -> bool {
        self.fetching && self.state == State::Connecting
    }

    /// The password source answered: `Ok` logs in with the password,
    /// `Err` says why not and asks for it at the prompt instead. `from`
    /// names the source for the note.
    pub fn password_fetched(
        &mut self,
        result: Result<Zeroizing<String>, String>,
        from: &str,
    ) -> Vec<Effect> {
        if !self.fetching_password() {
            return Vec::new();
        }
        self.fetching = false;
        match result {
            Ok(password) => {
                self.from_source = true;
                self.password = Some(password);
                let mut effects = vec![Effect::Show(dim(&format!(
                    "Password from {}.",
                    sanitize(from)
                )))];
                effects.extend(self.connect());
                effects
            }
            Err(why) => {
                let mut effects = Vec::new();
                if !why.is_empty() {
                    effects.push(Effect::Show(dim(&sanitize(&why))));
                }
                effects.extend(self.ask());
                effects
            }
        }
    }

    /// The password from the source did not get in: the next try asks.
    pub fn source_refused(&self) -> bool {
        self.from_source && self.login_refused && !self.was_connected
    }

    fn connect(&mut self) -> Vec<Effect> {
        let t = &self.target;
        let mut text = format!("Connecting to {}", sanitize(&t.label()));
        if t.hostname != t.alias {
            text.push_str(&format!(" ({}:{})", sanitize(&t.hostname), t.port));
        }
        text.push_str(" over WinRM...");
        vec![
            Effect::Show(dim(&text)),
            Effect::Send(Message::WinrmConnect {
                channel: self.channel,
                host: t.hostname.clone(),
                port: t.port,
                connect_timeout: t.connect_timeout,
                command: t.command.clone(),
                https: t.https,
            }),
        ]
    }

    /// Typed or pasted bytes.
    pub fn input(&mut self, bytes: &[u8]) -> Vec<Effect> {
        if self.asking {
            return self.password_input(bytes);
        }
        if self.cert_prompt.is_some() {
            return self.cert_input(bytes);
        }
        if self.state != State::Connected {
            return Vec::new();
        }
        let mut effects = Vec::new();
        let mut echo = String::new();
        let text = String::from_utf8_lossy(bytes).into_owned();
        for c in text.chars() {
            let after_cr = std::mem::replace(&mut self.after_cr, false);
            match self.keys {
                Keys::Esc => {
                    self.keys = if c == '[' || c == 'O' {
                        Keys::Seq
                    } else {
                        Keys::Text
                    };
                    self.seq.clear();
                    continue;
                }
                Keys::Seq => {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        self.keys = Keys::Text;
                        match c {
                            'A' => self.recall_line(-1, &mut echo),
                            'B' => self.recall_line(1, &mut echo),
                            _ => {}
                        }
                    } else if self.seq.len() < 16 {
                        self.seq.push(c as u8);
                    }
                    continue;
                }
                Keys::Text => {}
            }
            match c {
                '\x1b' => self.keys = Keys::Esc,
                '\r' | '\n' => {
                    if c == '\n' && after_cr {
                        continue;
                    }
                    self.after_cr = c == '\r';
                    echo.push_str("\r\n");
                    let line = std::mem::take(&mut self.line);
                    if !line.trim().is_empty() && self.history.last() != Some(&line) {
                        if self.history.len() >= HISTORY {
                            self.history.remove(0);
                        }
                        self.history.push(line.clone());
                    }
                    self.recall = self.history.len();
                    self.draft.clear();
                    self.flush_echo(&mut echo, &mut effects);
                    let mut bytes = line.into_bytes();
                    bytes.extend_from_slice(b"\r\n");
                    for chunk in bytes.chunks(MAX_DATA_CHUNK) {
                        effects.push(Effect::Send(Message::Data {
                            channel: self.channel,
                            bytes: chunk.to_vec(),
                        }));
                    }
                }
                '\x7f' | '\x08' => {
                    if self.line.pop().is_some() {
                        echo.push_str("\x08 \x08");
                    }
                }
                '\x03' => {
                    // Interrupt the command; what was typed is dropped.
                    self.line.clear();
                    echo.push_str("^C\r\n");
                    self.flush_echo(&mut echo, &mut effects);
                    effects.push(Effect::Send(Message::Data {
                        channel: self.channel,
                        bytes: vec![0x03],
                    }));
                }
                '\x15' => self.erase_line(&mut echo),
                '\t' => {
                    if self.line.len() < MAX_LINE {
                        self.line.push(' ');
                        echo.push(' ');
                    }
                }
                c if c.is_control() => {}
                c => {
                    if self.line.len() < MAX_LINE {
                        self.line.push(c);
                        echo.push(c);
                    }
                }
            }
        }
        self.flush_echo(&mut echo, &mut effects);
        effects
    }

    /// Echo goes into the terminal and the log, as a terminal's own
    /// echo would.
    fn flush_echo(&self, echo: &mut String, effects: &mut Vec<Effect>) {
        if !echo.is_empty() {
            effects.push(Effect::Echo(std::mem::take(echo).into_bytes()));
        }
    }

    fn erase_line(&mut self, echo: &mut String) {
        for _ in self.line.chars() {
            echo.push_str("\x08 \x08");
        }
        self.line.clear();
    }

    /// Up (`-1`) or Down (`1`) through earlier lines.
    fn recall_line(&mut self, step: isize, echo: &mut String) {
        let len = self.history.len();
        let next = match step {
            -1 if self.recall > 0 => self.recall - 1,
            1 if self.recall < len => self.recall + 1,
            _ => return,
        };
        if self.recall == len {
            self.draft = self.line.clone();
        }
        self.erase_line(echo);
        self.recall = next;
        self.line = self
            .history
            .get(next)
            .cloned()
            .unwrap_or_else(|| self.draft.clone());
        echo.push_str(&sanitize(&self.line));
    }

    fn password_input(&mut self, bytes: &[u8]) -> Vec<Effect> {
        let text = String::from_utf8_lossy(bytes).into_owned();
        for c in text.chars() {
            match c {
                '\r' | '\n' => {
                    self.asking = false;
                    let password = std::mem::take(&mut self.typed_password);
                    if self.remember_password && !password.is_empty() {
                        self.to_store = Some(password.clone());
                    }
                    self.password = Some(password);
                    let mut effects = vec![Effect::Show("\r\n".into())];
                    effects.extend(self.connect());
                    return effects;
                }
                '\x7f' | '\x08' => {
                    self.typed_password.pop();
                }
                '\x03' => {
                    self.asking = false;
                    self.typed_password = Zeroizing::new(String::new());
                    self.state = State::Ended;
                    return vec![
                        Effect::Show(format!(
                            "\r\n{DIM}[Cancelled. Press Enter to connect again.]{RESET}\r\n"
                        )),
                        Effect::Ended,
                    ];
                }
                c if c.is_control() => {}
                c => self.typed_password.push(c),
            }
        }
        Vec::new()
    }

    /// Ends the connection from this side.
    pub fn close(&mut self) -> Vec<Effect> {
        if self.state == State::Ended {
            return Vec::new();
        }
        let was = self.state;
        self.state = State::Ended;
        self.asking = false;
        self.password = None;
        let mut effects = vec![Effect::Send(Message::Close {
            channel: self.channel,
        })];
        if was == State::Connected {
            // The worker still ends the command and deletes the shell on
            // the server, sealed with this login's keys.
            self.closer = Some(WinrmCloser {
                channel: self.channel,
                host: self.target.hostname.clone(),
            });
        } else {
            effects.push(Effect::Broker(Job::SspiEnd(self.channel)));
        }
        effects
    }

    fn cut_off(&mut self) -> Vec<Effect> {
        let mut effects = vec![Effect::Show(format!(
            "\r\n{RED}The WinRM worker sent something a WinRM session never does, so \
             FeroSSH closed the connection.{RESET}\r\n"
        ))];
        effects.extend(self.close());
        // A worker that misbehaves gets no more sealing.
        if let Some(c) = self.closer.take() {
            effects.extend(c.end());
        }
        effects.push(Effect::Ended);
        effects
    }

    pub fn on_worker(&mut self, msg: Message) -> Vec<Effect> {
        match msg {
            Message::SspiRequest { conn, op, .. }
                if self.state == State::Connecting && !self.asking && conn <= 1 =>
            {
                if matches!(op, SspiOp::Start) {
                    self.starts += 1;
                    if self.starts > MAX_STARTS {
                        return self.cut_off();
                    }
                }
                if matches!(op, SspiOp::TlsStart) {
                    self.tls_starts += 1;
                    if !self.target.https || self.tls_starts > MAX_STARTS {
                        return self.cut_off();
                    }
                }
                if self.cert_prompt.is_some() {
                    // Nothing goes on while the user decides.
                    return self.cut_off();
                }
                let credentials = match (&self.target.user, &self.password) {
                    (Some(user), Some(password)) => Some(Credentials {
                        user: user.clone(),
                        password: password.clone(),
                    }),
                    _ => None,
                };
                vec![Effect::Broker(Job::Sspi(SspiJob {
                    login: self.channel,
                    conn,
                    host: self.target.hostname.clone(),
                    credentials: credentials.filter(|_| matches!(op, SspiOp::Start)),
                    op,
                }))]
            }
            // Once logged in, only sealing and unsealing.
            Message::SspiRequest {
                conn,
                op:
                    op @ (SspiOp::Seal(_)
                    | SspiOp::Unseal { .. }
                    | SspiOp::TlsSend(_)
                    | SspiOp::TlsReceived(_)),
                ..
            } if self.state == State::Connected && conn <= 1 => {
                vec![Effect::Broker(Job::Sspi(SspiJob {
                    login: self.channel,
                    conn,
                    host: self.target.hostname.clone(),
                    credentials: None,
                    op,
                }))]
            }
            Message::Connected { .. } if self.state == State::Connecting && !self.asking => {
                self.state = State::Connected;
                self.was_connected = true;
                self.password = None;
                let how = match self.package.as_deref() {
                    Some(p) if p.eq_ignore_ascii_case("Kerberos") => " with Kerberos",
                    Some(p) if p.eq_ignore_ascii_case("NTLM") => " with NTLM",
                    _ => "",
                };
                let over = if self.target.https { " over HTTPS" } else { "" };
                let mut effects = vec![Effect::Show(dim(&format!(
                    "Connected{how}{over}; shell traffic is encrypted. This is a WinRS shell \
                     without a terminal: lines are sent on Enter, Ctrl+C interrupts, \
                     full-screen programs do not work."
                )))];
                // Over HTTPS the certificate proves the server's identity.
                if how == " with NTLM" && !self.target.https {
                    effects.push(Effect::Show(format!(
                        "{YELLOW}NTLM does not prove the server's identity. Use the \
                         server's domain name to log in with Kerberos.{RESET}\r\n"
                    )));
                }
                if let (Some(user), Some(password)) = (&self.target.user, self.to_store.take()) {
                    effects.push(Effect::StorePassword {
                        user: user.clone(),
                        host: self.target.hostname.clone(),
                        port: self.target.port,
                        password: Secrets(vec![password.to_string()]),
                    });
                }
                effects.push(Effect::Connected);
                effects
            }
            Message::Data { bytes, .. } if self.state == State::Connected => {
                vec![Effect::Output(bytes)]
            }
            Message::ExecStderr { bytes, .. } if self.state == State::Connected => {
                let mut out = RED.as_bytes().to_vec();
                out.extend_from_slice(&bytes);
                out.extend_from_slice(RESET.as_bytes());
                vec![Effect::Output(out)]
            }
            Message::ExitStatus { .. } if self.state == State::Connected => Vec::new(),
            Message::Notice { text, .. } => vec![Effect::Show(dim(&sanitize(&text)))],
            Message::Disconnected { reason, .. } => {
                if self.state == State::Ended {
                    return vec![Effect::Ended];
                }
                self.state = State::Ended;
                self.password = None;
                // The worker's words for a 401 to the last login token.
                self.login_refused = !self.was_connected && reason.starts_with(ACCESS_DENIED);
                vec![
                    Effect::Broker(Job::SspiEnd(self.channel)),
                    Effect::Show(format!(
                        "\r\n{DIM}[{} Press Enter to reconnect.]{RESET}\r\n",
                        sanitize(&reason)
                    )),
                    Effect::Ended,
                ]
            }
            // Once closed from this side, late messages are dropped.
            _ if self.state == State::Ended => Vec::new(),
            _ => self.cut_off(),
        }
    }

    /// The broker's answer to an SSPI step.
    pub fn on_broker(&mut self, reply: ferossh_auth::Reply) -> Vec<Effect> {
        let ferossh_auth::Reply::Sspi {
            login,
            conn,
            output,
        } = reply
        else {
            return Vec::new();
        };
        if login != self.channel || self.state == State::Ended {
            return Vec::new();
        }
        if let Some(p) = output.package {
            self.package = Some(p);
        }
        if let Some(cert) = output.cert {
            return self.check_cert(conn, cert, output.result);
        }
        vec![self.reply(conn, output.result)]
    }

    fn reply(&self, conn: u8, result: SspiResult) -> Effect {
        Effect::Send(Message::SspiReply {
            channel: self.channel,
            conn,
            result,
        })
    }

    /// `host:port`, as the certificate store keys a server.
    fn cert_key(&self) -> String {
        if self.target.hostname.contains(':') {
            format!("[{}]:{}", self.target.hostname, self.target.port)
        } else {
            format!("{}:{}", self.target.hostname, self.target.port)
        }
    }

    /// The handshake is done: the worker gets `held` once the server's
    /// certificate is trusted, by Windows, by an earlier "yes", or by the
    /// user now.
    fn check_cert(&mut self, conn: u8, cert: ServerCert, held: SspiResult) -> Vec<Effect> {
        if matches!(held, SspiResult::Failed(_)) {
            return vec![self.reply(conn, held)];
        }
        let key = self.cert_key();
        let first = conn == 0 && self.state == State::Connecting;
        match &cert.windows {
            Ok(()) => {
                let mut effects = Vec::new();
                if first {
                    effects.push(Effect::Show(dim(
                        "HTTPS: the server's certificate is valid for this name (checked by Windows).",
                    )));
                }
                effects.push(self.reply(conn, held));
                return effects;
            }
            Err(_) if self.trusted_certs.contains(&cert.sha256) => {
                return vec![self.reply(conn, held)];
            }
            Err(_) => {}
        }
        let known = self.cert_store.as_deref().and_then(|p| known_cert(p, &key));
        match known {
            Some(sha) if sha == cert.sha256 => {
                self.trusted_certs.push(cert.sha256);
                let mut effects = Vec::new();
                if first {
                    effects.push(Effect::Show(dim(
                        "HTTPS: the server's certificate is the one you trusted before.",
                    )));
                }
                effects.push(self.reply(conn, held));
                effects
            }
            Some(_) => {
                let text = format!(
                    "{RED}The server's certificate is not the one you trusted for {} before, \
                     and Windows does not trust it either. Someone may be in the way. \
                     Its SHA-256 fingerprint is now {}. If the server got a new certificate, \
                     remove the old line from {}.{RESET}\r\n",
                    sanitize(&key),
                    cert.sha256,
                    self.cert_store
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
                vec![
                    Effect::Show(text),
                    self.reply(
                        conn,
                        SspiResult::Failed("The server's certificate changed.".into()),
                    ),
                ]
            }
            None => {
                let why = cert.windows.err().unwrap_or_default();
                let text = format!(
                    "{YELLOW}Windows does not trust the server's certificate: {}{RESET}\r\n\
                     SHA-256 fingerprint: {}\r\n\
                     Check it on the server (Get-ChildItem Cert:\\LocalMachine\\My | \
                     Format-List Subject, Thumbprint shows SHA-1; \
                     (Get-ChildItem Cert:\\LocalMachine\\My\\<thumbprint>).GetCertHashString('SHA256') \
                     the SHA-256).\r\n\
                     Trust this certificate for {}? (yes/no) ",
                    sanitize(why.trim_end_matches('.')),
                    cert.sha256,
                    sanitize(&key)
                );
                self.cert_prompt = Some(CertPrompt {
                    conn,
                    held,
                    sha256: cert.sha256,
                    answer: String::new(),
                });
                vec![Effect::Show(text)]
            }
        }
    }

    /// Typing the answer to the certificate question.
    fn cert_input(&mut self, bytes: &[u8]) -> Vec<Effect> {
        let text = String::from_utf8_lossy(bytes).into_owned();
        let mut effects = Vec::new();
        for c in text.chars() {
            let Some(prompt) = self.cert_prompt.as_mut() else {
                break;
            };
            match c {
                '\r' | '\n' => {
                    let Some(prompt) = self.cert_prompt.take() else {
                        break;
                    };
                    effects.push(Effect::Show("\r\n".into()));
                    if prompt.answer.trim().eq_ignore_ascii_case("yes") {
                        let key = self.cert_key();
                        if let Some(path) = self.cert_store.clone() {
                            let note = match remember_cert(&path, &key, &prompt.sha256) {
                                Ok(()) => format!("Trusted; kept in {}.", path.display()),
                                Err(e) => format!("Trusted for this session ({e})."),
                            };
                            effects.push(Effect::Show(dim(&sanitize(&note))));
                        }
                        self.trusted_certs.push(prompt.sha256);
                        effects.push(self.reply(prompt.conn, prompt.held));
                    } else {
                        effects.push(self.reply(
                            prompt.conn,
                            SspiResult::Failed("The certificate was not trusted.".into()),
                        ));
                    }
                    break;
                }
                '\x7f' | '\x08' => {
                    if prompt.answer.pop().is_some() {
                        effects.push(Effect::Show("\x08 \x08".into()));
                    }
                }
                '\x03' => {
                    let Some(prompt) = self.cert_prompt.take() else {
                        break;
                    };
                    effects.push(Effect::Show("\r\n".into()));
                    effects.push(self.reply(
                        prompt.conn,
                        SspiResult::Failed("The certificate was not trusted.".into()),
                    ));
                    break;
                }
                c if c.is_control() => {}
                c if prompt.answer.len() < 16 => {
                    prompt.answer.push(c);
                    effects.push(Effect::Show(c.to_string()));
                }
                _ => {}
            }
        }
        effects
    }
}

/// The fingerprint kept for `key` (`host:port`) in `path`.
fn known_cert(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|l| {
        let (k, sha) = l.trim().split_once(' ')?;
        k.eq_ignore_ascii_case(key).then(|| sha.trim().to_owned())
    })
}

/// Keeps `sha256` as the certificate of `key` in `path`, replacing an
/// older one.
fn remember_cert(path: &Path, key: &str, sha256: &str) -> Result<(), String> {
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let mut text: String = old
        .lines()
        .filter(|l| {
            l.trim()
                .split_once(' ')
                .is_none_or(|(k, _)| !k.eq_ignore_ascii_case(key))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    text.push_str(&format!("{key} {sha256}\n"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use ferossh_auth::negotiate::SspiOutput;
    use ferossh_ipc::SspiResult;

    fn shown(effects: &[Effect]) -> String {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Show(t) => Some(t.clone()),
                Effect::Output(b) | Effect::Echo(b) => {
                    Some(String::from_utf8_lossy(b).into_owned())
                }
                _ => None,
            })
            .collect()
    }

    fn sent(effects: &[Effect]) -> Vec<Vec<u8>> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send(Message::Data { bytes, .. }) => Some(bytes.clone()),
                _ => None,
            })
            .collect()
    }

    fn connected(user: bool) -> WinrmSession {
        let text = if user {
            "winrm://LAB\\admin@srv"
        } else {
            "winrm srv"
        };
        let mut s = WinrmSession::new(5, WinrmTarget::typed(text).unwrap());
        s.start();
        if user {
            s.input(b"pw\r");
        }
        s.on_worker(Message::Connected {
            channel: 5,
            kex: "WinRM".into(),
            cipher: "SSPI".into(),
            host_key: "none".into(),
        });
        s
    }

    fn connected_msg() -> Message {
        Message::Connected {
            channel: 5,
            kex: "WinRM".into(),
            cipher: "SSPI".into(),
            host_key: "none".into(),
        }
    }

    /// The password the first login hands the broker.
    fn login_password(s: &mut WinrmSession) -> Option<String> {
        let fx = s.on_worker(Message::SspiRequest {
            channel: 5,
            conn: 0,
            op: SspiOp::Start,
        });
        fx.into_iter().find_map(|e| match e {
            Effect::Broker(Job::Sspi(j)) => j.credentials.map(|c| c.password.to_string()),
            _ => None,
        })
    }

    fn stored(effects: &[Effect]) -> Option<String> {
        effects.iter().find_map(|e| match e {
            Effect::StorePassword {
                user,
                host,
                port,
                password,
            } => Some(format!("{user}@{host}:{port}={}", password.0.first()?)),
            _ => None,
        })
    }

    #[test]
    fn a_password_source_answers_before_the_prompt() {
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://admin@srv").unwrap());
        s.password_source = true;
        let fx = s.start();
        assert!(matches!(fx[..], [Effect::FetchPassword]), "{fx:?}");
        assert!(s.fetching_password());
        // Typing meanwhile goes nowhere.
        assert!(s.input(b"x").is_empty());
        let fx = s.password_fetched(Ok(Zeroizing::new("secret".into())), "pass");
        assert!(shown(&fx).contains("Password from pass."));
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Send(Message::WinrmConnect { .. })))
        );
        assert!(!s.fetching_password());
        // A late second answer changes nothing.
        assert!(
            s.password_fetched(Ok(Zeroizing::new("x".into())), "pass")
                .is_empty()
        );
        assert_eq!(login_password(&mut s).as_deref(), Some("secret"));
        // From the source: nothing to store.
        assert_eq!(stored(&s.on_worker(connected_msg())), None);
        assert!(!s.source_refused());
    }

    #[test]
    fn without_a_password_from_the_source_it_asks() {
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://admin@srv").unwrap());
        s.password_source = true;
        s.start();
        let fx = s.password_fetched(Err("No password saved yet.".into()), "");
        let text = shown(&fx);
        assert!(text.contains("No password saved yet."), "{text:?}");
        assert!(text.contains("admin's password on srv"), "{text:?}");
        assert!(!fx.iter().any(|e| matches!(e, Effect::Send(_))));
        let fx = s.input(b"typed\r");
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Send(Message::WinrmConnect { .. })))
        );
        assert_eq!(login_password(&mut s).as_deref(), Some("typed"));
    }

    #[test]
    fn a_typed_password_is_kept_once_the_login_works() {
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://LAB\\admin@srv").unwrap());
        s.remember_password = true;
        s.start();
        s.input(b"pw\r");
        let fx = s.on_worker(connected_msg());
        assert_eq!(stored(&fx).as_deref(), Some("LAB\\admin@srv:5985=pw"));
        // Not without the Credential Manager as the source.
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://admin@srv").unwrap());
        s.start();
        s.input(b"pw\r");
        assert_eq!(stored(&s.on_worker(connected_msg())), None);
        // Nor when the login fails.
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://admin@srv").unwrap());
        s.remember_password = true;
        s.start();
        s.input(b"pw\r");
        let fx = s.on_worker(Message::Disconnected {
            channel: 5,
            reason: "Access denied: the server refused the login.".into(),
        });
        assert_eq!(stored(&fx), None);
    }

    #[test]
    fn a_refused_source_password_is_asked_for_next_time() {
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://admin@srv").unwrap());
        s.password_source = true;
        s.start();
        s.password_fetched(
            Ok(Zeroizing::new("old".into())),
            "the Windows Credential Manager",
        );
        s.on_worker(Message::Disconnected {
            channel: 5,
            reason: "Access denied: the server refused the login.".into(),
        });
        assert!(s.source_refused());
        // Once the shell was open, a drop later is not the password's fault.
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://admin@srv").unwrap());
        s.password_source = true;
        s.start();
        s.password_fetched(Ok(Zeroizing::new("ok".into())), "pass");
        s.on_worker(connected_msg());
        s.on_worker(Message::Disconnected {
            channel: 5,
            reason: "The server closed the connection.".into(),
        });
        assert!(!s.source_refused());
        // Nor is a network failure while logging in.
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://admin@srv").unwrap());
        s.password_source = true;
        s.start();
        s.password_fetched(Ok(Zeroizing::new("ok".into())), "pass");
        s.on_worker(Message::Disconnected {
            channel: 5,
            reason: "Could not reach srv:5985: timed out.".into(),
        });
        assert!(!s.source_refused());
    }

    /// A broker answer that finished a TLS handshake, with `cert`.
    fn handshake_done(s: &mut WinrmSession, conn: u8, cert: ServerCert) -> Vec<Effect> {
        s.on_broker(ferossh_auth::Reply::Sspi {
            login: 5,
            conn,
            output: SspiOutput {
                result: SspiResult::Tls {
                    send: Vec::new(),
                    plain: Vec::new(),
                    ready: true,
                    closed: false,
                },
                package: None,
                cert: Some(cert),
            },
        })
    }

    fn replied(effects: &[Effect]) -> Option<&SspiResult> {
        effects.iter().find_map(|e| match e {
            Effect::Send(Message::SspiReply { result, .. }) => Some(result),
            _ => None,
        })
    }

    fn https_session(store: Option<PathBuf>) -> WinrmSession {
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrms://srv").unwrap());
        s.cert_store = store;
        let fx = s.start();
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Send(Message::WinrmConnect {
                https: true,
                port: 5986,
                ..
            })
        )));
        // The worker's TLS steps go to the broker, without credentials.
        let fx = s.on_worker(Message::SspiRequest {
            channel: 5,
            conn: 0,
            op: SspiOp::TlsStart,
        });
        assert!(matches!(
            &fx[..],
            [Effect::Broker(Job::Sspi(SspiJob {
                op: SspiOp::TlsStart,
                credentials: None,
                ..
            }))]
        ));
        s
    }

    fn cert(sha: &str, windows: Result<(), String>) -> ServerCert {
        ServerCert {
            sha256: sha.into(),
            windows,
        }
    }

    #[test]
    fn a_certificate_windows_trusts_goes_through() {
        let mut s = https_session(None);
        let fx = handshake_done(&mut s, 0, cert("AA", Ok(())));
        assert!(shown(&fx).contains("checked by Windows"), "{fx:?}");
        assert!(matches!(
            replied(&fx),
            Some(SspiResult::Tls { ready: true, .. })
        ));
    }

    #[test]
    fn an_untrusted_certificate_is_asked_about_and_remembered() {
        let dir = std::env::temp_dir().join(format!("ferossh-winrm-certs-{}", std::process::id()));
        let store = dir.join("winrm_certs");
        let _ = std::fs::remove_file(&store);
        let mut s = https_session(Some(store.clone()));
        let fx = handshake_done(&mut s, 0, cert("AB:CD", Err("self-signed".into())));
        let text = shown(&fx);
        assert!(
            text.contains("self-signed") && text.contains("AB:CD"),
            "{text:?}"
        );
        assert!(text.contains("(yes/no)"));
        // Held until the user answers.
        assert!(replied(&fx).is_none());
        let fx = s.input(b"yes\r");
        assert!(matches!(
            replied(&fx),
            Some(SspiResult::Tls { ready: true, .. })
        ));
        assert_eq!(known_cert(&store, "srv:5986").as_deref(), Some("AB:CD"));
        // The input connection's handshake shows the same certificate.
        let fx = handshake_done(&mut s, 1, cert("AB:CD", Err("self-signed".into())));
        assert!(matches!(replied(&fx), Some(SspiResult::Tls { .. })));
        // Next time it is known.
        let mut s = https_session(Some(store.clone()));
        let fx = handshake_done(&mut s, 0, cert("AB:CD", Err("self-signed".into())));
        assert!(shown(&fx).contains("trusted before"), "{fx:?}");
        assert!(matches!(replied(&fx), Some(SspiResult::Tls { .. })));
        // Another certificate for the same server is refused outright.
        let mut s = https_session(Some(store.clone()));
        let fx = handshake_done(&mut s, 0, cert("EE:FF", Err("self-signed".into())));
        assert!(shown(&fx).contains("not the one you trusted"), "{fx:?}");
        assert!(matches!(replied(&fx), Some(SspiResult::Failed(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_certificate_not_trusted_ends_the_login() {
        let mut s = https_session(None);
        handshake_done(&mut s, 0, cert("AB", Err("expired".into())));
        let fx = s.input(b"no\r");
        assert!(matches!(replied(&fx), Some(SspiResult::Failed(_))));
        // Kept for nothing: no store.
        let mut s = https_session(None);
        handshake_done(&mut s, 0, cert("AB", Err("expired".into())));
        assert!(matches!(
            replied(&s.input(b"\x03")),
            Some(SspiResult::Failed(_))
        ));
    }

    #[test]
    fn an_http_session_never_starts_tls() {
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm srv").unwrap());
        s.start();
        let fx = s.on_worker(Message::SspiRequest {
            channel: 5,
            conn: 0,
            op: SspiOp::TlsStart,
        });
        assert!(fx.iter().any(|e| matches!(e, Effect::Ended)), "{fx:?}");
    }

    #[test]
    fn reads_what_was_typed() {
        let t = WinrmTarget::typed("winrm://admin@10.0.0.5:5986/").unwrap();
        assert_eq!(
            (t.hostname.as_str(), t.port, t.user.as_deref()),
            ("10.0.0.5", 5986, Some("admin"))
        );
        // 5986 is WinRM's HTTPS port.
        assert!(t.https);
        assert_eq!(t.label(), "10.0.0.5");
        let t = WinrmTarget::typed("winrms://srv").unwrap();
        assert_eq!((t.port, t.https), (5986, true));
        let t = WinrmTarget::typed("winrms://srv:443").unwrap();
        assert_eq!((t.port, t.https, t.label()), (443, true, "srv:443".into()));
        let t = WinrmTarget::typed("WINRM win-srv").unwrap();
        assert_eq!(
            (t.port, t.user.as_deref(), t.label(), t.https),
            (5985, None, "win-srv".into(), false)
        );
        assert_eq!(t.command, DEFAULT_WINRM_COMMAND);
        let t = WinrmTarget::typed("winrm jurgen@lab.test@srv 5985").unwrap();
        assert_eq!(t.user.as_deref(), Some("jurgen@lab.test"));
        assert_eq!(WinrmTarget::typed("srv"), None);
        assert_eq!(WinrmTarget::typed("winrm -oProxyCommand=calc"), None);
        assert_eq!(WinrmTarget::typed("winrm a 1 2"), None);
    }

    #[test]
    fn resolves_saved_connections() {
        let env = Env {
            home: std::env::temp_dir(),
            local_user: "me".into(),
            vars: Vec::new(),
            config: "Host dc\n    HostName dc1.lab.test\n    User LAB\\admin\n    Tag production\n"
                .into(),
        };
        let t = WinrmTarget::resolve("dc", &env, Some("cmd"), false, None).unwrap();
        assert_eq!((t.hostname.as_str(), t.port), ("dc1.lab.test", 5985));
        assert_eq!(t.user.as_deref(), Some("LAB\\admin"));
        assert_eq!(t.command, "cmd");
        assert_eq!(t.tag.as_deref(), Some("production"));
        let t = WinrmTarget::resolve("dc", &env, Some("  "), false, None).unwrap();
        assert_eq!(t.command, DEFAULT_WINRM_COMMAND);
        // A saved connection keeps its own user and port over an SSH
        // `Host *` block, and a blank user stays blank.
        let env = Env {
            config: "Host *\n    User sshuser\n    Port 2222\n".into(),
            ..env
        };
        let saved = SavedHost {
            alias: "dc".into(),
            hostname: Some("dc1.lab.test".into()),
            ..SavedHost::default()
        };
        let t = WinrmTarget::resolve("dc", &env, None, false, Some(&saved)).unwrap();
        assert_eq!((t.hostname.as_str(), t.port), ("dc1.lab.test", 5985));
        assert_eq!(t.user, None);
        let saved = SavedHost {
            user: Some("LAB\\admin".into()),
            port: Some(5986),
            ..saved
        };
        let t = WinrmTarget::resolve("dc", &env, None, false, Some(&saved)).unwrap();
        assert_eq!((t.port, t.user.as_deref()), (5986, Some("LAB\\admin")));
    }

    #[test]
    fn asks_for_the_password_and_relays_logins() {
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm://LAB\\admin@srv").unwrap());
        let fx = s.start();
        assert!(shown(&fx).contains("LAB\\admin's password on srv"));
        // The password is not echoed and not sent to the worker.
        let fx = s.input(b"se");
        assert!(fx.is_empty());
        let fx = s.input(b"cret\x7ft\r");
        assert!(matches!(
            fx.last(),
            Some(Effect::Send(Message::WinrmConnect { port: 5985, .. }))
        ));
        assert!(!shown(&fx).contains("secret"));
        let fx = s.on_worker(Message::SspiRequest {
            channel: 5,
            conn: 0,
            op: SspiOp::Start,
        });
        let [Effect::Broker(Job::Sspi(job))] = &fx[..] else {
            panic!("{fx:?}")
        };
        assert_eq!(job.host, "srv");
        let c = job.credentials.as_ref().unwrap();
        assert_eq!(
            (c.user.as_str(), c.password.as_str()),
            ("LAB\\admin", "secret")
        );
        // Later steps carry no credentials.
        let fx = s.on_worker(Message::SspiRequest {
            channel: 5,
            conn: 0,
            op: SspiOp::Step(vec![1]),
        });
        let [Effect::Broker(Job::Sspi(job))] = &fx[..] else {
            panic!()
        };
        assert!(job.credentials.is_none());
        let fx = s.on_broker(ferossh_auth::Reply::Sspi {
            login: 5,
            conn: 0,
            output: SspiOutput {
                result: SspiResult::Done(Vec::new()),
                package: Some("NTLM".into()),
                cert: None,
            },
        });
        assert!(matches!(
            &fx[..],
            [Effect::Send(Message::SspiReply {
                channel: 5,
                conn: 0,
                ..
            })]
        ));
        let fx = s.on_worker(Message::Connected {
            channel: 5,
            kex: String::new(),
            cipher: String::new(),
            host_key: String::new(),
        });
        assert!(shown(&fx).contains("with NTLM"));
        assert!(shown(&fx).contains("does not prove the server's identity"));
        assert!(matches!(fx.last(), Some(Effect::Connected)));
        assert!(s.password.is_none());
    }

    #[test]
    fn edits_lines_locally() {
        let mut s = connected(false);
        let fx = s.input(b"dirx\x7f\r");
        assert_eq!(sent(&fx), vec![b"dir\r\n".to_vec()]);
        assert!(shown(&fx).starts_with("dirx\x08 \x08\r\n"));
        // CR LF is one Enter; a paste of two lines sends two.
        let fx = s.input(b"a\r\nb\n");
        assert_eq!(sent(&fx), vec![b"a\r\n".to_vec(), b"b\r\n".to_vec()]);
        // Up recalls, Down goes back to the draft.
        let fx = s.input(b"dr\x1b[A");
        assert!(shown(&fx).ends_with('b'));
        s.input(b"\x1b[A\x1b[A");
        assert_eq!(s.line, "dir");
        s.input(b"\x1b[B\x1b[B\x1b[B");
        assert_eq!(s.line, "dr");
        // Other keys' sequences are ignored, Ctrl+U clears.
        s.input(b"\x1b[3~\x1bOP\x15");
        assert_eq!(s.line, "");
        let fx = s.input(b"long\x03");
        assert_eq!(sent(&fx), vec![vec![3]]);
        assert_eq!(s.line, "");
    }

    #[test]
    fn shows_output_and_ends() {
        let mut s = connected(true);
        let fx = s.on_worker(Message::Data {
            channel: 5,
            bytes: b"PS C:\\> ".to_vec(),
        });
        assert!(matches!(&fx[..], [Effect::Output(b)] if b == b"PS C:\\> "));
        let fx = s.on_worker(Message::ExecStderr {
            channel: 5,
            bytes: b"oops".to_vec(),
        });
        assert!(shown(&fx).contains("\x1b[0;31moops"));
        let fx = s.on_worker(Message::Disconnected {
            channel: 5,
            reason: "The command ended (exit code 0).".into(),
        });
        assert!(shown(&fx).contains("Press Enter to reconnect"));
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Broker(Job::SspiEnd(5))))
        );
        assert_eq!(s.state(), State::Ended);
    }

    #[test]
    fn closing_keeps_sealing_until_the_worker_is_done() {
        // Before the shell is open: the keys go at once.
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm srv").unwrap());
        s.start();
        let fx = s.close();
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Broker(Job::SspiEnd(5))))
        );
        assert!(s.take_closer().is_none());
        // With the shell open, a closer seals the goodbye.
        let mut s = connected(false);
        let fx = s.close();
        assert!(matches!(
            &fx[..],
            [Effect::Send(Message::Close { channel: 5 })]
        ));
        let mut c = s.take_closer().unwrap();
        let (fx, done) = c.on_worker(Message::SspiRequest {
            channel: 5,
            conn: 1,
            op: SspiOp::Seal(b"terminate".to_vec()),
        });
        assert!(!done);
        assert!(matches!(
            &fx[..],
            [Effect::Broker(Job::Sspi(SspiJob {
                login: 5,
                conn: 1,
                credentials: None,
                ..
            }))]
        ));
        let fx = c.on_broker(ferossh_auth::Reply::Sspi {
            login: 5,
            conn: 1,
            output: ferossh_auth::negotiate::SspiOutput {
                result: SspiResult::Sealed {
                    header: vec![1],
                    data: vec![2],
                },
                package: None,
                cert: None,
            },
        });
        assert!(matches!(
            &fx[..],
            [Effect::Send(Message::SspiReply {
                channel: 5,
                conn: 1,
                ..
            })]
        ));
        // Output after the close is ignored; a new login ends it.
        let (fx, done) = c.on_worker(Message::Data {
            channel: 5,
            bytes: vec![1],
        });
        assert!(fx.is_empty() && !done);
        let (fx, done) = c.on_worker(Message::Disconnected {
            channel: 5,
            reason: String::new(),
        });
        assert!(done);
        assert!(matches!(&fx[..], [Effect::Broker(Job::SspiEnd(5))]));
        // A worker cut off gets no closer.
        let mut s = connected(false);
        let fx = s.on_worker(Message::IdentitiesRequest { channel: 5 });
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Broker(Job::SspiEnd(5))))
        );
        assert!(s.take_closer().is_none());
    }

    #[test]
    fn a_worker_that_asks_for_other_things_is_cut_off() {
        for msg in [
            Message::HostKey {
                channel: 5,
                key: vec![1],
            },
            Message::IdentitiesRequest { channel: 5 },
            Message::SspiRequest {
                channel: 5,
                conn: 2,
                op: SspiOp::Start,
            },
            // A new login once connected.
            Message::SspiRequest {
                channel: 5,
                conn: 0,
                op: SspiOp::Start,
            },
        ] {
            let mut s = connected(false);
            let fx = s.on_worker(msg);
            assert!(shown(&fx).contains("closed the connection"), "{fx:?}");
            assert_eq!(s.state(), State::Ended);
        }
        // Data before the connection is reported, and too many logins.
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm srv").unwrap());
        s.start();
        let fx = s.on_worker(Message::Data {
            channel: 5,
            bytes: vec![1],
        });
        assert!(shown(&fx).contains("closed the connection"));
        let mut s = WinrmSession::new(5, WinrmTarget::typed("winrm srv").unwrap());
        s.start();
        for _ in 0..MAX_STARTS {
            s.on_worker(Message::SspiRequest {
                channel: 5,
                conn: 0,
                op: SspiOp::Start,
            });
        }
        assert_eq!(s.state(), State::Connecting);
        s.on_worker(Message::SspiRequest {
            channel: 5,
            conn: 1,
            op: SspiOp::Start,
        });
        assert_eq!(s.state(), State::Ended);
    }
}
