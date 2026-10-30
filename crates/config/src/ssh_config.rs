//! The part of OpenSSH's `ssh_config` that FeroSSH uses: `Host` blocks
//! with `HostName`, `User`, `Port`, `IdentityFile`, `Tag`, the forwarding
//! keywords (`LocalForward`, `RemoteForward`, `DynamicForward`,
//! `ForwardX11`, `GatewayPorts`, `ClearAllForwardings`), how to get there
//! (`ProxyJump`, `ProxyCommand`), `Include`d files, and the list of hosts
//! the connection manager shows.
//!
//! The rules follow `ssh_config(5)`: keywords are case-insensitive, the
//! first value found for a keyword wins (except `IdentityFile` and the
//! forwards, which accumulate), and a `Host` line matches when any of its patterns matches
//! the name typed and no `!` pattern does. `Match` blocks are skipped.
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
    folder: Option<String>,
}

impl Line {
    fn mark(keyword: &str) -> Self {
        Self {
            keyword: keyword.to_owned(),
            args: Vec::new(),
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
/// typed.
pub fn resolve_in(text: &str, name: &str, includes: &dyn Includes) -> HostConfig {
    let mut cfg = HostConfig::default();
    // Lines before the first Host apply to every host.
    let mut active = true;
    // The state where each open Include started. A Host line inside an
    // included file does not leak into the file that included it, and an
    // Include in a block that does not match never matches, as in OpenSSH.
    let mut outer: Vec<bool> = Vec::new();
    for Line { keyword, args, .. } in lines(text, includes) {
        match keyword.as_str() {
            "{" => outer.push(active),
            "}" => active = outer.pop().unwrap_or(active),
            "host" => active = outer.iter().all(|&a| a) && host_line_matches(&args, name),
            "match" => active = false,
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
            "identityfile" => cfg.identity_files.extend(args.into_iter().take(1)),
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
            _ => {}
        }
    }
    cfg
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
    let valid = !host.is_empty()
        && !host.starts_with('-')
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
        assert_eq!(d("@host"), None);
        assert_eq!(d("ho st"), None);
        assert_eq!(d("-oProxyCommand=x"), None);
        assert_eq!(d("-host"), None);
    }
}
