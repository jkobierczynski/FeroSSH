//! The UI side of one SSH connection.
//!
//! [`SshSession`] answers the worker's questions: it checks host keys
//! against known_hosts and asks the user about new ones, lists keys and
//! signs through the broker after checking each request, and asks for
//! passwords, passphrases and one-time codes. Prompts are shown in the
//! terminal pane itself, as OpenSSH does in a console, and typed into a
//! local line editor that never reaches the host or the log.
//!
//! A host reached through jump hosts (`ProxyJump`) is a chain of logins,
//! each in a worker of its own: every jump host logs in without a shell
//! and its worker opens a `direct-tcpip` stream to the next hop, over
//! which the next hop's worker runs its own SSH connection. The window
//! only relays that stream's bytes ([`Effect::Tunnel`]), which are the
//! next hop's encrypted connection: a jump host that took over its worker
//! sees no more of the target than the jump host itself does. Each hop
//! checks its own host key against known_hosts and signs its own login.
//! A `ProxyCommand` is a local program whose input and output the window
//! relays the same way ([`Effect::ProxyCommand`]).
//!
//! It only returns [`Effect`]s; the window carries them out. That keeps
//! this logic platform-neutral and tested on any host.

use std::cell::Cell;
use std::path::PathBuf;

use ferossh_auth::{Identity, Job, KeyFileSpec, Reply, SignError, Source, userauth};
use ferossh_config::known_hosts::{self, KnownHosts, Status};
use ferossh_config::{ForwardSpec, hostkey, ssh_config};
use ferossh_ipc::{
    ChannelId, MAX_DATA_CHUNK, Message, PromptField, PromptKind, Secrets, Timing, X11Auth,
};
use ferossh_worker::crypto_policy;
use zeroize::Zeroizing;

/// Wrong passphrases allowed per key before it is skipped.
const PASSPHRASE_TRIES: u8 = 3;

const DIM: &str = "\x1b[0;90m";
const RED: &str = "\x1b[0;31m";
const AMBER: &str = "\x1b[0;33m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// A host certificate as the worker reports it.
#[derive(Debug)]
struct HostCert {
    ca: Vec<u8>,
    key: Vec<u8>,
    principals: Vec<String>,
    /// Valid from, valid before: seconds since the Unix epoch.
    valid: (u64, u64),
    key_id: String,
    serial: u64,
}

/// Where to connect, after applying `ssh_config`.
#[derive(Debug, Clone)]
pub struct Target {
    /// What the user typed, which names the tab.
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub key_files: Vec<KeyFileSpec>,
    /// ssh_config's `Tag`, which picks a colour theme.
    pub tag: Option<String>,
    /// Port forwards from ssh_config (after `ClearAllForwardings`).
    pub forwards: Vec<ForwardSpec>,
    pub forward_x11: bool,
    pub gateway_ports: bool,
    /// ConnectTimeout and ServerAlive*, over FeroSSH's defaults.
    pub timing: Timing,
    /// The jump hosts to go through (`ProxyJump`), first first, nested
    /// ones included; each without jumps of its own.
    pub jumps: Vec<Target>,
    /// The program that carries the connection to the first hop
    /// (`ProxyCommand`): the first jump host, or this host without jumps.
    pub proxy_command: Option<ProxyCommand>,
}

/// A `ProxyCommand` to run: never through a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyCommand {
    /// The line as the config has it, before the tokens are filled in:
    /// what the user approves once.
    pub line: String,
    pub program: String,
    pub args: Vec<String>,
}

/// The user's environment the target is resolved in.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub local_user: String,
    /// The text of `~/.ssh/config`, if any, followed by the hosts
    /// imported from PuTTY. `Include` lines are read relative to `home`.
    pub config: String,
}

impl Env {
    pub fn known_hosts(&self) -> PathBuf {
        self.home.join(".ssh").join("known_hosts")
    }
}

impl Target {
    /// Resolves `[user@]host[:port]` with the config; `None` if it does not
    /// parse.
    pub fn resolve(typed: &str, env: &Env) -> Option<Self> {
        Self::resolve_route(typed, env).ok()
    }

    /// Like [`Self::resolve`], and says what is wrong: a name that does
    /// not parse, or jump hosts that loop or nest too deep.
    pub fn resolve_route(typed: &str, env: &Env) -> Result<Self, String> {
        Self::resolve_at(typed, env, 0)
    }

    /// `depth` counts the jump hosts already on the way here.
    fn resolve_at(typed: &str, env: &Env, depth: usize) -> Result<Self, String> {
        let mut target = Self::resolve_host(typed, env)
            .ok_or_else(|| format!("\"{}\" is not a host name.", sanitize(typed)))?;
        let includes = ssh_config::DirIncludes {
            home: env.home.clone(),
        };
        let proxy = ssh_config::resolve_in(&env.config, &target.alias, &includes).proxy;
        match proxy {
            Some(ssh_config::Proxy::Jump(hops)) => {
                for (i, hop) in hops.iter().enumerate() {
                    if depth + target.jumps.len() >= ssh_config::MAX_HOPS {
                        return Err(format!(
                            "The jump hosts on the way to {} go more than {} deep; do they \
                             loop through each other?",
                            sanitize(typed),
                            ssh_config::MAX_HOPS
                        ));
                    }
                    // As in OpenSSH, only the first jump host's own
                    // ProxyJump or ProxyCommand applies: it is reached as
                    // a host of its own, the others through the one before.
                    let mut hop = if i == 0 {
                        Self::resolve_at(hop, env, depth + 1)?
                    } else {
                        Self::resolve_host(hop, env).ok_or_else(|| {
                            format!("The jump host \"{}\" is not a host name.", sanitize(hop))
                        })?
                    };
                    if i == 0 {
                        target.proxy_command = hop.proxy_command.take();
                        target.jumps.append(&mut hop.jumps);
                    }
                    target.jumps.push(hop);
                }
            }
            Some(ssh_config::Proxy::Command(argv)) => {
                let t = &target;
                target.proxy_command =
                    ssh_config::proxy_command(&argv, &t.hostname, t.port, &t.user, &t.alias).map(
                        |(program, args)| ProxyCommand {
                            line: argv.join(" "),
                            program,
                            args,
                        },
                    );
            }
            Some(ssh_config::Proxy::Direct) | None => {}
        }
        Ok(target)
    }

    /// The host itself, without the way there.
    fn resolve_host(typed: &str, env: &Env) -> Option<Self> {
        let (user, alias, port) = ssh_config::parse_destination(typed)?;
        let includes = ssh_config::DirIncludes {
            home: env.home.clone(),
        };
        let cfg = ssh_config::resolve_in(&env.config, &alias, &includes);
        let forwards = cfg.active_forwards().to_vec();
        let home = env.home.to_string_lossy();
        let user = user.or(cfg.user).unwrap_or_else(|| env.local_user.clone());
        let expand = |s: &str| ssh_config::expand_path(s, &home, &env.local_user, &alias, &user);
        let hostname = cfg
            .hostname
            .map(|h| expand(&h))
            .unwrap_or_else(|| alias.clone());
        let mut key_files: Vec<KeyFileSpec> = cfg
            .identity_files
            .iter()
            .map(|f| KeyFileSpec {
                path: PathBuf::from(expand(f)),
                explicit: true,
            })
            .collect();
        // Like ssh: the default keys only when the config names none.
        if key_files.is_empty() {
            for name in ["id_ed25519", "id_ecdsa", "id_ecdsa_sk", "id_ed25519_sk"] {
                key_files.push(KeyFileSpec {
                    path: env.home.join(".ssh").join(name),
                    explicit: false,
                });
            }
        }
        Some(Self {
            port: port.or(cfg.port).unwrap_or(22),
            forwards,
            forward_x11: cfg.forward_x11 == Some(true),
            gateway_ports: cfg.gateway_ports == Some(true),
            timing: {
                let d = Timing::default();
                Timing {
                    connect_timeout: cfg.connect_timeout.unwrap_or(d.connect_timeout),
                    alive_interval: cfg.server_alive_interval.unwrap_or(d.alive_interval),
                    alive_max: cfg.server_alive_count_max.unwrap_or(d.alive_max),
                }
            },
            tag: cfg.tag,
            alias,
            hostname,
            user,
            key_files,
            jumps: Vec::new(),
            proxy_command: None,
        })
    }

    /// Whether `other` logs in to the same host, port and user, the same
    /// way.
    pub fn same_login(&self, other: &Target) -> bool {
        self.hostname.eq_ignore_ascii_case(&other.hostname)
            && self.port == other.port
            && self.user == other.user
            && self.route() == other.route()
    }

    /// The way there, for the log and for telling routes apart: the jump
    /// hosts' labels, or the command.
    pub fn route(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .proxy_command
            .iter()
            .map(|c| format!("command {}", c.line))
            .collect();
        out.extend(self.jumps.iter().map(Target::label));
        out
    }

    /// `user@host` or `user@host:port`, for titles and the log.
    pub fn label(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.user, self.alias)
        } else {
            format!("{}@{}:{}", self.user, self.alias, self.port)
        }
    }
}

/// What the window must do.
#[derive(Debug)]
pub enum Effect {
    /// Send to the worker.
    Send(Message),
    /// Show local text (prompts, notes) in the terminal; never logged.
    Show(String),
    /// Bytes from the host: feed the terminal and the log.
    Output(Vec<u8>),
    /// Hand a job to the signing broker.
    Broker(Job),
    /// Logged in: the shell is open.
    Connected,
    /// Get the login password from the password source of the host
    /// [`SshSession::login_target`] names, then call
    /// [`SshSession::password_fetched`].
    FetchPassword,
    /// The password typed for this login worked: keep it in the Windows
    /// Credential Manager for `user` at `host`:`port`. One string;
    /// `Secrets` keeps it out of `Debug`.
    StorePassword {
        user: String,
        host: String,
        port: u16,
        password: Secrets,
    },
    /// Relay stream `channel` between the worker of connection `from`
    /// (a jump host's, which opens it) and that of `to` (the next hop's,
    /// whose SSH connection runs over it), before the messages that open
    /// it are sent.
    Tunnel {
        from: ChannelId,
        to: ChannelId,
        channel: ChannelId,
    },
    /// Run `command` and relay its input and output as stream `tunnel` of
    /// the worker of connection `connection`, whose SSH connection runs
    /// over it. Without that, close the stream: the login then ends.
    ProxyCommand {
        connection: ChannelId,
        tunnel: ChannelId,
        command: ProxyCommand,
    },
    /// The connection is gone.
    Ended,
    /// The connection dropped (or a reconnect could not reach the host):
    /// try again after a pause. [`Effect::Ended`] follows.
    Reconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Connecting,
    /// The host key changed: nothing more happens until the user replaces
    /// the key or connects elsewhere.
    Blocked,
    Connected,
    Ended,
}

/// What a finished local prompt is for.
enum Purpose {
    TrustHostKey {
        key: Vec<u8>,
    },
    /// One field of a server prompt; earlier answers are kept.
    /// `password` marks the login password.
    Answer {
        fields: Vec<PromptField>,
        answers: Secrets,
        password: bool,
    },
    Passphrase {
        identity: Identity,
        data: Vec<u8>,
        flags: u32,
    },
}

struct LocalPrompt {
    echo: bool,
    text: Zeroizing<String>,
    purpose: Purpose,
}

/// One connection, from the UI's side.
pub struct SshSession {
    pub channel: ChannelId,
    /// The channel that made the SSH connection this shell runs on: its own
    /// for a fresh login, another tab's for a shell opened on it.
    pub connection: ChannelId,
    pub target: Target,
    known_hosts: PathBuf,
    state: State,
    prompt: Option<LocalPrompt>,
    identities: Vec<Identity>,
    /// The sign request waiting for the broker.
    signing: Option<(Identity, Vec<u8>, u32)>,
    passphrase_tries: u8,
    /// The key that blocked the connection, for "Replace host key".
    changed_key: Option<Vec<u8>>,
    pub exit_code: Option<u32>,
    /// X11 forwarding for the shell, when the host has it on.
    pub x11: Option<X11Auth>,
    /// Also offer the legacy algorithms (`LegacyAlgorithms yes`).
    pub legacy: bool,
    /// The connection negotiated one of the legacy algorithms.
    legacy_in_use: bool,
    /// The host's profile names a password source to try first.
    pub password_source: bool,
    /// Keep a typed password that worked (the Windows Credential Manager).
    pub remember_password: bool,
    /// The kind of the server prompt being answered.
    prompt_kind: Option<PromptKind>,
    /// The source was asked once; a second password prompt is typed.
    source_tried: bool,
    /// The source gave the password just sent.
    source_used: bool,
    /// The password prompt waiting for the source, and the fields after it.
    fetching: Option<(PromptField, Vec<PromptField>)>,
    /// The password typed for this login, to keep once it worked.
    typed_password: Option<Zeroizing<String>>,
    /// Where this login is, for checking that the worker asks in turn.
    login: Login,
    /// Connect again on its own when the connection drops.
    pub auto_reconnect: bool,
    /// This session is a reconnect after a drop: failing to reach the
    /// host tries again too.
    pub retrying: bool,
    /// The worker said the connection broke under the open shell.
    lost: bool,
    /// The jump hosts on the way, first first, each logging in with a
    /// session and a worker of its own.
    hops: Vec<Hop>,
    /// The stream this login's connection runs over, instead of TCP: the
    /// previous hop's, or a `ProxyCommand`'s.
    tunnel: Option<ChannelId>,
    /// The `ProxyCommand` this login's stream comes from.
    proxy: Option<ProxyCommand>,
    /// A jump host's login: no shell, only the next hop's stream.
    jump: bool,
    /// The pane's size, for a login that starts after its jump hosts'.
    size: Cell<(u16, u16)>,
    /// The end of a login whose stream the jump host before it has not
    /// yet said is gone: held until it says why, so the reason shows
    /// first. The two workers' messages come in any order.
    held: Option<Message>,
}

/// A jump host on the way to the target: its login, and the stream its
/// worker opens to the next hop once logged in.
struct Hop {
    session: SshSession,
    tunnel: ChannelId,
    /// The jump host's worker said the stream ended.
    tunnel_ended: bool,
}

/// How far a login has come. The worker may only ask for what the honest
/// order needs: one host key verdict, then keys, signatures and prompts,
/// and nothing of the kind once the shell is open or on a shell opened on
/// an existing connection. A worker that asks out of turn is treated as
/// compromised and the connection is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Login {
    /// This session sent [`Message::Connect`]: it logs in itself.
    started: bool,
    /// The worker asked about the host key (or certificate).
    host_asked: bool,
    /// The host key was trusted.
    host_trusted: bool,
    /// Signatures asked for so far.
    signatures: u32,
    /// The login worked.
    done: bool,
}

/// Signatures one login may ask for: one per key tried, with room for
/// partial-success logins that need several. OpenSSH tries at most a few
/// dozen keys before the server's MaxAuthTries ends it anyway.
const MAX_SIGNATURES: u32 = 32;

/// Questions in one prompt. OpenSSH's sshd sends one or two; PAM stacks a
/// few more.
const MAX_PROMPT_FIELDS: usize = 16;

impl std::fmt::Debug for SshSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshSession")
            .field("channel", &self.channel)
            .field("target", &self.target)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// A prompt that asks for the login password: the `password` method, or
/// a keyboard-interactive round whose only question is a hidden
/// "Password:" (PAM's way).
fn is_password_prompt(kind: PromptKind, fields: &[PromptField]) -> bool {
    match (kind, fields) {
        (PromptKind::Password, [_]) => true,
        (PromptKind::KeyboardInteractive, [f]) => {
            !f.echo && f.text.to_ascii_lowercase().contains("password")
        }
        _ => false,
    }
}

/// Server-supplied text made safe to show: printable characters, tabs and
/// line breaks only, so a banner cannot send escape sequences to the
/// terminal (OpenSSH filters the same way).
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\r\n"),
            '\t' => out.push(c),
            c if c.is_control() => {}
            // Bidirectional overrides could disguise the text.
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {}
            c => out.push(c),
        }
    }
    out
}

fn dim(text: &str) -> String {
    format!("{DIM}{text}{RESET}\r\n")
}

impl SshSession {
    pub fn new(channel: ChannelId, target: Target, known_hosts: PathBuf) -> Self {
        Self {
            channel,
            connection: channel,
            target,
            known_hosts,
            state: State::Connecting,
            prompt: None,
            identities: Vec::new(),
            signing: None,
            passphrase_tries: 0,
            changed_key: None,
            exit_code: None,
            x11: None,
            legacy: false,
            legacy_in_use: false,
            password_source: false,
            remember_password: false,
            prompt_kind: None,
            source_tried: false,
            source_used: false,
            fetching: None,
            typed_password: None,
            login: Login::default(),
            auto_reconnect: false,
            retrying: false,
            lost: false,
            hops: Vec::new(),
            tunnel: None,
            proxy: None,
            jump: false,
            size: Cell::new((80, 24)),
            held: None,
        }
    }

    /// Sets up the logins on the way to the target from its jump hosts
    /// and `ProxyCommand`, with channel ids from `next`: each jump host
    /// logs in on a channel (and so in a worker) of its own.
    pub fn with_route(mut self, next: &mut dyn FnMut() -> ChannelId) -> Self {
        for target in self.target.jumps.clone() {
            let mut session = SshSession::new(next(), target, self.known_hosts.clone());
            session.jump = true;
            session.tunnel = self.hops.last().map(|h| h.tunnel);
            self.hops.push(Hop {
                session,
                tunnel: next(),
                tunnel_ended: false,
            });
        }
        self.tunnel = self.hops.last().map(|h| h.tunnel);
        if let Some(command) = self.target.proxy_command.clone() {
            let tunnel = Some(next());
            let first = match self.hops.first_mut() {
                Some(h) => &mut h.session,
                None => &mut self,
            };
            first.proxy = Some(command);
            first.tunnel = tunnel;
        }
        self
    }

    /// The jump hosts' logins, to give them their profiles' settings.
    pub fn hops_mut(&mut self) -> impl Iterator<Item = &mut SshSession> {
        self.hops.iter_mut().map(|h| &mut h.session)
    }

    /// The connections of the jump hosts' logins, each needing a worker.
    pub fn hop_connections(&self) -> Vec<ChannelId> {
        self.hops.iter().map(|h| h.session.channel).collect()
    }

    /// Whether `channel` is this session's: its shell, a jump host's
    /// login, or a jump host's stream to the next hop.
    pub fn owns(&self, channel: ChannelId) -> bool {
        channel == self.channel
            || self
                .hops
                .iter()
                .any(|h| h.session.channel == channel || h.tunnel == channel)
    }

    /// The connection whose worker sends and receives the messages of
    /// `channel`; `None` for a channel not this session's.
    pub fn worker_for(&self, channel: Option<ChannelId>) -> Option<ChannelId> {
        let channel = channel?;
        if channel == self.channel {
            return Some(self.connection);
        }
        self.hops
            .iter()
            .find(|h| h.session.channel == channel || h.tunnel == channel)
            .map(|h| h.session.channel)
    }

    /// The login that runs in the worker of `connection`, if one does.
    pub fn login_on(&self, connection: ChannelId) -> Option<ChannelId> {
        if connection == self.connection {
            return Some(self.channel);
        }
        self.hops
            .iter()
            .find(|h| h.session.channel == connection)
            .map(|h| h.session.channel)
    }

    /// The jump host logging in now, while one is.
    fn active_hop(&self) -> Option<usize> {
        if self.state != State::Connecting {
            return None;
        }
        self.hops
            .iter()
            .position(|h| h.session.state != State::Connected)
    }

    /// The host whose login asks the questions now: a jump host's, or
    /// this one's. Its profile picks the password source.
    pub fn login_target(&self) -> &Target {
        match self.active_hop().and_then(|i| self.hops.get(i)) {
            Some(h) => &h.session.target,
            None => &self.target,
        }
    }

    /// The jump host whose changed host key stopped the login, else this
    /// session.
    fn blocked(&self) -> &SshSession {
        self.hops
            .iter()
            .map(|h| &h.session)
            .find(|s| s.state == State::Blocked)
            .unwrap_or(self)
    }

    /// The host whose changed host key blocked the connection.
    pub fn blocked_host(&self) -> &str {
        &self.blocked().target.hostname
    }

    /// Marks this login, and its jump hosts', as a reconnect after a
    /// drop.
    pub fn set_retrying(&mut self) {
        self.retrying = true;
        for h in &mut self.hops {
            h.session.retrying = true;
        }
    }

    /// Whether the connection uses one of the opt-in legacy algorithms,
    /// which the tab's badge shows.
    pub fn legacy_in_use(&self) -> bool {
        self.legacy_in_use && self.state == State::Connected
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn prompt_active(&self) -> bool {
        match self.active_hop().and_then(|i| self.hops.get(i)) {
            Some(h) => h.session.prompt_active(),
            None => self.prompt.is_some(),
        }
    }

    /// The host key that blocked the connection, if one did: this host's
    /// or a jump host's.
    pub fn changed_key(&self) -> Option<&[u8]> {
        self.blocked().changed_key.as_deref()
    }

    fn known(&self) -> KnownHosts {
        KnownHosts::load(&self.known_hosts).unwrap_or_default()
    }

    /// Starts connecting with a pane of `cols` x `rows`: the first jump
    /// host's login, or this one.
    pub fn start(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
        self.size.set((cols, rows));
        let Some(first) = self.hops.first_mut() else {
            return self.begin(None);
        };
        let route: Vec<String> = self.target.jumps.iter().map(Target::label).collect();
        let mut effects = vec![Effect::Show(dim(&sanitize(&format!(
            "Connecting to {} through {}...",
            self.target.label(),
            route.join(", then ")
        ))))];
        effects.extend(first.session.begin(None));
        effects
    }

    /// Starts this login; `via` names the jump host its stream comes from.
    fn begin(&mut self, via: Option<&str>) -> Vec<Effect> {
        let t = &self.target;
        let known = self.known();
        let mut host_key_algs = known.key_types(&t.hostname, t.port);
        if known.has_authority(&t.hostname, t.port) {
            host_key_algs.push(crypto_policy::CERT_AUTHORITY.to_owned());
        }
        let mut text = format!(
            "Connecting to {}{}",
            if self.jump { "jump host " } else { "" },
            t.label()
        );
        if t.hostname != t.alias {
            text.push_str(&format!(" ({})", t.hostname));
        }
        match (via, &self.proxy) {
            (Some(hop), _) => text.push_str(&format!(" through {hop}")),
            (None, Some(c)) => text.push_str(&format!(" through the command {}", c.program)),
            (None, None) => {}
        }
        text.push_str("...");
        self.login = Login {
            started: true,
            ..Login::default()
        };
        let (cols, rows) = self.size.get();
        let mut effects = vec![Effect::Show(dim(&sanitize(&text)))];
        if let (Some(command), Some(tunnel)) = (&self.proxy, self.tunnel) {
            effects.push(Effect::ProxyCommand {
                connection: self.channel,
                tunnel,
                command: command.clone(),
            });
        }
        effects.push(Effect::Send(Message::Connect {
            channel: self.channel,
            host: t.hostname.clone(),
            port: t.port,
            user: t.user.clone(),
            cols,
            rows,
            host_key_algs,
            x11: if self.jump { None } else { self.x11.clone() },
            legacy: self.legacy,
            timing: t.timing,
            tunnel: self.tunnel,
            shell: !self.jump,
        }));
        effects
    }

    /// What became of jump host `i`'s login: once in, its worker opens
    /// the stream to the next hop and the next hop logs in over it; if it
    /// fails, the whole connection does.
    fn after_hop(&mut self, i: usize, effects: Vec<Effect>) -> Vec<Effect> {
        let mut out = Vec::new();
        for effect in effects {
            match effect {
                Effect::Connected => out.extend(self.next_hop(i)),
                // Whether to try again is the whole route's call.
                Effect::Reconnect => {}
                Effect::Ended => {
                    // No word on its stream is coming any more.
                    if let Some(h) = self.hops.get_mut(i) {
                        h.tunnel_ended = true;
                    }
                    out.extend(self.hop_ended(i));
                    out.extend(self.release());
                }
                other => out.push(other),
            }
        }
        out
    }

    /// Jump host `i` is logged in: opens its stream to the next hop and
    /// starts that hop's login over it.
    fn next_hop(&mut self, i: usize) -> Vec<Effect> {
        let Some(hop) = self.hops.get(i) else {
            return Vec::new();
        };
        let (from, tunnel) = (hop.session.channel, hop.tunnel);
        let via = hop.session.target.label();
        let next = match self.hops.get_mut(i + 1) {
            Some(h) => &mut h.session,
            None => &mut *self,
        };
        let open = Message::ForwardOpen {
            channel: tunnel,
            connection: from,
            target: Some((next.target.hostname.clone(), next.target.port)),
            origin_host: "127.0.0.1".into(),
            origin_port: 0,
            // The stream holds the jump host's connection for as long as
            // the next hop uses it.
            keep: true,
        };
        let mut effects = vec![
            Effect::Tunnel {
                from,
                to: next.channel,
                channel: tunnel,
            },
            Effect::Send(open),
        ];
        effects.extend(next.begin(Some(&via)));
        effects
    }

    /// Jump host `i`'s login ended: before the target is in, that ends
    /// the connection.
    fn hop_ended(&mut self, i: usize) -> Vec<Effect> {
        if self.state != State::Connecting {
            return Vec::new();
        }
        let blocked = self
            .hops
            .get(i)
            .is_some_and(|h| h.session.state == State::Blocked);
        // A reconnect whose jump host is not reachable yet tries again.
        let unreachable = self.retrying
            && !blocked
            && self
                .hops
                .get(i)
                .is_some_and(|h| !h.session.login.host_asked);
        let mut effects = Vec::new();
        // The blocked one stays so, for Replace host key.
        for h in &mut self.hops {
            if matches!(h.session.state, State::Connecting | State::Connected) {
                effects.extend(h.session.close());
            }
        }
        if self.login.started {
            effects.push(Effect::Send(Message::Close {
                channel: self.channel,
            }));
        }
        self.prompt = None;
        self.state = if blocked {
            State::Blocked
        } else {
            State::Ended
        };
        if unreachable {
            effects.push(Effect::Reconnect);
        }
        effects.push(Effect::Ended);
        effects
    }

    /// A message about jump host `i`'s stream to the next hop.
    fn on_tunnel(&mut self, i: usize, msg: Message) -> Vec<Effect> {
        let Message::Disconnected { reason, .. } = msg else {
            return Vec::new();
        };
        if let Some(h) = self.hops.get_mut(i) {
            h.tunnel_ended = true;
        }
        let mut effects = self.tunnel_failed(i, &reason);
        effects.extend(self.release());
        effects
    }

    /// The end [`Self::held`], now that its reason has shown.
    fn release(&mut self) -> Vec<Effect> {
        match self.held.take() {
            Some(msg) => self.on_worker(msg),
            None => Vec::new(),
        }
    }

    /// Whether `msg` ends the login of hop `n` (the target after the
    /// jump hosts) before it got in, while the jump host before it has
    /// not yet said why its stream ended.
    fn holds(&self, n: usize, msg: &Message) -> bool {
        let login = match self.hops.get(n) {
            Some(h) => h.session.login,
            None => self.login,
        };
        matches!(msg, Message::Disconnected { .. })
            && login.started
            && !login.done
            && n.checked_sub(1)
                .and_then(|i| self.hops.get(i))
                .is_some_and(|h| !h.tunnel_ended)
    }

    fn tunnel_failed(&self, i: usize, reason: &str) -> Vec<Effect> {
        let Some(hop) = self.hops.get(i) else {
            return Vec::new();
        };
        let next_in = match self.hops.get(i + 1) {
            Some(h) => h.session.login.done,
            None => self.login.done,
        };
        if reason.is_empty() || next_in || self.state != State::Connecting {
            return Vec::new();
        }
        vec![Effect::Show(format!(
            "{RED}{}{RESET}\r\n",
            sanitize(&format!(
                "The jump host {} could not reach {reason}",
                hop.session.target.label()
            ))
        ))]
    }

    /// Opens a shell on the connection `connection` made, as another
    /// channel: no new login.
    pub fn start_on(&mut self, connection: ChannelId, cols: u16, rows: u16) -> Vec<Effect> {
        self.connection = connection;
        vec![
            Effect::Show(dim(&format!(
                "Opening a new shell on the connection to {}...",
                self.target.label()
            ))),
            Effect::Send(Message::OpenChannel {
                channel: self.channel,
                connection,
                cols,
                rows,
                x11: self.x11.clone(),
            }),
        ]
    }

    /// Whether a new tab for `target` can share this session's connection:
    /// same host, port and user, and logged in.
    pub fn can_share(&self, target: &Target) -> bool {
        self.state == State::Connected
            && self.target.hostname.eq_ignore_ascii_case(&target.hostname)
            && self.target.port == target.port
            && self.target.user == target.user
            // The same name through other jump hosts can be another host.
            && self.target.route() == target.route()
    }

    /// Typed or pasted bytes for the host, once connected.
    pub fn input(&self, bytes: &[u8]) -> Vec<Effect> {
        if self.state != State::Connected {
            return Vec::new();
        }
        bytes
            .chunks(MAX_DATA_CHUNK)
            .map(|c| {
                Effect::Send(Message::Data {
                    channel: self.channel,
                    bytes: c.to_vec(),
                })
            })
            .collect()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Option<Effect> {
        self.size.set((cols, rows));
        matches!(self.state, State::Connecting | State::Connected).then(|| {
            Effect::Send(Message::Resize {
                channel: self.channel,
                cols,
                rows,
            })
        })
    }

    /// Ends the connection from this side. The jump hosts' logins close
    /// too; their streams to the next hop keep their connections for as
    /// long as another tab still uses this one.
    pub fn close(&mut self) -> Vec<Effect> {
        self.prompt = None;
        self.held = None;
        let mut effects = Vec::new();
        for h in &mut self.hops {
            effects.extend(h.session.close());
        }
        if self.state == State::Ended {
            return effects;
        }
        self.state = State::Ended;
        effects.push(Effect::Send(Message::Close {
            channel: self.channel,
        }));
        effects
    }

    /// Why `msg` is not something the worker may send now, if it is not.
    fn out_of_turn(&self, msg: &Message) -> Option<&'static str> {
        let login = self.login;
        let logging_in = login.started && self.state == State::Connecting;
        match msg {
            Message::HostKey { .. } | Message::HostCert { .. } => {
                if !logging_in {
                    Some("a host key check outside a login")
                } else if login.host_asked {
                    Some("a second host key check")
                } else {
                    None
                }
            }
            Message::Banner { .. }
            | Message::IdentitiesRequest { .. }
            | Message::SignRequest { .. }
            | Message::Prompt { .. } => {
                if !logging_in {
                    Some("a login step outside a login")
                } else if !login.host_trusted {
                    Some("a login step before the host key was trusted")
                } else if matches!(msg, Message::SignRequest { .. })
                    && login.signatures >= MAX_SIGNATURES
                {
                    Some("too many signatures")
                } else if matches!(msg, Message::Prompt { fields, .. } if fields.len() > MAX_PROMPT_FIELDS)
                {
                    Some("a prompt with too many questions")
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// A message from a worker: this login's, a jump host's login's, or
    /// about a jump host's stream to the next hop. The caller checks that
    /// it came from [`Self::worker_for`] its channel.
    pub fn on_worker(&mut self, msg: Message) -> Vec<Effect> {
        let channel = msg.channel();
        if let Some(i) = self
            .hops
            .iter()
            .position(|h| Some(h.session.channel) == channel)
        {
            if self.holds(i, &msg) {
                self.held = Some(msg);
                return Vec::new();
            }
            let effects = self.hops.get_mut(i).map(|h| h.session.on_worker(msg));
            return self.after_hop(i, effects.unwrap_or_default());
        }
        if let Some(i) = self.hops.iter().position(|h| Some(h.tunnel) == channel) {
            return self.on_tunnel(i, msg);
        }
        if channel != Some(self.channel) {
            return Vec::new();
        }
        if self.holds(self.hops.len(), &msg) {
            self.held = Some(msg);
            return Vec::new();
        }
        self.on_own(msg)
    }

    fn on_own(&mut self, msg: Message) -> Vec<Effect> {
        if let Some(what) = self.out_of_turn(&msg) {
            let mut effects = vec![Effect::Show(format!(
                "\r\n{RED}The SSH worker asked for {what}. That is not how a login goes, so \
                 FeroSSH closed the connection.{RESET}\r\n"
            ))];
            effects.extend(self.close());
            effects.push(Effect::Ended);
            return effects;
        }
        match msg {
            Message::HostKey { key, .. } => {
                self.login.host_asked = true;
                self.on_host_key(key)
            }
            Message::HostCert {
                ca,
                key,
                principals,
                valid_after,
                valid_before,
                key_id,
                serial,
                ..
            } => {
                self.login.host_asked = true;
                let cert = HostCert {
                    ca,
                    key,
                    principals,
                    valid: (valid_after, valid_before),
                    key_id,
                    serial,
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                self.on_host_cert(&cert, now)
            }
            Message::Banner { text, .. } => vec![Effect::Show(sanitize(&text))],
            Message::Notice { text, .. } => vec![Effect::Show(dim(&sanitize(&text)))],
            Message::IdentitiesRequest { .. } => vec![Effect::Broker(Job::Identities {
                files: self.target.key_files.clone(),
            })],
            Message::SignRequest { key, data, .. } => {
                self.login.signatures += 1;
                self.on_sign_request(&key, data)
            }
            Message::Prompt {
                kind,
                name,
                instructions,
                fields,
                ..
            } => self.on_prompt(kind, &name, &instructions, fields),
            Message::Connected {
                kex,
                cipher,
                host_key,
                ..
            } => {
                self.state = State::Connected;
                self.login.done = true;
                let text = if self.jump {
                    format!(
                        "Logged in to jump host {} ({kex}, {cipher}, {host_key} host key).",
                        self.target.label()
                    )
                } else {
                    format!("Connected ({kex}, {cipher}, {host_key} host key).")
                };
                let mut effects = vec![Effect::Show(dim(&sanitize(&text)))];
                let old: Vec<&str> = [&kex, &cipher, &host_key]
                    .into_iter()
                    .map(String::as_str)
                    .filter(|n| crypto_policy::is_legacy(n))
                    .collect();
                self.legacy_in_use = !old.is_empty();
                if self.legacy_in_use {
                    let text = format!(
                        "Using legacy algorithms ({}), allowed by LegacyAlgorithms in this \
                         host's profile.",
                        old.join(", ")
                    );
                    effects.push(Effect::Show(format!(
                        "{AMBER}{}{RESET}\r\n",
                        sanitize(&text)
                    )));
                }
                if let Some(password) = self.typed_password.take() {
                    effects.push(Effect::StorePassword {
                        user: self.target.user.clone(),
                        host: self.target.hostname.clone(),
                        port: self.target.port,
                        password: Secrets(vec![password.to_string()]),
                    });
                }
                effects.push(Effect::Connected);
                effects
            }
            Message::Data { bytes, .. } => vec![Effect::Output(bytes)],
            Message::ExitStatus { code, .. } => {
                self.exit_code = Some(code);
                Vec::new()
            }
            Message::Lost { .. } => {
                self.lost = self.state == State::Connected;
                Vec::new()
            }
            Message::Disconnected { reason, .. } => {
                self.prompt = None;
                let was = self.state;
                let was_blocked = self.state == State::Blocked;
                self.state = if was_blocked {
                    State::Blocked
                } else {
                    State::Ended
                };
                if was_blocked {
                    // The warning stays on screen; nothing to add.
                    return vec![Effect::Ended];
                }
                let mut text = sanitize(&reason);
                if self.jump {
                    text = format!("Jump host {}: {text}", sanitize(&self.target.label()));
                }
                // A drop under an open shell, or a reconnect that did not
                // get as far as the host key (the network or the host is
                // still down): try again. A login that failed, a host key
                // that changed or a shell that exited stays ended.
                let dropped = self.lost && was == State::Connected && self.auto_reconnect;
                let unreachable =
                    self.retrying && was == State::Connecting && !self.login.host_asked;
                if dropped || unreachable {
                    return vec![
                        Effect::Show(format!("\r\n{DIM}[{text}]{RESET}\r\n")),
                        Effect::Reconnect,
                        Effect::Ended,
                    ];
                }
                if let Some(code) = self.exit_code {
                    text = format!("{text} The shell exited with code {code}.");
                }
                vec![
                    Effect::Show(format!(
                        "\r\n{DIM}[{text} Press Enter to reconnect.]{RESET}\r\n"
                    )),
                    Effect::Ended,
                ]
            }
            // UI-to-worker messages never come this way.
            _ => Vec::new(),
        }
    }

    fn verdict(&mut self, accept: bool) -> Effect {
        self.login.host_trusted = accept;
        Effect::Send(Message::HostKeyVerdict {
            channel: self.channel,
            accept,
        })
    }

    /// A host certificate the worker checked the CA signature of: trusted
    /// when known_hosts names its CA for this host, the hostname is one of
    /// its principals, it is valid now and nothing in it is revoked. There
    /// is no prompt either way, as in OpenSSH.
    fn on_host_cert(&mut self, cert: &HostCert, now: u64) -> Vec<Effect> {
        let (host, port) = (self.target.hostname.clone(), self.target.port);
        let (Some(ca), Some(key)) = (hostkey::describe(&cert.ca), hostkey::describe(&cert.key))
        else {
            return vec![self.verdict(false)];
        };
        let known = self.known();
        let problem = if known.is_revoked(&cert.ca) {
            Some(format!(
                "its CA key {} is marked as revoked",
                ca.fingerprint
            ))
        } else if known.is_revoked(&cert.key) {
            Some(format!(
                "its host key {} is marked as revoked",
                key.fingerprint
            ))
        } else if !known.is_authority_for(&host, port, &cert.ca) {
            Some(format!(
                "it is signed by CA {} {}, which no @cert-authority line in known_hosts \
                 trusts for {host}",
                ca.kind, ca.fingerprint
            ))
        } else if !cert
            .principals
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&host))
        {
            Some(format!(
                "it is not issued for {host} (it names {})",
                if cert.principals.is_empty() {
                    "no hosts".to_owned()
                } else {
                    cert.principals.join(", ")
                }
            ))
        } else if now < cert.valid.0 {
            Some("it is not valid yet".to_owned())
        } else if now >= cert.valid.1 {
            Some("it has expired".to_owned())
        } else {
            None
        };
        match problem {
            Some(why) => vec![
                Effect::Show(sanitize(&format!(
                    "{RED}The host certificate offered by {host} is not accepted: {why}. \
                     Not connecting.{RESET}\r\n"
                ))),
                self.verdict(false),
            ],
            None => vec![
                Effect::Show(dim(&sanitize(&format!(
                    "Host certificate \"{}\" (serial {}) for {} key {}, signed by trusted CA {}.",
                    cert.key_id, cert.serial, key.kind, key.fingerprint, ca.fingerprint
                )))),
                self.verdict(true),
            ],
        }
    }

    fn on_host_key(&mut self, key: Vec<u8>) -> Vec<Effect> {
        let (host, port) = (self.target.hostname.clone(), self.target.port);
        let Some(desc) = hostkey::describe(&key) else {
            return vec![self.verdict(false)];
        };
        match self.known().check(&host, port, &key) {
            Status::Trusted => vec![self.verdict(true)],
            Status::Revoked => vec![
                Effect::Show(format!(
                    "{RED}The {} host key {} offered by {host} is marked as revoked in \
                     known_hosts. Not connecting.{RESET}\r\n",
                    desc.kind, desc.fingerprint
                )),
                self.verdict(false),
            ],
            Status::Changed { lines } => {
                self.state = State::Blocked;
                self.changed_key = Some(key);
                let lines: Vec<String> = lines.iter().map(usize::to_string).collect();
                let text = format!(
                    "\x1b[0m\x1b[2J\x1b[H\x1b[1;37;41m{:^60}\x1b[0m\r\n\r\n\
                     {BOLD}The host key of {host} has changed.{RESET}\r\n\r\n\
                     Someone could be intercepting the connection (a man-in-the-middle \
                     attack), or the server was reinstalled or its key replaced.\r\n\r\n\
                     The key it offers now:\r\n    {} {} {}\r\n\r\n\
                     known_hosts ({}) holds a different key on line {}.\r\n\r\n\
                     FeroSSH did not connect. If you know why the key changed, check the \
                     new fingerprint with the server's administrator, then use \
                     Session > Replace host key.\r\n",
                    "WARNING: HOST KEY CHANGED",
                    desc.kind,
                    desc.bits,
                    desc.fingerprint,
                    self.known_hosts.display(),
                    lines.join(", "),
                );
                vec![Effect::Show(text), self.verdict(false)]
            }
            Status::Unknown { other_types } => {
                let mut text = format!(
                    "The authenticity of host {}{BOLD}{host}{RESET} can't be established.\r\n",
                    if port == 22 {
                        String::new()
                    } else {
                        format!("port {port} on ")
                    }
                );
                if !other_types.is_empty() {
                    text.push_str(&format!(
                        "{RED}known_hosts holds a {} key for this host, but the server now \
                         offers a {} key instead.{RESET}\r\n",
                        other_types.join(", "),
                        desc.kind
                    ));
                }
                text.push_str(&format!(
                    "{} key fingerprint is {BOLD}{}{RESET}\r\n{}\r\n\
                     Type {BOLD}yes{RESET} to trust this key and connect: ",
                    desc.kind,
                    desc.fingerprint,
                    desc.randomart.replace('\n', "\r\n"),
                ));
                self.prompt = Some(LocalPrompt {
                    echo: true,
                    text: Zeroizing::new(String::new()),
                    purpose: Purpose::TrustHostKey { key },
                });
                vec![Effect::Show(text)]
            }
        }
    }

    /// After the user confirmed twice: store the key that blocked the
    /// connection, this host's or a jump host's. The caller then
    /// reconnects.
    pub fn replace_host_key(&mut self) -> std::io::Result<()> {
        if let Some(h) = self
            .hops
            .iter_mut()
            .find(|h| h.session.state == State::Blocked)
        {
            return h.session.replace_host_key();
        }
        let Some(key) = self.changed_key.take() else {
            return Ok(());
        };
        known_hosts::replace(
            &self.known_hosts,
            &self.target.hostname,
            self.target.port,
            &key,
        )
    }

    pub fn on_broker(&mut self, reply: Reply) -> Vec<Effect> {
        if self.state == State::Ended {
            return Vec::new();
        }
        if let Some(i) = self.active_hop() {
            let effects = self.hops.get_mut(i).map(|h| h.session.on_broker(reply));
            return self.after_hop(i, effects.unwrap_or_default());
        }
        match reply {
            Reply::Identities { keys, notes } => {
                let mut effects: Vec<Effect> = notes
                    .iter()
                    .map(|n| Effect::Show(dim(&sanitize(n))))
                    .collect();
                let blobs = keys.iter().map(|k| k.public.clone()).collect();
                self.identities = keys;
                effects.push(Effect::Send(Message::Identities {
                    channel: self.channel,
                    keys: blobs,
                }));
                effects
            }
            Reply::Signed(Ok(signature)) => {
                self.signing = None;
                vec![self.signature(signature)]
            }
            Reply::Signed(Err(SignError::NeedsPassphrase)) => self.ask_passphrase(false),
            Reply::Signed(Err(SignError::WrongPassphrase)) => self.ask_passphrase(true),
            Reply::Signed(Err(SignError::Failed(e))) => {
                self.signing = None;
                vec![
                    Effect::Show(dim(&format!("Signing failed: {}", sanitize(&e)))),
                    self.signature(Vec::new()),
                ]
            }
        }
    }

    fn signature(&self, signature: Vec<u8>) -> Effect {
        Effect::Send(Message::Signature {
            channel: self.channel,
            signature,
        })
    }

    fn on_sign_request(&mut self, key: &[u8], data: Vec<u8>) -> Vec<Effect> {
        let Some(identity) = self.identities.iter().find(|i| i.public == key).cloned() else {
            return vec![self.signature(Vec::new())];
        };
        let flags = match userauth::check(&data, &self.target.user, key) {
            Ok(target) => target.flags,
            Err(why) => {
                return vec![
                    Effect::Show(format!(
                        "{RED}The SSH worker asked for a signature that is not a login to \
                         {} ({why}); refused.{RESET}\r\n",
                        self.target.hostname
                    )),
                    self.signature(Vec::new()),
                ];
            }
        };
        // Name the host and key, so a surprise request is visible.
        let what = if userauth::is_certificate(key) {
            hostkey::describe_certificate(key)
                .map(|d| format!("{} certificate for key {}", d.kind, d.fingerprint))
        } else {
            hostkey::describe(key).map(|d| format!("{} key {}", d.kind, d.fingerprint))
        }
        .unwrap_or_else(|| "key".into());
        let from = match &identity.source {
            Source::Agent(a) if !identity.comment.is_empty() => {
                format!("{}, {}", a.label(), identity.comment)
            }
            _ => identity.origin(),
        };
        let note = dim(&sanitize(&format!(
            "Signing in to {} with {what} ({from}).",
            self.target.hostname
        )));
        self.passphrase_tries = 0;
        self.signing = Some((identity.clone(), data.clone(), flags));
        let mut effects = vec![Effect::Show(note)];
        if userauth::key_type(key).is_some_and(|t| t.starts_with("sk-")) {
            effects.push(Effect::Show(dim(
                "Touch your security key, or confirm with Windows Hello.",
            )));
        }
        effects.push(Effect::Broker(Job::Sign {
            identity,
            data,
            flags,
            passphrase: None,
        }));
        effects
    }

    fn ask_passphrase(&mut self, wrong: bool) -> Vec<Effect> {
        let Some((identity, data, flags)) = self.signing.take() else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        if wrong {
            self.passphrase_tries += 1;
            if self.passphrase_tries >= PASSPHRASE_TRIES {
                effects.push(Effect::Show(dim(
                    "Too many wrong passphrases; skipping the key.",
                )));
                effects.push(self.signature(Vec::new()));
                return effects;
            }
            effects.push(Effect::Show(dim("Wrong passphrase.")));
        }
        effects.push(Effect::Show(format!(
            "Passphrase for key {} (empty skips it): ",
            sanitize(&identity.origin())
        )));
        self.prompt = Some(LocalPrompt {
            echo: false,
            text: Zeroizing::new(String::new()),
            purpose: Purpose::Passphrase {
                identity,
                data,
                flags,
            },
        });
        effects
    }

    fn on_prompt(
        &mut self,
        kind: PromptKind,
        name: &str,
        instructions: &str,
        fields: Vec<PromptField>,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.prompt_kind = Some(kind);
        let asks_password = is_password_prompt(kind, &fields);
        if asks_password && self.source_used {
            self.source_used = false;
            effects.push(Effect::Show(dim(
                "The password from the password source did not work; type it.",
            )));
        }
        if kind == PromptKind::KeyboardInteractive {
            for text in [name, instructions] {
                let text = sanitize(text.trim_end());
                if !text.is_empty() {
                    effects.push(Effect::Show(format!("{text}\r\n")));
                }
            }
        }
        if asks_password && self.password_source && !self.source_tried {
            self.source_tried = true;
            let mut fields = fields;
            let field = fields.remove(0);
            self.fetching = Some((field, fields));
            effects.push(Effect::FetchPassword);
            return effects;
        }
        effects.extend(self.next_field(fields, Secrets::default()));
        effects
    }

    /// Whether the session waits for its password source.
    pub fn fetching_password(&self) -> bool {
        match self.active_hop().and_then(|i| self.hops.get(i)) {
            Some(h) => h.session.fetching_password(),
            None => self.fetching.is_some(),
        }
    }

    /// The password source answered: `Ok` sends the password, `Err` says
    /// why not and asks for it at the prompt instead. `from` names the
    /// source for the note.
    pub fn password_fetched(
        &mut self,
        result: Result<Zeroizing<String>, String>,
        from: &str,
    ) -> Vec<Effect> {
        if let Some(i) = self.active_hop() {
            let effects = self
                .hops
                .get_mut(i)
                .map(|h| h.session.password_fetched(result, from));
            return self.after_hop(i, effects.unwrap_or_default());
        }
        let Some((field, rest)) = self.fetching.take() else {
            return Vec::new();
        };
        match result {
            Ok(password) => {
                self.source_used = true;
                let mut effects = vec![Effect::Show(dim(&format!(
                    "Password from {}.",
                    sanitize(from)
                )))];
                let answers = Secrets(vec![password.to_string()]);
                effects.extend(self.next_field(rest, answers));
                effects
            }
            Err(why) => {
                let mut effects = vec![Effect::Show(dim(&sanitize(&why)))];
                let mut fields = vec![field];
                fields.extend(rest);
                effects.extend(self.next_field(fields, Secrets::default()));
                effects
            }
        }
    }

    /// Asks the next server question, or sends the answers when none is
    /// left.
    fn next_field(&mut self, mut fields: Vec<PromptField>, answers: Secrets) -> Vec<Effect> {
        if fields.is_empty() {
            return vec![Effect::Send(Message::Answers {
                channel: self.channel,
                answers,
            })];
        }
        let password = answers.0.is_empty()
            && self
                .prompt_kind
                .is_some_and(|k| is_password_prompt(k, &fields));
        let field = fields.remove(0);
        let echo = field.echo;
        self.prompt = Some(LocalPrompt {
            echo,
            text: Zeroizing::new(String::new()),
            purpose: Purpose::Answer {
                fields,
                answers,
                password,
            },
        });
        vec![Effect::Show(sanitize(&field.text))]
    }

    /// Text pasted while a prompt is open: line breaks and control
    /// characters are dropped.
    pub fn prompt_paste(&mut self, text: &str) -> Vec<Effect> {
        if let Some(i) = self.active_hop() {
            let effects = self.hops.get_mut(i).map(|h| h.session.prompt_paste(text));
            return self.after_hop(i, effects.unwrap_or_default());
        }
        let Some(p) = &mut self.prompt else {
            return Vec::new();
        };
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        p.text.push_str(&clean);
        if p.echo {
            vec![Effect::Show(clean)]
        } else {
            Vec::new()
        }
    }

    /// Keys typed while a prompt is open, as the terminal encoded them.
    pub fn prompt_input(&mut self, bytes: &[u8]) -> Vec<Effect> {
        if let Some(i) = self.active_hop() {
            let effects = self.hops.get_mut(i).map(|h| h.session.prompt_input(bytes));
            return self.after_hop(i, effects.unwrap_or_default());
        }
        let Some(p) = &mut self.prompt else {
            return Vec::new();
        };
        match bytes {
            b"\r" | b"\n" | b"\r\n" => {
                let Some(prompt) = self.prompt.take() else {
                    return Vec::new();
                };
                let mut effects = vec![Effect::Show("\r\n".into())];
                effects.extend(self.submit(prompt));
                effects
            }
            // Ctrl+C or Escape: give up on the connection.
            [0x03] | [0x1b] => {
                let mut effects = vec![Effect::Show(dim("^C"))];
                effects.extend(self.close());
                effects.push(Effect::Show(dim("[Cancelled. Press Enter to reconnect.]")));
                effects.push(Effect::Ended);
                effects
            }
            [0x7f] | [0x08] => {
                if p.text.pop().is_some() && p.echo {
                    vec![Effect::Show("\x08 \x08".into())]
                } else {
                    Vec::new()
                }
            }
            // Ctrl+U clears the line.
            [0x15] => {
                let n = p.text.chars().count();
                p.text.clear();
                if p.echo {
                    vec![Effect::Show("\x08 \x08".repeat(n))]
                } else {
                    Vec::new()
                }
            }
            // Arrows and other keys that send sequences.
            [0x1b, ..] => Vec::new(),
            _ => {
                let typed: String = String::from_utf8_lossy(bytes)
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect();
                p.text.push_str(&typed);
                if p.echo {
                    vec![Effect::Show(typed)]
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn submit(&mut self, prompt: LocalPrompt) -> Vec<Effect> {
        let LocalPrompt { text, purpose, .. } = prompt;
        match purpose {
            Purpose::TrustHostKey { key } => {
                if text.trim() != "yes" {
                    return vec![
                        Effect::Show(dim("Host key not trusted.")),
                        self.verdict(false),
                    ];
                }
                let (host, port) = (&self.target.hostname, self.target.port);
                let mut effects = Vec::new();
                match known_hosts::add(&self.known_hosts, host, port, &key) {
                    Ok(()) => effects.push(Effect::Show(dim(&format!(
                        "Added {host} to {}.",
                        self.known_hosts.display()
                    )))),
                    Err(e) => effects.push(Effect::Show(dim(&format!(
                        "Could not save the key to {} ({e}); trusting it for this \
                         connection only.",
                        self.known_hosts.display()
                    )))),
                }
                effects.push(self.verdict(true));
                effects
            }
            Purpose::Answer {
                fields,
                mut answers,
                password,
            } => {
                if password && self.remember_password && !text.is_empty() {
                    self.typed_password = Some(text.clone());
                }
                answers.0.push(text.to_string());
                self.next_field(fields, answers)
            }
            Purpose::Passphrase {
                identity,
                data,
                flags,
            } => {
                if text.is_empty() {
                    return vec![self.signature(Vec::new())];
                }
                self.signing = Some((identity.clone(), data.clone(), flags));
                vec![Effect::Broker(Job::Sign {
                    identity,
                    data,
                    flags,
                    passphrase: Some(text),
                })]
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn env(dir: &std::path::Path) -> Env {
        Env {
            home: dir.to_owned(),
            local_user: "me".into(),
            config: "Host prod\n  HostName prod.example.org\n  User admin\n  Port 2200\n".into(),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ferossh-ui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ed25519_blob() -> Vec<u8> {
        russh::keys::PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINX+3J1AiintwNo3NPzRD4dq1fDjyMII0jItKqKOYfxb",
        )
        .unwrap()
        .to_bytes()
        .unwrap()
    }

    fn shown(effects: &[Effect]) -> String {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Show(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    fn sent(effects: &[Effect]) -> Vec<&Message> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    /// Starts `s` and trusts the host key, as a login does first.
    fn trusted(s: &mut SshSession) {
        s.start(80, 24);
        let mut fx = s.on_worker(Message::HostKey {
            channel: s.channel,
            key: ed25519_blob(),
        });
        if s.prompt_active() {
            s.prompt_input(b"yes");
            fx = s.prompt_input(b"\r");
        }
        assert!(
            matches!(
                sent(&fx)[..],
                [Message::HostKeyVerdict { accept: true, .. }]
            ),
            "{fx:?}"
        );
    }

    fn connected(s: &mut SshSession) {
        trusted(s);
        s.on_worker(Message::Connected {
            channel: s.channel,
            kex: "mlkem768x25519-sha256".into(),
            cipher: "aes256-gcm@openssh.com".into(),
            host_key: "ssh-ed25519".into(),
        });
        assert_eq!(s.state(), State::Connected);
    }

    fn reconnects(fx: &[Effect]) -> bool {
        fx.iter().any(|e| matches!(e, Effect::Reconnect))
    }

    fn gone(s: &mut SshSession, reason: &str) -> Vec<Effect> {
        s.on_worker(Message::Disconnected {
            channel: s.channel,
            reason: reason.into(),
        })
    }

    #[test]
    fn a_dropped_connection_reconnects_and_an_exit_does_not() {
        let dir = temp("reconnect");
        let e = env(&dir);
        let session = |auto: bool| {
            let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
            s.auto_reconnect = auto;
            s
        };

        // Lost under the open shell: reconnect, and say nothing of Enter.
        let mut s = session(true);
        connected(&mut s);
        s.on_worker(Message::Lost { channel: 1 });
        let fx = gone(&mut s, "Keepalive timeout.");
        assert!(reconnects(&fx));
        assert!(matches!(fx[..], [_, Effect::Reconnect, Effect::Ended]));
        assert!(!shown(&fx).contains("Press Enter"));

        // Turned off: the old "Press Enter".
        let mut s = session(false);
        connected(&mut s);
        s.on_worker(Message::Lost { channel: 1 });
        let fx = gone(&mut s, "Keepalive timeout.");
        assert!(!reconnects(&fx));
        assert!(shown(&fx).contains("Press Enter to reconnect"));

        // The shell exited (exit, logout): no Lost from the worker.
        let mut s = session(true);
        connected(&mut s);
        s.on_worker(Message::ExitStatus {
            channel: 1,
            code: 0,
        });
        let fx = gone(&mut s, "Connection closed.");
        assert!(!reconnects(&fx));
        assert!(shown(&fx).contains("exited with code 0"));

        // A Lost before the shell opened counts for nothing.
        let mut s = session(true);
        trusted(&mut s);
        s.on_worker(Message::Lost { channel: 1 });
        assert!(!reconnects(&gone(&mut s, "Authentication failed.")));

        // Closed from this side (Disconnect, closing the tab).
        let mut s = session(true);
        connected(&mut s);
        s.close();
        s.on_worker(Message::Lost { channel: 1 });
        assert!(!reconnects(&gone(&mut s, "Cancelled.")));
    }

    #[test]
    fn a_reconnect_retries_only_while_the_host_is_unreachable() {
        let dir = temp("retry");
        let e = env(&dir);
        let session = || {
            let mut s = SshSession::new(2, Target::resolve("host", &e).unwrap(), e.known_hosts());
            s.auto_reconnect = true;
            s.retrying = true;
            s
        };
        // Refused or timed out before the host key: try again.
        let mut s = session();
        s.start(80, 24);
        let fx = gone(&mut s, "Could not connect to host: refused.");
        assert!(reconnects(&fx));
        // The host answered, but the login failed: stop.
        let mut s = session();
        trusted(&mut s);
        assert!(!reconnects(&gone(&mut s, "Permission denied.")));
        // A first connect (not a reconnect) that fails: stop.
        let mut s = session();
        s.retrying = false;
        s.start(80, 24);
        assert!(!reconnects(&gone(&mut s, "Timed out connecting to host.")));
    }

    #[test]
    fn resolves_targets() {
        let dir = temp("resolve");
        let e = env(&dir);
        let t = Target::resolve("prod", &e).unwrap();
        assert_eq!(
            (t.hostname.as_str(), t.user.as_str(), t.port),
            ("prod.example.org", "admin", 2200)
        );
        assert_eq!(t.key_files.len(), 4);
        assert!(t.key_files[3].path.ends_with("id_ed25519_sk"));
        assert!(!t.key_files[0].explicit);
        let t = Target::resolve("root@other:23", &e).unwrap();
        assert_eq!(t.label(), "root@other:23");
        assert!(Target::resolve("bad host", &e).is_none());
    }

    const CA: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHBotJ7Eq4j3xY4L22Rdv6r113TCdKlCUcMe9icH6KGY";

    fn ca_blob() -> Vec<u8> {
        russh::keys::PublicKey::from_openssh(CA)
            .unwrap()
            .to_bytes()
            .unwrap()
    }

    #[test]
    fn host_certificates_need_a_trusted_ca_principal_and_validity() {
        let dir = temp("cert");
        let e = env(&dir);
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();
        let known = |text: &str| std::fs::write(e.known_hosts(), text).unwrap();
        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        // No CA: no certificates asked for.
        let fx = s.start(80, 24);
        assert!(
            matches!(&sent(&fx)[..], [Message::Connect { host_key_algs, .. }] if host_key_algs.is_empty())
        );
        known(&format!("@cert-authority *.lab,host {CA}\n"));
        let fx = s.start(80, 24);
        assert!(
            matches!(&sent(&fx)[..], [Message::Connect { host_key_algs, .. }]
            if host_key_algs == &[crypto_policy::CERT_AUTHORITY])
        );

        let cert = |principals: &[&str], valid: (u64, u64)| HostCert {
            ca: ca_blob(),
            key: ed25519_blob(),
            principals: principals.iter().map(|p| (*p).to_owned()).collect(),
            valid,
            key_id: "host-2026".into(),
            serial: 9,
        };
        let verdict = |fx: &[Effect]| match sent(fx)[..] {
            [Message::HostKeyVerdict { accept, .. }] => *accept,
            _ => panic!("no verdict"),
        };
        let fx = s.on_host_cert(&cert(&["HOST"], (100, 200)), 150);
        assert!(verdict(&fx), "{}", shown(&fx));
        assert!(shown(&fx).contains("\"host-2026\" (serial 9)"));
        for (c, now, why) in [
            (
                cert(&["other"], (100, 200)),
                150,
                "not issued for host (it names other)",
            ),
            (cert(&[], (100, 200)), 150, "names no hosts"),
            (cert(&["host"], (100, 200)), 99, "not valid yet"),
            (cert(&["host"], (100, 200)), 200, "expired"),
        ] {
            let fx = s.on_host_cert(&c, now);
            assert!(!verdict(&fx));
            assert!(shown(&fx).contains(why), "{}", shown(&fx));
        }
        // Not this host's CA, or revoked.
        known(&format!("@cert-authority *.lab {CA}\n"));
        let fx = s.on_host_cert(&cert(&["host"], (100, 200)), 150);
        assert!(!verdict(&fx));
        assert!(
            shown(&fx).contains("no @cert-authority line"),
            "{}",
            shown(&fx)
        );
        known(&format!("@cert-authority host {CA}\n@revoked * {CA}\n"));
        let fx = s.on_host_cert(&cert(&["host"], (100, 200)), 150);
        assert!(!verdict(&fx));
        assert!(shown(&fx).contains("CA key SHA256:"), "{}", shown(&fx));
        // A CA line is no plain host key: known_hosts stays as it was.
        assert_eq!(
            KnownHosts::load(&e.known_hosts())
                .unwrap()
                .check("host", 22, &ca_blob()),
            Status::Revoked
        );
    }

    #[test]
    fn legacy_algorithms_are_asked_for_and_flagged() {
        let dir = temp("legacy");
        let e = env(&dir);
        let mut s = SshSession::new(1, Target::resolve("prod", &e).unwrap(), e.known_hosts());
        let fx = s.start(80, 24);
        assert!(matches!(
            sent(&fx)[..],
            [Message::Connect { legacy: false, .. }]
        ));
        s.legacy = true;
        let fx = s.start(80, 24);
        assert!(matches!(
            sent(&fx)[..],
            [Message::Connect { legacy: true, .. }]
        ));

        let connected = |kex: &str, cipher: &str| Message::Connected {
            channel: 1,
            kex: kex.into(),
            cipher: cipher.into(),
            host_key: "ssh-ed25519".into(),
        };
        // Offered but not needed: no badge.
        let fx = s.on_worker(connected("curve25519-sha256", "aes256-gcm@openssh.com"));
        assert!(!s.legacy_in_use());
        assert!(!shown(&fx).contains("legacy"));
        let fx = s.on_worker(connected("ecdh-sha2-nistp256", "aes256-ctr"));
        assert!(s.legacy_in_use());
        assert!(
            shown(&fx).contains("Using legacy algorithms (ecdh-sha2-nistp256, aes256-ctr)"),
            "{}",
            shown(&fx)
        );
    }

    #[test]
    fn second_tab_shares_a_logged_in_connection() {
        let dir = temp("share");
        let e = env(&dir);
        let mut first = SshSession::new(1, Target::resolve("prod", &e).unwrap(), e.known_hosts());
        first.start(80, 24);
        // Same host, port and user, under another spelling.
        let same = Target::resolve("admin@PROD.example.org:2200", &e).unwrap();
        assert!(!first.can_share(&same), "not logged in yet");
        first.on_worker(Message::Connected {
            channel: 1,
            kex: "k".into(),
            cipher: "c".into(),
            host_key: "h".into(),
        });
        assert!(first.can_share(&same));
        assert!(!first.can_share(&Target::resolve("root@prod.example.org:2200", &e).unwrap()));
        assert!(!first.can_share(&Target::resolve("admin@prod.example.org", &e).unwrap()));

        let mut second = SshSession::new(2, same, e.known_hosts());
        let fx = second.start_on(first.connection, 100, 30);
        assert_eq!(
            sent(&fx),
            vec![&Message::OpenChannel {
                channel: 2,
                connection: 1,
                cols: 100,
                rows: 30,
                x11: None
            }]
        );
        assert_eq!(second.connection, 1);
        assert!(shown(&fx).contains("new shell"));
    }

    #[test]
    fn new_host_key_needs_yes_and_is_saved() {
        let dir = temp("newkey");
        let e = env(&dir);
        let target = Target::resolve("host", &e).unwrap();
        let mut s = SshSession::new(1, target, e.known_hosts());
        let fx = s.start(80, 24);
        assert!(matches!(sent(&fx)[..], [Message::Connect { .. }]));

        let fx = s.on_worker(Message::HostKey {
            channel: 1,
            key: ed25519_blob(),
        });
        assert!(sent(&fx).is_empty());
        assert!(shown(&fx).contains("SHA256:vWYvpuNm8Qqc7Z0RA0GFQtnBKGWwNkzadS6MX5uwbZ4"));
        assert!(s.prompt_active());
        s.prompt_input(b"y");
        s.prompt_input(b"e");
        s.prompt_input(b"s");
        let fx = s.prompt_input(b"\r");
        assert_eq!(
            sent(&fx),
            vec![&Message::HostKeyVerdict {
                channel: 1,
                accept: true
            }]
        );
        let kh = KnownHosts::load(&e.known_hosts()).unwrap();
        assert_eq!(kh.check("host", 22, &ed25519_blob()), Status::Trusted);

        // The next connection trusts it without asking.
        let mut s2 = SshSession::new(2, Target::resolve("host", &e).unwrap(), e.known_hosts());
        s2.start(80, 24);
        let fx = s2.on_worker(Message::HostKey {
            channel: 2,
            key: ed25519_blob(),
        });
        assert_eq!(
            sent(&fx),
            vec![&Message::HostKeyVerdict {
                channel: 2,
                accept: true
            }]
        );
    }

    #[test]
    fn anything_but_yes_rejects() {
        let dir = temp("no");
        let e = env(&dir);
        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        s.start(80, 24);
        s.on_worker(Message::HostKey {
            channel: 1,
            key: ed25519_blob(),
        });
        s.prompt_input(b"y");
        let fx = s.prompt_input(b"\r");
        assert_eq!(
            sent(&fx),
            vec![&Message::HostKeyVerdict {
                channel: 1,
                accept: false
            }]
        );
        assert!(!e.known_hosts().exists());
    }

    #[test]
    fn changed_key_blocks_and_can_be_replaced() {
        let dir = temp("changed");
        let e = env(&dir);
        let mut other = vec![0, 0, 0, 11];
        other.extend_from_slice(b"ssh-ed25519");
        other.extend_from_slice(&[0, 0, 0, 32]);
        other.extend_from_slice(&[7; 32]);
        known_hosts::add(&e.known_hosts(), "host", 22, &other).unwrap();

        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        s.start(80, 24);
        let fx = s.on_worker(Message::HostKey {
            channel: 1,
            key: ed25519_blob(),
        });
        assert!(shown(&fx).contains("HOST KEY CHANGED"));
        assert!(!shown(&fx).to_lowercase().contains("type yes"));
        assert_eq!(
            sent(&fx),
            vec![&Message::HostKeyVerdict {
                channel: 1,
                accept: false
            }]
        );
        assert_eq!(s.state(), State::Blocked);
        assert!(!s.prompt_active());

        s.replace_host_key().unwrap();
        let kh = KnownHosts::load(&e.known_hosts()).unwrap();
        assert_eq!(kh.check("host", 22, &ed25519_blob()), Status::Trusted);
        assert!(matches!(
            kh.check("host", 22, &other),
            Status::Changed { .. }
        ));
    }

    /// A session to "inner" through "bastion", then "edge" (as
    /// `ProxyJump bastion,edge`), with channel ids 2 to 5 for the hops.
    fn chain(e: &Env) -> SshSession {
        let mut next = 1;
        SshSession::new(
            1,
            Target::resolve_route("inner", e).unwrap(),
            e.known_hosts(),
        )
        .with_route(&mut || {
            next += 1;
            next
        })
    }

    fn jump_env(dir: &std::path::Path) -> Env {
        Env {
            config: "Host inner\n  HostName 10.0.0.5\n  ProxyJump bastion,edge\n\
                     Host bastion\n  HostName bastion.example.org\n  User jump\n  Port 2222\n\
                     Host edge\n  ProxyJump nested\n"
                .into(),
            ..env(dir)
        }
    }

    /// Lets hop login `channel` in: host key trusted, logged in.
    fn hop_in(s: &mut SshSession, channel: ChannelId) -> Vec<Effect> {
        s.on_worker(Message::HostKey {
            channel,
            key: ed25519_blob(),
        });
        s.prompt_input(b"yes");
        let fx = s.prompt_input(b"\r");
        assert!(sent(&fx).contains(&&Message::HostKeyVerdict {
            channel,
            accept: true
        }));
        s.on_worker(Message::Connected {
            channel,
            kex: "k".into(),
            cipher: "c".into(),
            host_key: "ssh-ed25519".into(),
        })
    }

    #[test]
    fn resolves_jump_hosts_in_order_without_their_later_ones_own() {
        let dir = temp("route");
        let e = jump_env(&dir);
        let t = Target::resolve_route("inner", &e).unwrap();
        let hops: Vec<String> = t.jumps.iter().map(Target::label).collect();
        // Only the first jump host's own ProxyJump applies, as in OpenSSH.
        assert_eq!(hops, ["jump@bastion:2222", "me@edge"]);
        assert!(t.jumps.iter().all(|j| j.jumps.is_empty()));
        assert_eq!(t.jumps[0].hostname, "bastion.example.org");

        let e = Env {
            config: "Host a\n  ProxyJump b\nHost b\n  ProxyJump a\n".into(),
            ..env(&dir)
        };
        let err = Target::resolve_route("a", &e).unwrap_err();
        assert!(err.contains("loop"), "{err}");
        let e = Env {
            config: "Host a\n  ProxyCommand nc -X connect -x proxy:3128 %h %p\n".into(),
            ..env(&dir)
        };
        let t = Target::resolve_route("a", &e).unwrap();
        let c = t.proxy_command.unwrap();
        assert_eq!(c.program, "nc");
        assert_eq!(c.args, ["-X", "connect", "-x", "proxy:3128", "a", "22"]);
        assert_eq!(c.line, "nc -X connect -x proxy:3128 %h %p");
    }

    #[test]
    fn jump_hosts_log_in_one_after_another_each_in_its_own_worker() {
        let dir = temp("chain");
        let e = jump_env(&dir);
        let mut s = chain(&e);
        assert_eq!(s.hop_connections(), [2, 4]);
        let fx = s.start(100, 30);
        // The first jump host first: no shell, no tunnel, its own worker.
        let [
            Message::Connect {
                channel: 2,
                host,
                tunnel: None,
                shell: false,
                ..
            },
        ] = &sent(&fx)[..]
        else {
            panic!("not what the login does next");
        };
        assert_eq!(host, "bastion.example.org");
        assert_eq!(s.worker_for(Some(2)), Some(2));
        assert_eq!(s.worker_for(Some(1)), Some(1));
        assert_eq!(s.worker_for(Some(99)), None);
        assert!(s.owns(3) && !s.owns(6));

        // Bastion in: its worker opens a stream to edge, which logs in
        // over it in a worker of its own.
        let fx = hop_in(&mut s, 2);
        assert!(shown(&fx).contains("Logged in to jump host jump@bastion:2222"));
        assert!(fx.iter().any(|f| matches!(
            f,
            Effect::Tunnel {
                from: 2,
                to: 4,
                channel: 3
            }
        )));
        let msgs = sent(&fx);
        assert!(
            matches!(
                msgs[..],
                [
                    Message::ForwardOpen {
                        channel: 3,
                        connection: 2,
                        target: Some((h, 22)),
                        keep: true,
                        ..
                    },
                    Message::Connect {
                        channel: 4,
                        tunnel: Some(3),
                        shell: false,
                        ..
                    }
                ] if h == "edge"
            ),
            "{msgs:?}"
        );
        assert_eq!(s.worker_for(Some(3)), Some(2));

        // Edge in: the target logs in over edge's stream, with a shell
        // of the pane's size.
        let fx = hop_in(&mut s, 4);
        let msgs = sent(&fx);
        assert!(
            matches!(
                msgs[..],
                [
                    Message::ForwardOpen { channel: 5, connection: 4, target: Some((h, 22)), .. },
                    Message::Connect { channel: 1, tunnel: Some(5), shell: true, cols: 100, rows: 30, .. }
                ] if h == "10.0.0.5"
            ),
            "{msgs:?}"
        );
        assert_eq!(s.state(), State::Connecting);
        assert!(!fx.iter().any(|f| matches!(f, Effect::Connected)));

        // The target's own login, then the shell.
        hop_in(&mut s, 1);
        assert_eq!(s.state(), State::Connected);
        // Closing closes every login on the way.
        let closes: Vec<ChannelId> = sent(&s.close())
            .iter()
            .filter_map(|m| match m {
                Message::Close { channel } => Some(*channel),
                _ => None,
            })
            .collect();
        assert_eq!(closes, [2, 4, 1]);
    }

    #[test]
    fn a_failed_jump_host_ends_the_connection_and_says_why() {
        let dir = temp("chain-fail");
        let e = jump_env(&dir);
        let mut s = chain(&e);
        s.start(80, 24);
        hop_in(&mut s, 2);
        // Bastion cannot reach edge.
        let fx = s.on_worker(Message::Disconnected {
            channel: 3,
            reason: "edge:22: the server could not connect (ConnectFailed).".into(),
        });
        assert!(shown(&fx).contains("The jump host jump@bastion:2222 could not reach edge:22"));
        assert_eq!(s.state(), State::Connecting);
        // Edge's login then fails, and with it the connection.
        let fx = s.on_worker(Message::Disconnected {
            channel: 4,
            reason: "The server closed the connection without saying why.".into(),
        });
        assert!(shown(&fx).contains("Jump host me@edge:"));
        assert!(matches!(fx.last(), Some(Effect::Ended)));
        assert!(sent(&fx).contains(&&Message::Close { channel: 2 }));
        assert_eq!(s.state(), State::Ended);

        // The two workers may say it the other way round: the reason
        // still shows first.
        let dir = temp("chain-fail-order");
        let e = jump_env(&dir);
        let mut s = chain(&e);
        s.start(80, 24);
        hop_in(&mut s, 2);
        let fx = s.on_worker(Message::Disconnected {
            channel: 4,
            reason: String::new(),
        });
        assert!(fx.is_empty());
        assert_eq!(s.state(), State::Connecting);
        let fx = s.on_worker(Message::Disconnected {
            channel: 3,
            reason: "edge:22: the server could not connect (ConnectFailed).".into(),
        });
        let text = shown(&fx);
        let why = text.find("could not reach edge:22").unwrap();
        assert!(why < text.find("Jump host me@edge").unwrap(), "{text}");
        assert!(matches!(fx.last(), Some(Effect::Ended)));
        assert_eq!(s.state(), State::Ended);
    }

    #[test]
    fn a_changed_jump_host_key_blocks_and_can_be_replaced() {
        let dir = temp("chain-changed");
        let e = jump_env(&dir);
        let mut other = vec![0, 0, 0, 11];
        other.extend_from_slice(b"ssh-ed25519");
        other.extend_from_slice(&[0, 0, 0, 32]);
        other.extend_from_slice(&[7; 32]);
        known_hosts::add(&e.known_hosts(), "bastion.example.org", 2222, &other).unwrap();
        let mut s = chain(&e);
        s.start(80, 24);
        let fx = s.on_worker(Message::HostKey {
            channel: 2,
            key: ed25519_blob(),
        });
        assert!(shown(&fx).contains("HOST KEY CHANGED"));
        let fx = s.on_worker(Message::Disconnected {
            channel: 2,
            reason: "Host key not accepted.".into(),
        });
        assert!(matches!(fx.last(), Some(Effect::Ended)));
        assert_eq!(s.state(), State::Blocked);
        assert_eq!(s.blocked_host(), "bastion.example.org");
        assert_eq!(s.changed_key(), Some(&ed25519_blob()[..]));
        s.replace_host_key().unwrap();
        let kh = KnownHosts::load(&e.known_hosts()).unwrap();
        assert_eq!(
            kh.check("bastion.example.org", 2222, &ed25519_blob()),
            Status::Trusted
        );
    }

    #[test]
    fn the_target_may_not_speak_before_its_jump_hosts_are_in() {
        let dir = temp("chain-turns");
        let e = jump_env(&dir);
        let mut s = chain(&e);
        s.start(80, 24);
        let fx = s.on_worker(Message::HostKey {
            channel: 1,
            key: ed25519_blob(),
        });
        assert!(shown(&fx).contains("not how a login goes"));
        assert!(sent(&fx).contains(&&Message::Close { channel: 2 }));
        assert_eq!(s.state(), State::Ended);
    }

    #[test]
    fn a_route_reconnects_after_a_drop_and_while_a_jump_host_is_down() {
        let dir = temp("chain-reconnect");
        let e = jump_env(&dir);
        // Logged in through both jump hosts, then the route breaks.
        let mut s = chain(&e);
        s.auto_reconnect = true;
        s.start(80, 24);
        hop_in(&mut s, 2);
        hop_in(&mut s, 4);
        hop_in(&mut s, 1);
        assert_eq!(s.state(), State::Connected);
        s.on_worker(Message::Lost { channel: 1 });
        let fx = gone(&mut s, "The connection was lost.");
        assert!(reconnects(&fx));

        // The reconnect cannot reach the first jump host yet: try again,
        // once, without "Press Enter".
        let mut s = chain(&e);
        s.auto_reconnect = true;
        s.start(80, 24);
        s.set_retrying();
        let fx = s.on_worker(Message::Disconnected {
            channel: 2,
            reason: "Timed out connecting to bastion.example.org.".into(),
        });
        let n = fx.iter().filter(|f| matches!(f, Effect::Reconnect)).count();
        assert_eq!(n, 1);
        assert!(!shown(&fx).contains("Press Enter"));
        assert_eq!(s.state(), State::Ended);

        // A jump host that refuses the login stops it.
        let mut s = chain(&e);
        s.start(80, 24);
        s.set_retrying();
        s.on_worker(Message::HostKey {
            channel: 2,
            key: ed25519_blob(),
        });
        let fx = s.on_worker(Message::Disconnected {
            channel: 2,
            reason: "Permission denied.".into(),
        });
        assert!(!reconnects(&fx));
    }

    #[test]
    fn the_same_name_through_other_jump_hosts_is_another_connection() {
        let dir = temp("chain-share");
        let e = jump_env(&dir);
        let mut s = chain(&e);
        s.start(80, 24);
        hop_in(&mut s, 2);
        hop_in(&mut s, 4);
        hop_in(&mut s, 1);
        let same = Target::resolve_route("inner", &e).unwrap();
        assert!(s.can_share(&same));
        let direct = Target::resolve("10.0.0.5", &e).unwrap();
        assert!(!s.can_share(&direct));
    }

    #[test]
    fn a_proxy_command_carries_the_first_login() {
        let dir = temp("chain-command");
        let e = Env {
            config: "Host inner\n  ProxyJump bastion\nHost bastion\n  ProxyCommand nc %h %p\n"
                .into(),
            ..env(&dir)
        };
        let mut s = chain(&e);
        let fx = s.start(80, 24);
        let Some(Effect::ProxyCommand {
            connection: 2,
            tunnel,
            command,
        }) = fx.iter().find(|f| matches!(f, Effect::ProxyCommand { .. }))
        else {
            panic!("not what the login does next");
        };
        assert_eq!(command.args, ["bastion", "22"]);
        assert!(sent(&fx).iter().any(|m| matches!(m,
            Message::Connect { channel: 2, tunnel: Some(t), .. } if t == tunnel)));
    }

    #[test]
    fn prompts_collect_answers_in_order_without_echoing_secrets() {
        let dir = temp("prompt");
        let e = env(&dir);
        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        trusted(&mut s);
        let fx = s.on_worker(Message::Prompt {
            channel: 1,
            kind: PromptKind::KeyboardInteractive,
            name: "\x1b[31mMFA".into(),
            instructions: String::new(),
            fields: vec![
                PromptField {
                    text: "Password: ".into(),
                    echo: false,
                },
                PromptField {
                    text: "Code: ".into(),
                    echo: true,
                },
            ],
        });
        let text = shown(&fx);
        assert!(text.contains("MFA") && !text.contains('\x1b'));
        assert!(s.prompt_input(b"hunter2").is_empty());
        s.prompt_input(&[0x7f]);
        s.prompt_input(b"2");
        let fx = s.prompt_input(b"\r");
        assert!(shown(&fx).contains("Code: "));
        assert_eq!(shown(&s.prompt_input(b"42")), "42");
        let fx = s.prompt_input(b"\r");
        match sent(&fx)[..] {
            [Message::Answers { answers, .. }] => {
                assert_eq!(answers.0, vec!["hunter2".to_owned(), "42".to_owned()]);
            }
            ref other => panic!("{other:?}"),
        }
    }

    fn password_prompt(channel: ChannelId) -> Message {
        Message::Prompt {
            channel,
            kind: PromptKind::Password,
            name: String::new(),
            instructions: String::new(),
            fields: vec![PromptField {
                text: "Password: ".into(),
                echo: false,
            }],
        }
    }

    fn answers(fx: &[Effect]) -> Vec<String> {
        match sent(fx)[..] {
            [Message::Answers { answers, .. }] => answers.0.clone(),
            ref other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_password_source_answers_once_then_the_user_types() {
        let dir = temp("source");
        let e = env(&dir);
        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        trusted(&mut s);
        s.password_source = true;
        let fx = s.on_worker(password_prompt(1));
        assert!(matches!(fx[..], [Effect::FetchPassword]), "{fx:?}");
        assert!(s.fetching_password() && !s.prompt_active());
        let fx = s.password_fetched(Ok(Zeroizing::new("s3cret".into())), "1Password");
        assert!(shown(&fx).contains("Password from 1Password."));
        assert!(!shown(&fx).contains("s3cret"));
        assert_eq!(answers(&fx), ["s3cret"]);
        // Refused: the server asks again, and now the user types.
        let fx = s.on_worker(password_prompt(1));
        assert!(shown(&fx).contains("did not work"));
        assert!(s.prompt_active());
        s.prompt_input(b"typed");
        assert_eq!(answers(&s.prompt_input(b"\r")), ["typed"]);
    }

    #[test]
    fn a_failed_source_falls_back_to_the_prompt() {
        let dir = temp("source-fail");
        let e = env(&dir);
        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        trusted(&mut s);
        s.password_source = true;
        // PAM's keyboard-interactive password counts too.
        let fx = s.on_worker(Message::Prompt {
            channel: 1,
            kind: PromptKind::KeyboardInteractive,
            name: String::new(),
            instructions: String::new(),
            fields: vec![PromptField {
                text: "(admin@host) Password: ".into(),
                echo: false,
            }],
        });
        assert!(matches!(fx[..], [Effect::FetchPassword]), "{fx:?}");
        let fx = s.password_fetched(Err("bw ended with code 1: locked".into()), "Bitwarden");
        assert!(shown(&fx).contains("locked") && shown(&fx).contains("Password:"));
        s.prompt_input(b"pw");
        assert_eq!(answers(&s.prompt_input(b"\r")), ["pw"]);
        // A one-time code is never taken from the source.
        let fx = s.on_worker(Message::Prompt {
            channel: 1,
            kind: PromptKind::KeyboardInteractive,
            name: String::new(),
            instructions: String::new(),
            fields: vec![PromptField {
                text: "Verification code: ".into(),
                echo: false,
            }],
        });
        assert!(!fx.iter().any(|e| matches!(e, Effect::FetchPassword)));
    }

    #[test]
    fn a_typed_password_is_kept_only_after_it_worked() {
        let dir = temp("remember");
        let e = env(&dir);
        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        trusted(&mut s);
        s.password_source = true;
        s.remember_password = true;
        s.on_worker(password_prompt(1));
        s.password_fetched(Err("No saved password yet.".into()), "Windows");
        s.prompt_input(b"hunter2");
        s.prompt_input(b"\r");
        let fx = s.on_worker(Message::Connected {
            channel: 1,
            kex: "curve25519-sha256".into(),
            cipher: "chacha20-poly1305@openssh.com".into(),
            host_key: "ssh-ed25519".into(),
        });
        let stored: Vec<&str> = fx
            .iter()
            .filter_map(|e| match e {
                Effect::StorePassword { password, .. } => password.0.first().map(String::as_str),
                _ => None,
            })
            .collect();
        assert_eq!(stored, ["hunter2"]);
    }

    #[test]
    fn escape_cancels_the_connection() {
        let dir = temp("cancel");
        let e = env(&dir);
        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        trusted(&mut s);
        s.on_worker(Message::Prompt {
            channel: 1,
            kind: PromptKind::Password,
            name: String::new(),
            instructions: String::new(),
            fields: vec![PromptField {
                text: "Password: ".into(),
                echo: false,
            }],
        });
        let fx = s.prompt_input(&[0x1b]);
        assert_eq!(sent(&fx), vec![&Message::Close { channel: 1 }]);
        assert_eq!(s.state(), State::Ended);
    }

    #[test]
    fn sign_requests_are_checked() {
        let dir = temp("sign");
        let e = env(&dir);
        let mut s = SshSession::new(1, Target::resolve("host", &e).unwrap(), e.known_hosts());
        trusted(&mut s);
        let key = ed25519_blob();
        // Unknown key: refused without asking the broker.
        let fx = s.on_worker(Message::SignRequest {
            channel: 1,
            key: key.clone(),
            data: b"x".to_vec(),
        });
        assert!(!fx.iter().any(|e| matches!(e, Effect::Broker(_))));
        s.on_broker(Reply::Identities {
            keys: vec![Identity {
                public: key.clone(),
                comment: "laptop".into(),
                source: Source::Agent(ferossh_auth::agent::AgentKind::OpenSsh),
            }],
            notes: vec![],
        });
        // Not a user-auth request: refused.
        let fx = s.on_worker(Message::SignRequest {
            channel: 1,
            key: key.clone(),
            data: b"please sign this document".to_vec(),
        });
        assert!(shown(&fx).contains("refused"));
        assert!(matches!(
            sent(&fx)[..],
            [Message::Signature { signature, .. }] if signature.is_empty()
        ));
    }

    #[test]
    fn a_worker_that_asks_out_of_turn_is_cut_off() {
        let dir = temp("turns");
        let e = env(&dir);
        let target = || Target::resolve("host", &e).unwrap();
        let closed = |fx: &[Effect]| {
            shown(fx).contains("not how a login goes")
                && sent(fx).contains(&&Message::Close { channel: 1 })
        };
        let sign = Message::SignRequest {
            channel: 1,
            key: ed25519_blob(),
            data: b"x".to_vec(),
        };

        // Before the host key: no prompts, keys or signatures.
        for msg in [
            password_prompt(1),
            sign.clone(),
            Message::IdentitiesRequest { channel: 1 },
        ] {
            let mut s = SshSession::new(1, target(), e.known_hosts());
            s.start(80, 24);
            let fx = s.on_worker(msg);
            assert!(closed(&fx), "{fx:?}");
            assert_eq!(s.state(), State::Ended);
        }

        // One host key per login.
        let mut s = SshSession::new(1, target(), e.known_hosts());
        trusted(&mut s);
        let fx = s.on_worker(Message::HostKey {
            channel: 1,
            key: ed25519_blob(),
        });
        assert!(closed(&fx));

        // Nothing of the kind once logged in.
        let mut s = SshSession::new(1, target(), e.known_hosts());
        trusted(&mut s);
        s.on_worker(Message::Connected {
            channel: 1,
            kex: "k".into(),
            cipher: "c".into(),
            host_key: "h".into(),
        });
        assert!(closed(&s.on_worker(password_prompt(1))));

        // A shell on an existing connection never logs in.
        let mut s = SshSession::new(1, target(), e.known_hosts());
        s.start_on(7, 80, 24);
        assert!(closed(&s.on_worker(Message::HostKey {
            channel: 1,
            key: ed25519_blob(),
        })));

        // Too many questions, or too many signatures.
        let mut s = SshSession::new(1, target(), e.known_hosts());
        trusted(&mut s);
        let fields = vec![
            PromptField {
                text: "?".into(),
                echo: true,
            };
            MAX_PROMPT_FIELDS + 1
        ];
        assert!(closed(&s.on_worker(Message::Prompt {
            channel: 1,
            kind: PromptKind::KeyboardInteractive,
            name: String::new(),
            instructions: String::new(),
            fields,
        })));
        let mut s = SshSession::new(1, target(), e.known_hosts());
        trusted(&mut s);
        for _ in 0..MAX_SIGNATURES {
            assert!(!closed(&s.on_worker(sign.clone())));
        }
        assert!(closed(&s.on_worker(sign)));
    }

    #[test]
    fn sanitizes_server_text() {
        assert_eq!(sanitize("a\x1b[2Jb\nc\u{202E}d\x07"), "a[2Jb\r\ncd");
    }
}
