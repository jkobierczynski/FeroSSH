//! The part of OpenSSH's `ssh_config` that FeroSSH uses: `Host` and
//! `Match` blocks with `HostName`, `User`, `Port`, the keys
//! (`IdentityFile`, `CertificateFile`, `IdentitiesOnly`, `IdentityAgent`,
//! `ForwardAgent`), `Tag`, the forwarding keywords (`LocalForward`,
//! `RemoteForward`, `DynamicForward`, `ForwardX11`, `GatewayPorts`,
//! `ClearAllForwardings`), the session (`RemoteCommand`, `RequestTTY`,
//! `SendEnv`, `SetEnv`), host keys (`StrictHostKeyChecking`,
//! `UserKnownHostsFile`, `HostKeyAlias`), how to get there (`ProxyJump`,
//! `ProxyCommand`, `AddressFamily`), Kerberos (`GSSAPIAuthentication`,
//! `GSSAPIDelegateCredentials`, `GSSAPIServerIdentity`), `Include`d
//! files, and the list of hosts the connection manager shows.
//!
//! The rules follow `ssh_config(5)`: keywords are case-insensitive, the
//! first value found for a keyword wins (except `IdentityFile`,
//! `CertificateFile`, `SendEnv` and the forwards, which accumulate), and a
//! `Host` line matches when any of its patterns matches the name typed and
//! no `!` pattern does. `Match` takes `all`, `host`, `originalhost`,
//! `user`, `localuser`, `tagged`, `canonical` and `final` (with `!` to
//! negate); `Match exec`, `localnetwork` and criteria FeroSSH does not know
//! never match, so FeroSSH runs no command to read its config.
//! `Include` reads files through an [`Includes`], so the parser itself
//! never touches the disk.
//!
//! The connection manager groups hosts into folders: a comment line
//! `# folder: Name` puts the hosts after it in that folder, and hosts from
//! an included file without such a comment go in a folder named after the
//! file.

use crate::forward::{ForwardKind, ForwardSpec, parse_config};
use crate::known_hosts::match_pattern_list;

/// Settings for one host after applying the config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostConfig {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// As written, before `~` and `%` expansion; see [`expand_path`].
    pub identity_files: Vec<String>,
    /// The `Tag` keyword (OpenSSH 9.4+), which also picks a colour theme.
    pub tag: Option<String>,
    /// `LocalForward`, `RemoteForward` and `DynamicForward`, in order.
    /// Lines that do not parse are left out.
    pub forwards: Vec<ForwardSpec>,
    pub forward_x11: Option<bool>,
    /// Local and dynamic forwards without a listening address listen on
    /// every interface, not only on loopback.
    pub gateway_ports: Option<bool>,
    /// `ClearAllForwardings yes`: no forwards at all.
    pub clear_all_forwardings: Option<bool>,
    /// Seconds between keepalives when the server is quiet; 0 turns them
    /// off.
    pub server_alive_interval: Option<u32>,
    /// Unanswered keepalives before the connection counts as dead.
    pub server_alive_count_max: Option<u32>,
    /// Seconds to wait for the TCP connection and the server's banner.
    pub connect_timeout: Option<u32>,
    /// `ProxyJump` or `ProxyCommand`, whichever came first, as OpenSSH
    /// takes them.
    pub proxy: Option<Proxy>,
    /// `ForwardAgent yes`: lets the server use the agent's keys.
    pub forward_agent: Option<bool>,
    /// `CertificateFile`, as written; they accumulate like `IdentityFile`.
    pub certificate_files: Vec<String>,
    /// `IdentitiesOnly yes`: only the keys `IdentityFile` names, also
    /// when an agent holds more.
    pub identities_only: Option<bool>,
    /// `IdentityAgent`: `none`, `SSH_AUTH_SOCK`, or an agent's pipe.
    pub identity_agent: Option<String>,
    /// `SendEnv`: names (patterns) of local variables to pass on.
    pub send_env: Vec<String>,
    /// `SetEnv`: variables to set on the server, first value wins.
    pub set_env: Vec<(String, String)>,
    /// `RemoteCommand`: run this instead of a login shell. Empty for
    /// `RemoteCommand none`.
    pub remote_command: Option<String>,
    pub request_tty: Option<RequestTty>,
    pub strict_host_key_checking: Option<StrictHostKeyChecking>,
    /// `UserKnownHostsFile`: the files to read, the first to write; empty
    /// for `none`.
    pub user_known_hosts_files: Option<Vec<String>>,
    /// `HostKeyAlias`: the name to look the host key up under.
    pub host_key_alias: Option<String>,
    pub address_family: Option<AddressFamily>,
    /// `GSSAPIAuthentication`: log in with Kerberos tickets. Unset means
    /// FeroSSH's own choice (when Windows logged on to a domain).
    pub gssapi_authentication: Option<bool>,
    /// `GSSAPIDelegateCredentials yes`: hand the server a ticket of the
    /// user's own, for onward logins from there.
    pub gssapi_delegate_credentials: Option<bool>,
    /// `GSSAPIServerIdentity`: the host name in the server's Kerberos
    /// service principal, when not the one connected to.
    pub gssapi_server_identity: Option<String>,
}

/// `RequestTTY`: whether the session gets a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestTty {
    No,
    Yes,
    Force,
    /// A terminal unless a `RemoteCommand` runs (the default).
    Auto,
}

/// `StrictHostKeyChecking`: what to do with a host not in known_hosts.
/// A changed key is never accepted, whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictHostKeyChecking {
    /// Refuse it.
    Yes,
    /// Show its fingerprint and ask (the default).
    Ask,
    /// Trust it and add it to known_hosts without asking (`accept-new`,
    /// and `no` or `off`, which FeroSSH treats the same).
    AcceptNew,
}

/// `AddressFamily`: which IP versions to connect over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
    Any,
    Inet,
    Inet6,
}

/// What `Match` compares against besides the name typed.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatchContext<'a> {
    /// The Windows user name (`Match localuser`, the default remote user).
    pub local_user: &'a str,
    /// The user given on the command line or the prompt (`user@host`),
    /// which wins over `User`.
    pub user: Option<&'a str>,
}

/// How a host is reached when not directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proxy {
    /// `ProxyJump none` or `ProxyCommand none`: directly, whatever later
    /// lines say.
    Direct,
    /// `ProxyJump`: through these hosts in turn, each `[user@]host[:port]`
    /// (or a `Host` alias) as written.
    Jump(Vec<String>),
    /// `ProxyCommand`: a program whose standard input and output carry
    /// the connection, split into the program and its arguments, tokens
    /// not yet filled in (see [`proxy_command`]).
    Command(Vec<String>),
}

/// The hosts on a `ProxyJump` line (or `-J`): comma-separated
/// `[user@]host[:port]`, each optionally as `ssh://[user@]host[:port]`.
/// `None` when one of them does not parse; `none` is [`Proxy::Direct`].
pub fn parse_proxy_jump(text: &str) -> Option<Proxy> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("none") {
        return Some(Proxy::Direct);
    }
    let mut hops = Vec::new();
    for hop in text.split(',') {
        let hop = hop.trim();
        let hop = hop.strip_prefix("ssh://").unwrap_or(hop);
        let hop = hop.strip_suffix('/').unwrap_or(hop);
        parse_destination(hop)?;
        hops.push(hop.to_owned());
    }
    (!hops.is_empty()).then_some(Proxy::Jump(hops))
}

/// Most hops one connection goes through, nested `ProxyJump`s included.
pub const MAX_HOPS: usize = 8;

/// A `ProxyCommand`'s program and arguments with OpenSSH's tokens filled
/// into the arguments: `%h` the host name, `%p` the port, `%r` the remote
/// user, `%n` the name as typed, `%%` a `%`. The program itself is never
/// expanded, so no host name can pick what runs.
pub fn proxy_command(
    argv: &[String],
    host: &str,
    port: u16,
    user: &str,
    alias: &str,
) -> Option<(String, Vec<String>)> {
    let (program, args) = argv.split_first()?;
    let expand = |arg: &String| {
        let mut out = String::with_capacity(arg.len());
        let mut chars = arg.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('h') => out.push_str(host),
                Some('p') => out.push_str(&port.to_string()),
                Some('r') => out.push_str(user),
                Some('n') => out.push_str(alias),
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        }
        out
    };
    (!program.is_empty()).then(|| (program.clone(), args.iter().map(expand).collect()))
}

impl HostConfig {
    /// The forwards to set up, after `ClearAllForwardings`.
    pub fn active_forwards(&self) -> &[ForwardSpec] {
        if self.clear_all_forwardings == Some(true) {
            &[]
        } else {
            &self.forwards
        }
    }
}

/// A whole number of seconds (or a count), capped at a day.
fn seconds(args: &[String]) -> Option<u32> {
    args.first()?.parse::<u32>().ok().map(|n| n.min(86_400))
}

/// `yes` or `no` (and `true`/`false`), as ssh_config writes flags.
fn flag(args: &[String]) -> Option<bool> {
    match args.first()?.to_ascii_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

/// Include files nest at most this deep, as in OpenSSH.
const MAX_INCLUDE_DEPTH: usize = 16;

/// About the most lines a config may grow to with its Includes read. A file
/// that includes itself twice (or `Include *` in `~/.ssh`, which matches
/// `config` itself) would otherwise expand to n^16 copies before the
/// depth limit stops it, and FeroSSH would hang reading its config.
const MAX_LINES: usize = 1 << 16;

/// Reads the files an `Include` line names.
pub trait Includes {
    /// The files `pattern` names, in order, as (file name, text).
    fn read(&self, pattern: &str) -> Vec<(String, String)>;
}

/// Ignores `Include` lines.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoIncludes;

impl Includes for NoIncludes {
    fn read(&self, _: &str) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// Reads included files from disk: relative paths are relative to the
/// `.ssh` folder, `~` is the home folder, and `*` and `?` may appear in
/// the file name (not in folder names).
#[derive(Debug, Clone)]
pub struct DirIncludes {
    pub home: std::path::PathBuf,
}

impl Includes for DirIncludes {
    fn read(&self, pattern: &str) -> Vec<(String, String)> {
        use std::path::{Path, PathBuf};
        let path = match pattern.strip_prefix('~') {
            Some(rest) if rest.starts_with(['/', '\\']) => {
                self.home.join(rest.trim_start_matches(['/', '\\']))
            }
            _ if Path::new(pattern).is_absolute() => PathBuf::from(pattern),
            _ => self.home.join(".ssh").join(pattern),
        };
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Vec::new();
        };
        let name = name.to_string_lossy();
        let mut found: Vec<PathBuf> = if name.contains(['*', '?']) {
            std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                        .filter(|e| {
                            crate::pattern::match_pattern(&e.file_name().to_string_lossy(), &name)
                        })
                        .map(|e| e.path())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            vec![path.clone()]
        };
        // OpenSSH reads glob matches in lexical order.
        found.sort();
        found
            .into_iter()
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                let stem = p.file_stem()?.to_string_lossy().into_owned();
                Some((stem, text))
            })
            .collect()
    }
}

/// One line of the config with Includes expanded: keyword, arguments, and
/// the folder it belongs to. Empty keywords mark where an included file
/// starts (`"{"`) and ends (`"}"`).
struct Line {
    keyword: String,
    args: Vec<String>,
    /// The text after the keyword, unsplit: `RemoteCommand` keeps its
    /// quotes for the server's shell.
    raw: String,
    folder: Option<String>,
}

impl Line {
    fn mark(keyword: &str) -> Self {
        Self {
            keyword: keyword.to_owned(),
            args: Vec::new(),
            raw: String::new(),
            folder: None,
        }
    }
}

/// `# folder: Name` or `#folder Name`, as the connection manager's folders.
pub(crate) fn folder_comment(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix('#')?.trim_start();
    let head = rest.get(..6)?;
    if !head.eq_ignore_ascii_case("folder") {
        return None;
    }
    let name = rest.get(6..)?.trim_start_matches([':', ' ', '\t']).trim();
    Some(name.to_owned())
}

fn flatten(
    text: &str,
    folder: Option<String>,
    includes: &dyn Includes,
    depth: usize,
    out: &mut Vec<Line>,
) {
    let mut folder = folder;
    for raw in text.lines() {
        if let Some(name) = folder_comment(raw) {
            folder = (!name.is_empty()).then_some(name);
            continue;
        }
        let Some((keyword, args)) = split_line(raw) else {
            continue;
        };
        if out.len() >= MAX_LINES {
            return;
        }
        if keyword == "include" {
            if depth >= MAX_INCLUDE_DEPTH {
                continue;
            }
            for pattern in &args {
                for (stem, text) in includes.read(pattern) {
                    if out.len() >= MAX_LINES {
                        return;
                    }
                    let inner = folder.clone().or(Some(stem));
                    out.push(Line::mark("{"));
                    flatten(&text, inner, includes, depth + 1, out);
                    out.push(Line::mark("}"));
                }
            }
            continue;
        }
        out.push(Line {
            keyword,
            args,
            raw: rest_of_line(raw),
            folder: folder.clone(),
        });
    }
}

/// The config's lines with every `Include` read in place.
fn lines(text: &str, includes: &dyn Includes) -> Vec<Line> {
    let mut out = Vec::new();
    flatten(text, None, includes, 0, &mut out);
    out
}

/// A host the connection manager lists: a `Host` pattern without wildcards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub name: String,
    pub folder: Option<String>,
}

/// Every host named on a `Host` line, in file order, without duplicates.
/// Patterns with `*` or `?` and negated ones are not hosts one can pick.
pub fn hosts(text: &str, includes: &dyn Includes) -> Vec<HostEntry> {
    let mut out: Vec<HostEntry> = Vec::new();
    for line in lines(text, includes) {
        if line.keyword != "host" {
            continue;
        }
        for pattern in line.args.iter().flat_map(|a| a.split(',')) {
            if pattern.is_empty()
                || pattern.starts_with('!')
                || pattern.contains(['*', '?'])
                || out.iter().any(|h| h.name.eq_ignore_ascii_case(pattern))
            {
                continue;
            }
            out.push(HostEntry {
                name: pattern.to_owned(),
                folder: line.folder.clone(),
            });
        }
    }
    out
}

/// The text after a line's keyword (and its `=`), trimmed.
fn rest_of_line(line: &str) -> String {
    let line = line.trim();
    let end = line
        .find(|c: char| c.is_ascii_whitespace() || c == '=')
        .unwrap_or(line.len());
    let rest = line.get(end..).unwrap_or_default().trim_start();
    rest.strip_prefix('=').unwrap_or(rest).trim().to_owned()
}

/// Splits a config line into keyword and arguments, honouring `=` and
/// double quotes.
pub(crate) fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_ascii_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    let mut args = Vec::new();
    let mut chars = rest.chars().peekable();
    while chars.peek().is_some() {
        while chars.next_if(char::is_ascii_whitespace).is_some() {}
        let mut arg = String::new();
        let mut quoted = false;
        let mut any = false;
        while let Some(&c) = chars.peek() {
            if c == '"' {
                quoted = !quoted;
                any = true;
                chars.next();
            } else if c.is_ascii_whitespace() && !quoted {
                break;
            } else {
                arg.push(c);
                any = true;
                chars.next();
            }
        }
        if any {
            args.push(arg);
        }
    }
    Some((keyword.to_ascii_lowercase(), args))
}

/// Space-separated `Host` patterns, each possibly negated.
fn host_line_matches(patterns: &[String], name: &str) -> bool {
    let mut matched = false;
    for p in patterns {
        if let Some(neg) = p.strip_prefix('!') {
            if match_pattern_list(neg, name) {
                return false;
            }
        } else if match_pattern_list(p, name) {
            matched = true;
        }
    }
    matched
}

/// Applies the config text to the name the user typed, ignoring `Include`.
pub fn resolve(text: &str, name: &str) -> HostConfig {
    resolve_in(text, name, &NoIncludes)
}

/// Applies the config text, with its included files, to the name the user
/// typed, with no local user name for `Match`.
pub fn resolve_in(text: &str, name: &str, includes: &dyn Includes) -> HostConfig {
    resolve_for(text, name, includes, &MatchContext::default())
}

/// Applies the config text, with its included files, to the name the user
/// typed, as OpenSSH does: a first pass, then a second one for `Match
/// canonical` and `Match final` blocks when the config has any. FeroSSH
/// does not canonicalize host names, so both match only in that pass.
pub fn resolve_for(
    text: &str,
    name: &str,
    includes: &dyn Includes,
    ctx: &MatchContext<'_>,
) -> HostConfig {
    let lines = lines(text, includes);
    let mut cfg = HostConfig::default();
    let wants_final = apply(&lines, name, ctx, false, &mut cfg);
    if wants_final {
        apply(&lines, name, ctx, true, &mut cfg);
    }
    cfg
}

/// One pass over the lines; returns whether a `Match` asked for the final
/// pass.
fn apply(
    lines: &[Line],
    name: &str,
    ctx: &MatchContext<'_>,
    final_pass: bool,
    cfg: &mut HostConfig,
) -> bool {
    let mut wants_final = false;
    // Lines before the first Host apply to every host.
    let mut active = true;
    // The state where each open Include started. A Host line inside an
    // included file does not leak into the file that included it, and an
    // Include in a block that does not match never matches, as in OpenSSH.
    let mut outer: Vec<bool> = Vec::new();
    for Line {
        keyword, args, raw, ..
    } in lines
    {
        let args = args.clone();
        match keyword.as_str() {
            "{" => outer.push(active),
            "}" => active = outer.pop().unwrap_or(active),
            "host" => active = outer.iter().all(|&a| a) && host_line_matches(&args, name),
            "match" => {
                let (matched, asks_final) = match_line_matches(&args, name, ctx, cfg, final_pass);
                wants_final |= asks_final;
                active = outer.iter().all(|&a| a) && matched;
            }
            _ if !active => {}
            "hostname" => {
                if cfg.hostname.is_none() {
                    cfg.hostname = args.into_iter().next();
                }
            }
            "user" => {
                if cfg.user.is_none() {
                    cfg.user = args.into_iter().next();
                }
            }
            "port" => {
                if cfg.port.is_none() {
                    cfg.port = args
                        .first()
                        .and_then(|p| p.parse().ok())
                        .filter(|&p| p != 0);
                }
            }
            "identityfile" => {
                for f in args.into_iter().take(1) {
                    if !cfg.identity_files.contains(&f) {
                        cfg.identity_files.push(f);
                    }
                }
            }
            "certificatefile" => {
                for f in args.into_iter().take(1) {
                    if !cfg.certificate_files.contains(&f) {
                        cfg.certificate_files.push(f);
                    }
                }
            }
            "identitiesonly" if cfg.identities_only.is_none() => {
                cfg.identities_only = flag(&args);
            }
            "identityagent" if cfg.identity_agent.is_none() => {
                cfg.identity_agent = args.into_iter().next();
            }
            "forwardagent" if cfg.forward_agent.is_none() => cfg.forward_agent = flag(&args),
            "gssapiauthentication" if cfg.gssapi_authentication.is_none() => {
                cfg.gssapi_authentication = flag(&args);
            }
            "gssapidelegatecredentials" if cfg.gssapi_delegate_credentials.is_none() => {
                cfg.gssapi_delegate_credentials = flag(&args);
            }
            "gssapiserveridentity" if cfg.gssapi_server_identity.is_none() => {
                cfg.gssapi_server_identity = args.into_iter().next().filter(|h| valid_gss_host(h));
            }
            "tag" if cfg.tag.is_none() => cfg.tag = args.into_iter().next(),
            "localforward" | "remoteforward" | "dynamicforward" => {
                let kind = match keyword.as_str() {
                    "localforward" => ForwardKind::Local,
                    "remoteforward" => ForwardKind::Remote,
                    _ => ForwardKind::Dynamic,
                };
                if let Some(spec) = parse_config(kind, &args)
                    && !cfg.forwards.contains(&spec)
                {
                    cfg.forwards.push(spec);
                }
            }
            "forwardx11" if cfg.forward_x11.is_none() => cfg.forward_x11 = flag(&args),
            "gatewayports" if cfg.gateway_ports.is_none() => cfg.gateway_ports = flag(&args),
            "clearallforwardings" if cfg.clear_all_forwardings.is_none() => {
                cfg.clear_all_forwardings = flag(&args);
            }
            "serveraliveinterval" if cfg.server_alive_interval.is_none() => {
                cfg.server_alive_interval = seconds(&args);
            }
            "serveralivecountmax" if cfg.server_alive_count_max.is_none() => {
                cfg.server_alive_count_max = seconds(&args).filter(|&n| n > 0);
            }
            "connecttimeout" if cfg.connect_timeout.is_none() => {
                cfg.connect_timeout = seconds(&args).filter(|&n| n > 0);
            }
            "proxyjump" if cfg.proxy.is_none() => {
                cfg.proxy = args.first().and_then(|a| parse_proxy_jump(a));
            }
            "proxycommand" if cfg.proxy.is_none() => {
                cfg.proxy = match args.as_slice() {
                    [] => None,
                    [one] if one.eq_ignore_ascii_case("none") => Some(Proxy::Direct),
                    argv => Some(Proxy::Command(argv.to_vec())),
                };
            }
            "sendenv" => {
                for pattern in args {
                    if let Some(gone) = pattern.strip_prefix('-') {
                        // `-PATTERN` takes earlier names back (OpenSSH 8.7).
                        cfg.send_env
                            .retain(|p| !crate::pattern::match_pattern(p, gone));
                    } else if valid_env_name(&pattern.replace(['*', '?'], "A"))
                        && !cfg.send_env.contains(&pattern)
                    {
                        cfg.send_env.push(pattern);
                    }
                }
            }
            "setenv" => {
                for pair in args {
                    if let Some((k, v)) = pair.split_once('=')
                        && valid_env_name(k)
                        && !cfg.set_env.iter().any(|(n, _)| n == k)
                    {
                        cfg.set_env.push((k.to_owned(), v.to_owned()));
                    }
                }
            }
            "remotecommand" if cfg.remote_command.is_none() => {
                cfg.remote_command = Some(if raw.eq_ignore_ascii_case("none") {
                    String::new()
                } else {
                    raw.clone()
                });
            }
            "requesttty" if cfg.request_tty.is_none() => {
                cfg.request_tty = match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
                    Some("no" | "false") => Some(RequestTty::No),
                    Some("yes" | "true") => Some(RequestTty::Yes),
                    Some("force") => Some(RequestTty::Force),
                    Some("auto") => Some(RequestTty::Auto),
                    _ => None,
                };
            }
            "stricthostkeychecking" if cfg.strict_host_key_checking.is_none() => {
                cfg.strict_host_key_checking =
                    match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
                        Some("yes" | "true") => Some(StrictHostKeyChecking::Yes),
                        Some("ask") => Some(StrictHostKeyChecking::Ask),
                        Some("accept-new" | "no" | "false" | "off") => {
                            Some(StrictHostKeyChecking::AcceptNew)
                        }
                        _ => None,
                    };
            }
            "userknownhostsfile" if cfg.user_known_hosts_files.is_none() && !args.is_empty() => {
                cfg.user_known_hosts_files = Some(match args.as_slice() {
                    [one] if one.eq_ignore_ascii_case("none") => Vec::new(),
                    files => files.to_vec(),
                });
            }
            "hostkeyalias" if cfg.host_key_alias.is_none() => {
                cfg.host_key_alias = args.into_iter().next();
            }
            "addressfamily" if cfg.address_family.is_none() => {
                cfg.address_family = match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
                    Some("any") => Some(AddressFamily::Any),
                    Some("inet") => Some(AddressFamily::Inet),
                    Some("inet6") => Some(AddressFamily::Inet6),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    wants_final
}

/// A name `SendEnv` and `SetEnv` may use: letters, digits and `_`, not
/// starting with a digit, as a POSIX shell takes them.
pub fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A host name `GSSAPIServerIdentity` may give: a DNS name or address,
/// nothing that changes the service principal's form (`/`, `@`).
pub fn valid_gss_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
}

/// Whether a `Match` line matches, and whether it names `canonical` or
/// `final` (which asks for a second pass). Criteria are ANDed; a `!`
/// before one negates it. `exec`, `localnetwork`, `version` and unknown
/// criteria never match.
fn match_line_matches(
    args: &[String],
    name: &str,
    ctx: &MatchContext<'_>,
    cfg: &HostConfig,
    final_pass: bool,
) -> (bool, bool) {
    let mut matched = true;
    let mut asks_final = false;
    let mut args = args.iter();
    if args.len() == 0 {
        return (false, false);
    }
    while let Some(word) = args.next() {
        let lower = word.to_ascii_lowercase();
        let (negate, criterion) = match lower.strip_prefix('!') {
            Some(c) => (true, c.to_owned()),
            None => (false, lower),
        };
        let result = match criterion.as_str() {
            "all" => true,
            "canonical" | "final" => {
                asks_final = true;
                final_pass
            }
            "host" | "originalhost" | "user" | "localuser" | "tagged" => {
                let Some(list) = args.next() else {
                    return (false, asks_final);
                };
                let subject = match criterion.as_str() {
                    "host" => cfg
                        .hostname
                        .as_deref()
                        .map(|h| expand_match_host(h, name))
                        .unwrap_or_else(|| name.to_owned()),
                    "originalhost" => name.to_owned(),
                    "user" => ctx
                        .user
                        .or(cfg.user.as_deref())
                        .unwrap_or(ctx.local_user)
                        .to_owned(),
                    "localuser" => ctx.local_user.to_owned(),
                    _ => cfg.tag.clone().unwrap_or_default(),
                };
                !subject.is_empty() && match_pattern_list(list, &subject)
            }
            // A command to run, the local network, the version: FeroSSH
            // reads its config without running anything, so these never
            // match, negated or not: a block meant for when a command
            // fails must not apply everywhere.
            "exec" | "localnetwork" | "version" => return (false, asks_final),
            _ => return (false, asks_final),
        };
        matched &= result != negate;
    }
    (matched, asks_final)
}

/// `%h` and `%%` in a `HostName`, for `Match host`.
fn expand_match_host(hostname: &str, name: &str) -> String {
    hostname
        .replace("%%", "\u{0}")
        .replace("%h", name)
        .replace('\u{0}', "%")
}

/// Expands `~` and the `%d %u %h %r %%` tokens in an `IdentityFile` path.
pub fn expand_path(
    path: &str,
    home: &str,
    local_user: &str,
    host: &str,
    remote_user: &str,
) -> String {
    let path = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("{home}{rest}"),
        _ => path.to_owned(),
    };
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('d') => out.push_str(home),
            Some('u') => out.push_str(local_user),
            Some('h') => out.push_str(host),
            Some('r') => out.push_str(remote_user),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Parses `[user@]host[:port]` as typed on the command line or at the
/// connect prompt. IPv6 literals go in brackets: `[::1]:2222`.
pub fn parse_destination(text: &str) -> Option<(Option<String>, String, Option<u16>)> {
    let text = text.trim();
    let (user, rest) = match text.rsplit_once('@') {
        Some((u, r)) if !u.is_empty() => (Some(u.to_owned()), r),
        Some(_) => return None,
        None => (None, text),
    };
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        (host, port)
    } else if rest.matches(':').count() == 1 {
        let (host, port) = rest.split_once(':')?;
        (host, Some(port.parse().ok()?))
    } else {
        (rest, None)
    };
    // A host with one colon reads back as `host:port`, so only an IPv6
    // address (two or more colons) may carry one, even inside brackets.
    let valid = !host.is_empty()
        && !host.starts_with('-')
        && host.matches(':').count() != 1
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%'));
    if !valid || port == Some(0) {
        return None;
    }
    Some((user, host.to_owned(), port))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
User global
# routers
Host core-* !core-lab
    HostName %h.net.example
    Port 2222
    IdentityFile ~/.ssh/net_ed25519

Host web
    hostname=10.1.2.3
    User "deploy"
    IdentityFile "C:\Users\me\keys\web key"

Match host other
    User nobody

Host *
    Port 22
    IdentityFile ~/.ssh/id_ed25519
"#;

    #[test]
    fn first_value_wins_and_identities_accumulate() {
        let c = resolve(CONFIG, "core-1");
        assert_eq!(c.hostname.as_deref(), Some("%h.net.example"));
        assert_eq!(c.user.as_deref(), Some("global"));
        assert_eq!(c.port, Some(2222));
        assert_eq!(
            c.identity_files,
            vec!["~/.ssh/net_ed25519", "~/.ssh/id_ed25519"]
        );
    }

    #[test]
    fn proxy_jump_and_proxy_command_whichever_comes_first() {
        let text = "\
Host inner
    ProxyJump admin@bastion:2222,ssh://edge
Host inner
    ProxyCommand nc %h %p
Host piped
    ProxyCommand \"C:\\Program Files\\ncat.exe\" --proxy %r@proxy %h:%p
    ProxyJump bastion
Host direct
    ProxyJump none
Host *
    ProxyJump bastion
Host bad
    ProxyJump -oProxyCommand=calc
";
        assert_eq!(
            resolve(text, "inner").proxy,
            Some(Proxy::Jump(vec![
                "admin@bastion:2222".into(),
                "edge".into()
            ]))
        );
        let Some(Proxy::Command(argv)) = resolve(text, "piped").proxy else {
            panic!("no command");
        };
        assert_eq!(argv[0], r"C:\Program Files\ncat.exe");
        let (program, args) = proxy_command(&argv, "db.lan", 2200, "me", "piped").unwrap();
        assert_eq!(program, r"C:\Program Files\ncat.exe");
        assert_eq!(args, ["--proxy", "me@proxy", "db.lan:2200"]);
        assert_eq!(resolve(text, "direct").proxy, Some(Proxy::Direct));
        assert_eq!(
            resolve(text, "other").proxy,
            Some(Proxy::Jump(vec!["bastion".into()]))
        );
        // A line that does not parse is ignored, so the next one counts.
        assert_eq!(
            resolve(text, "bad").proxy,
            Some(Proxy::Jump(vec!["bastion".into()]))
        );
        assert_eq!(parse_proxy_jump("a,,b"), None);
        assert_eq!(parse_proxy_jump("NONE"), Some(Proxy::Direct));
    }

    #[test]
    fn proxy_command_tokens_never_pick_the_program() {
        let argv = vec!["%h".to_owned(), "%h%%p".to_owned(), "x%".to_owned()];
        let (program, args) = proxy_command(&argv, "evil", 22, "u", "n").unwrap();
        assert_eq!(program, "%h");
        assert_eq!(args, ["evil%p", "x%"]);
        assert_eq!(proxy_command(&[], "h", 22, "u", "n"), None);
    }

    #[test]
    fn negation_quotes_and_equals() {
        let lab = resolve(CONFIG, "core-lab");
        assert_eq!(lab.port, Some(22));
        assert_eq!(lab.hostname, None);
        let web = resolve(CONFIG, "web");
        assert_eq!(web.hostname.as_deref(), Some("10.1.2.3"));
        assert_eq!(web.user.as_deref(), Some("global"));
        assert_eq!(web.identity_files[0], r"C:\Users\me\keys\web key");
    }

    #[test]
    fn forwards_accumulate() {
        let text = "\
Host db
    LocalForward 5432 localhost:5432
    ForwardX11 yes
    RemoteForward 9000 localhost:3000
    LocalForward nonsense
Host *
    DynamicForward 1080
    LocalForward 5432 localhost:5432
    ForwardX11 no
    GatewayPorts yes
";
        let c = resolve(text, "db");
        let kinds: Vec<_> = c.forwards.iter().map(|f| (f.kind, f.port)).collect();
        assert_eq!(
            kinds,
            [
                (ForwardKind::Local, 5432),
                (ForwardKind::Remote, 9000),
                (ForwardKind::Dynamic, 1080)
            ]
        );
        assert_eq!(c.forward_x11, Some(true));
        assert_eq!(c.gateway_ports, Some(true));
        assert_eq!(c.active_forwards().len(), 3);
        let other = resolve(text, "web");
        assert_eq!(other.forward_x11, Some(false));
        let cleared = resolve(&format!("ClearAllForwardings yes\n{text}"), "db");
        assert!(cleared.active_forwards().is_empty());
    }

    #[test]
    fn keepalive_and_timeout() {
        let text = "\
Host sw*
    ServerAliveInterval 5
    ServerAliveCountMax 0
    ConnectTimeout 999999
Host *
    ServerAliveInterval 60
    ServerAliveCountMax 4
    ConnectTimeout x
";
        let c = resolve(text, "sw1");
        assert_eq!(c.server_alive_interval, Some(5));
        assert_eq!(c.server_alive_count_max, Some(4), "0 is not a count");
        assert_eq!(c.connect_timeout, Some(86_400));
        let c = resolve(text, "web");
        assert_eq!(c.server_alive_interval, Some(60));
        assert_eq!(c.connect_timeout, None);
        assert_eq!(
            resolve("ServerAliveInterval 0", "x").server_alive_interval,
            Some(0)
        );
    }

    #[test]
    fn match_blocks_are_skipped() {
        assert_eq!(resolve(CONFIG, "other").user.as_deref(), Some("global"));
    }

    fn resolve_as(text: &str, name: &str, user: Option<&str>) -> HostConfig {
        let ctx = MatchContext {
            local_user: "jurgen",
            user,
        };
        resolve_for(text, name, &NoIncludes, &ctx)
    }

    #[test]
    fn match_criteria() {
        let text = "\
Match host *.prod.example !originalhost bastion
    User ops
Match user root
    Port 2200
Match localuser jurgen
    ForwardAgent yes
Match originalhost web
    HostName web.prod.example
Match all
    Tag fallback
";
        // HostName is set after the first Match ran, so `host` only sees it
        // for names that are already in the domain.
        let c = resolve_as(text, "db.prod.example", None);
        assert_eq!(c.user.as_deref(), Some("ops"));
        assert_eq!(c.port, None);
        assert_eq!(c.forward_agent, Some(true));
        assert_eq!(c.tag.as_deref(), Some("fallback"));
        let c = resolve_as(text, "web", Some("root"));
        assert_eq!(c.user, None);
        assert_eq!(c.port, Some(2200));
        assert_eq!(c.hostname.as_deref(), Some("web.prod.example"));
        // Without a local user `localuser` cannot match.
        assert_eq!(resolve(text, "x").forward_agent, None);
    }

    #[test]
    fn match_host_sees_the_hostname_so_far() {
        let text = "\
Host web
    HostName %h.lan
Match host *.lan
    User lan
";
        assert_eq!(resolve(text, "web").user.as_deref(), Some("lan"));
        assert_eq!(resolve(text, "web.lan").user.as_deref(), Some("lan"));
        assert_eq!(resolve(text, "other").user, None);
    }

    #[test]
    fn match_exec_and_unknown_criteria_never_match() {
        let text = "\
Match exec \"true\"
    User a
Match localnetwork 10.0.0.0/8
    User b
Match frobnicate
    User c
Match !exec \"false\" host x
    User d
Match host
    User e
";
        // Not even negated: FeroSSH cannot tell whether the command fails.
        assert_eq!(resolve(text, "x").user, None);
        assert_eq!(resolve(text, "y").user, None);
    }

    #[test]
    fn match_final_applies_in_a_second_pass() {
        let text = "\
Match final host *.example
    Port 2222
Host db
    HostName db.example
Match canonical
    User late
Host *
    User early
";
        let c = resolve(text, "db");
        assert_eq!(c.port, Some(2222));
        // The first pass set User already, and the first value wins.
        assert_eq!(c.user.as_deref(), Some("early"));
        assert_eq!(
            resolve("Match !final\n User first\n", "x").user.as_deref(),
            Some("first")
        );
    }

    #[test]
    fn kerberos_keywords() {
        let text = "\
Host ad
    GSSAPIAuthentication yes
    GSSAPIDelegateCredentials yes
    GSSAPIServerIdentity sshgw.corp.example
Host bad
    GSSAPIServerIdentity evil/x@REALM
Host *
    GSSAPIAuthentication no
    GSSAPIDelegateCredentials no
";
        let c = resolve(text, "ad");
        assert_eq!(c.gssapi_authentication, Some(true));
        assert_eq!(c.gssapi_delegate_credentials, Some(true));
        assert_eq!(
            c.gssapi_server_identity.as_deref(),
            Some("sshgw.corp.example")
        );
        let c = resolve(text, "bad");
        assert_eq!(c.gssapi_authentication, Some(false));
        assert_eq!(c.gssapi_server_identity, None);
        assert_eq!(resolve("", "x").gssapi_authentication, None);
    }

    #[test]
    fn session_and_key_keywords() {
        let text = r#"
Host build
    ForwardAgent yes
    IdentitiesOnly yes
    IdentityAgent none
    CertificateFile ~/.ssh/id_ed25519-cert.pub
    CertificateFile ~/.ssh/other-cert.pub
    SendEnv LANG LC_* 9BAD BAD-NAME
    SendEnv -LC_ALL GIT_*
    SetEnv EDITOR=vim "GREETING=hello world" EDITOR=nano bad-name=x
    RemoteCommand tmux new -A -s "my session"
    RequestTTY force
    StrictHostKeyChecking accept-new
    UserKnownHostsFile ~/.ssh/known_hosts_build "C:\keys\more hosts"
    HostKeyAlias build.lan
    AddressFamily inet6
Host *
    ForwardAgent no
    RemoteCommand ls
    StrictHostKeyChecking yes
    UserKnownHostsFile none
"#;
        let c = resolve(text, "build");
        assert_eq!(c.forward_agent, Some(true));
        assert_eq!(c.identities_only, Some(true));
        assert_eq!(c.identity_agent.as_deref(), Some("none"));
        assert_eq!(
            c.certificate_files,
            vec!["~/.ssh/id_ed25519-cert.pub", "~/.ssh/other-cert.pub"]
        );
        assert_eq!(c.send_env, vec!["LANG", "LC_*", "GIT_*"]);
        assert_eq!(
            c.set_env,
            vec![
                ("EDITOR".to_owned(), "vim".to_owned()),
                ("GREETING".to_owned(), "hello world".to_owned())
            ]
        );
        assert_eq!(
            c.remote_command.as_deref(),
            Some(r#"tmux new -A -s "my session""#)
        );
        assert_eq!(c.request_tty, Some(RequestTty::Force));
        assert_eq!(
            c.strict_host_key_checking,
            Some(StrictHostKeyChecking::AcceptNew)
        );
        assert_eq!(
            c.user_known_hosts_files,
            Some(vec![
                "~/.ssh/known_hosts_build".to_owned(),
                r"C:\keys\more hosts".to_owned()
            ])
        );
        assert_eq!(c.host_key_alias.as_deref(), Some("build.lan"));
        assert_eq!(c.address_family, Some(AddressFamily::Inet6));

        let c = resolve(text, "other");
        assert_eq!(c.forward_agent, Some(false));
        assert_eq!(c.remote_command.as_deref(), Some("ls"));
        assert_eq!(c.user_known_hosts_files, Some(Vec::new()));
        let c = resolve("RemoteCommand none\nRemoteCommand ls\n", "x");
        assert_eq!(c.remote_command.as_deref(), Some(""));
        let c = resolve("StrictHostKeyChecking off\n", "x");
        assert_eq!(
            c.strict_host_key_checking,
            Some(StrictHostKeyChecking::AcceptNew)
        );
    }

    /// Serves included files from a list.
    struct Files(Vec<(&'static str, &'static str)>);

    impl Includes for Files {
        fn read(&self, pattern: &str) -> Vec<(String, String)> {
            self.0
                .iter()
                .filter(|(name, _)| crate::pattern::match_pattern(name, pattern))
                .map(|(n, t)| ((*n).to_owned(), (*t).to_owned()))
                .collect()
        }
    }

    #[test]
    fn self_including_configs_stay_small() {
        // `Include *` in ~/.ssh/config matches config itself: three files
        // that each include all three would be 3^16 copies.
        let text = "Include *\nHost h\n  User u\n";
        let all = Files(vec![("config", text), ("a", text), ("b", text)]);
        let started = std::time::Instant::now();
        assert_eq!(resolve_in(text, "h", &all).user.as_deref(), Some("u"));
        // Plus the closing marks of the includes still open at the cap.
        assert!(lines(text, &all).len() <= MAX_LINES + MAX_INCLUDE_DEPTH);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    const MAIN: &str = "\
Include work*
# folder: Home lab
Host nas pi, router *.lan
    Tag dev
Host !nas *
    User me
Host gateway
    Include conditional
";

    fn files() -> Files {
        Files(vec![
            (
                "work-eu",
                "Host db1 db2\n  HostName %h.eu.example\n  Tag production\nHost nas\n  Port 2222\n",
            ),
            ("work-us", "# folder: US\nHost web-us\n"),
            ("conditional", "Port 2200\nHost late\n  User late\n"),
        ])
    }

    #[test]
    fn lists_hosts_with_folders() {
        let list = hosts(MAIN, &files());
        let names: Vec<(&str, Option<&str>)> = list
            .iter()
            .map(|h| (h.name.as_str(), h.folder.as_deref()))
            .collect();
        assert_eq!(
            names,
            [
                ("db1", Some("work-eu")),
                ("db2", Some("work-eu")),
                ("nas", Some("work-eu")),
                ("web-us", Some("US")),
                ("pi", Some("Home lab")),
                ("router", Some("Home lab")),
                ("gateway", Some("Home lab")),
                ("late", Some("Home lab")),
            ]
        );
    }

    #[test]
    fn includes_apply_in_place() {
        let f = files();
        let db = resolve_in(MAIN, "db2", &f);
        assert_eq!(db.hostname.as_deref(), Some("%h.eu.example"));
        assert_eq!(db.tag.as_deref(), Some("production"));
        assert_eq!(db.user.as_deref(), Some("me"));
        // The included file's port wins: it comes first.
        assert_eq!(resolve_in(MAIN, "nas", &f).port, Some(2222));
        assert_eq!(resolve_in(MAIN, "nas", &f).tag.as_deref(), Some("dev"));
        // Without includes the same names know less.
        assert_eq!(resolve(MAIN, "db2").hostname, None);
        // Included inside `Host gateway`: only gateway gets the port, and
        // the Host line inside the conditional include never matches.
        assert_eq!(resolve_in(MAIN, "gateway", &f).port, Some(2200));
        assert_eq!(resolve_in(MAIN, "web", &f).port, None);
        assert_eq!(resolve_in(MAIN, "late", &f).user.as_deref(), Some("me"));
    }

    #[test]
    fn include_loops_stop() {
        let looped = Files(vec![("self", "Include self\nHost x\n")]);
        let list = hosts("Include self", &looped);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn reads_includes_from_disk() {
        let home = std::env::temp_dir().join(format!("ferossh-inc-{}", std::process::id()));
        let ssh = home.join(".ssh").join("conf.d");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::write(ssh.join("b.conf"), "Host bee\n").unwrap();
        std::fs::write(ssh.join("a.conf"), "Host ay\n").unwrap();
        std::fs::write(ssh.join("skip.txt"), "Host no\n").unwrap();
        let inc = DirIncludes { home: home.clone() };
        let list = hosts("Include conf.d/*.conf ~/.ssh/conf.d/b.conf", &inc);
        let names: Vec<&str> = list.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["ay", "bee"]);
        assert_eq!(list[0].folder.as_deref(), Some("a"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn expands_tokens() {
        assert_eq!(
            expand_path("~/.ssh/%r@%h_%u%%", r"C:\Users\me", "me", "srv", "root"),
            r"C:\Users\me/.ssh/root@srv_me%"
        );
        assert_eq!(expand_path("~user/x", "H", "u", "h", "r"), "~user/x");
    }

    #[test]
    fn destinations() {
        let d = |s| parse_destination(s);
        assert_eq!(d("host"), Some((None, "host".into(), None)));
        assert_eq!(
            d("me@host.example:2222"),
            Some((Some("me".into()), "host.example".into(), Some(2222)))
        );
        assert_eq!(
            d("a@b@host"),
            Some((Some("a@b".into()), "host".into(), None))
        );
        assert_eq!(d("[::1]:22"), Some((None, "::1".into(), Some(22))));
        assert_eq!(d("fe80::1"), Some((None, "fe80::1".into(), None)));
        assert_eq!(d("host:0"), None);
        assert_eq!(d("host:x"), None);
        assert_eq!(d("[box]:22"), Some((None, "box".into(), Some(22))));
        assert_eq!(d("[Book:s_]"), None);
        assert_eq!(d("[a:b]:22"), None);
        assert_eq!(d("@host"), None);
        assert_eq!(d("ho st"), None);
        assert_eq!(d("-oProxyCommand=x"), None);
        assert_eq!(d("-host"), None);
    }
}
