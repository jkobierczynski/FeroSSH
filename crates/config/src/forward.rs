//! Port forward specifications, as `ssh_config` and the `ssh` command line
//! write them: `LocalForward`/`-L`, `RemoteForward`/`-R` and
//! `DynamicForward`/`-D`.
//!
//! `-L [bind:]port:host:hostport`, `-R [bind:]port:host:hostport` and
//! `-D [bind:]port`; in `ssh_config` the listening side and the target are
//! two arguments (`LocalForward 8080 db:5432`). IPv6 addresses go in
//! brackets.
//!
//! `-R [bind:]port` without a target is a remote dynamic forward
//! (OpenSSH 7.6): the server listens and speaks SOCKS, and this PC
//! connects wherever each client there asks.
//!
//! Either side may be a Unix socket instead of an address and a port,
//! as in `-L 2375:/var/run/docker.sock` or `-R /tmp/app.sock:localhost:80`:
//! a field with a `/` or a `\` in it is a socket path. A path with a `:`
//! in it (a Windows drive letter) goes in brackets:
//! `-L [C:\Users\me\db.sock]:db:5432`. Paths may not hold spaces.

use std::fmt;

/// Which side listens, and what happens to a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForwardKind {
    /// Listen on this PC; the server connects to the target.
    Local,
    /// The server listens; this PC connects to the target, or, without a
    /// target, wherever the SOCKS client on the server asks.
    Remote,
    /// Listen on this PC as a SOCKS 4, 4a and 5 proxy; the server connects
    /// wherever each client asks.
    Dynamic,
}

impl ForwardKind {
    /// `L`, `R` or `D`, as in the `ssh` options.
    pub fn letter(self) -> char {
        match self {
            ForwardKind::Local => 'L',
            ForwardKind::Remote => 'R',
            ForwardKind::Dynamic => 'D',
        }
    }
}

/// Where a forward's connections go.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// A host (or address) and a port.
    Tcp(String, u16),
    /// A Unix socket, by its path.
    Unix(String),
}

impl fmt::Display for Target {
    /// `db:5432`, `[::1]:80` or the socket's path.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Tcp(host, port) => write!(f, "{}:{port}", host_text(host)),
            Target::Unix(path) => f.write_str(&host_text(path)),
        }
    }
}

/// One forward.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForwardSpec {
    pub kind: ForwardKind,
    /// The listening address as written: `None` when left out, `Some("*")`
    /// for every interface (an empty address, as in `:8080`, reads as `*`,
    /// so a spec reads back as written). Always `None` for a socket.
    pub bind: Option<String>,
    /// The listening port; 0 lets the listening side choose. 0 for a
    /// socket.
    pub port: u16,
    /// The Unix socket the listening side listens on, instead of a port.
    pub socket: Option<String>,
    /// Where connections go; `None` for a dynamic forward, local or
    /// remote.
    pub target: Option<Target>,
}

/// Wraps an IPv6 address (or a path with a drive letter) in brackets, as
/// forward specs write it.
fn host_text(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

impl fmt::Display for ForwardSpec {
    /// The command-line form: `-L 8080:db:5432`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "-{} {}", self.kind.letter(), self.listen_text())?;
        if let Some(target) = &self.target {
            write!(f, ":{target}")?;
        }
        Ok(())
    }
}

impl ForwardSpec {
    /// A remote forward without a target: the server speaks SOCKS.
    pub fn remote_dynamic(&self) -> bool {
        self.kind == ForwardKind::Remote && self.target.is_none()
    }

    /// The listening side as written: `8080`, `*:8080`, `[::1]:8080` or
    /// a socket's path.
    pub fn listen_text(&self) -> String {
        if let Some(path) = &self.socket {
            return host_text(path);
        }
        match &self.bind {
            Some(bind) => {
                let bind = if bind.is_empty() { "*" } else { bind };
                format!("{}:{}", host_text(bind), self.port)
            }
            None => self.port.to_string(),
        }
    }

    /// The `ssh_config` line: `LocalForward 8080 db:5432`.
    pub fn config_line(&self) -> String {
        let keyword = match self.kind {
            ForwardKind::Local => "LocalForward",
            ForwardKind::Remote => "RemoteForward",
            ForwardKind::Dynamic => "DynamicForward",
        };
        let mut out = format!("{keyword} {}", self.listen_text());
        if let Some(target) = &self.target {
            out.push_str(&format!(" {target}"));
        }
        out
    }
}

/// Splits on `:` outside brackets, dropping the brackets.
fn fields(text: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut bracket = false;
    for c in text.chars() {
        match c {
            '[' if !bracket && cur.is_empty() => bracket = true,
            ']' if bracket => bracket = false,
            ':' if !bracket => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if bracket {
        return None;
    }
    out.push(cur);
    Some(out)
}

fn port(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Host names and addresses as a target or listening address may be
/// written: no spaces, no control characters, nothing a server or a
/// resolver could read as an option.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%' | '*'))
}

/// The longest socket path: what fits in a `sockaddr_un` with its
/// terminating zero, on Linux and on Windows.
pub const MAX_SOCKET_PATH: usize = 107;

/// Whether a field names a Unix socket rather than an address or a port.
fn is_path(field: &str) -> bool {
    field.contains(['/', '\\'])
}

/// A socket path as a forward may name it: printable, no spaces or
/// quotes (so that an `ssh_config` line reads back the same), no
/// brackets, nothing that
/// reads as an option, and short enough for a socket address.
pub fn valid_socket_path(path: &str) -> bool {
    is_path(path)
        && path.len() <= MAX_SOCKET_PATH
        && !path.starts_with('-')
        && path
            .chars()
            .all(|c| !c.is_control() && !c.is_whitespace() && !matches!(c, '[' | ']' | '"'))
}

fn socket_field(text: &str) -> Option<String> {
    valid_socket_path(text).then(|| text.to_owned())
}

/// The listening address; empty means every interface, as `*` does, and
/// reads as `*` so that writing the spec out and reading it back gives the
/// same spec.
fn bind_field(text: &str) -> Option<String> {
    if text.is_empty() {
        Some("*".to_owned())
    } else if text == "*" || valid_host(text) {
        Some(text.to_owned())
    } else {
        None
    }
}

/// Parses the listening side and the target, already split into fields:
/// `[bind, port, host, hostport]` with the bind optional, either side a
/// socket path in one field, or for a dynamic forward `[bind, port]`
/// with the bind optional.
fn from_fields(kind: ForwardKind, f: &[String]) -> Option<ForwardSpec> {
    // The target, from the end.
    let (listen, target) = match f.split_last() {
        Some((last, head)) if is_path(last) && !head.is_empty() => {
            (head, Some(Target::Unix(socket_field(last)?)))
        }
        _ => {
            let dynamic = match kind {
                ForwardKind::Dynamic => true,
                ForwardKind::Local => false,
                // `-R [bind:]port` alone; a path alone has no target.
                ForwardKind::Remote => f.len() <= 2 && !f.first().is_some_and(|p| is_path(p)),
            };
            if dynamic {
                (f, None)
            } else {
                let at = f.len().checked_sub(2)?;
                let (head, tail) = f.split_at(at);
                let [host, p] = tail else {
                    return None;
                };
                if !valid_host(host) || host.contains('*') {
                    return None;
                }
                (
                    head,
                    Some(Target::Tcp(host.clone(), port(p).filter(|&p| p != 0)?)),
                )
            }
        }
    };
    let (bind, listen, socket) = match listen {
        [path] if is_path(path) => (None, 0, Some(socket_field(path)?)),
        [p] => (None, port(p)?, None),
        [b, p] => (Some(bind_field(b)?), port(p)?, None),
        _ => return None,
    };
    if socket.is_some() && kind == ForwardKind::Dynamic {
        return None;
    }
    // A local listener on port 0 would move every time: refuse it, as ssh
    // does for -L and -D. The server may choose a remote port.
    if socket.is_none() && listen == 0 && kind != ForwardKind::Remote {
        return None;
    }
    Some(ForwardSpec {
        kind,
        bind,
        port: listen,
        socket,
        target,
    })
}

/// Parses an `ssh_config` line's arguments: `LocalForward [bind:]port
/// host:hostport` (the command-line form in one argument works too) or
/// `DynamicForward [bind:]port`.
pub fn parse_config(kind: ForwardKind, args: &[String]) -> Option<ForwardSpec> {
    let mut all = Vec::new();
    for a in args {
        all.extend(fields(a)?);
    }
    from_fields(kind, &all)
}

/// Parses what the user types to add a forward: `-L 8080:db:5432`,
/// `-R 9000:localhost:3000`, `-D 1080`, or the same with `L`, `R`, `D`
/// and no dash. A space may separate the listening side from the target,
/// as in `ssh_config`.
pub fn parse_option(text: &str) -> Option<ForwardSpec> {
    let text = text.trim();
    let text = text.strip_prefix('-').unwrap_or(text);
    let mut chars = text.chars();
    let kind = match chars.next()?.to_ascii_uppercase() {
        'L' => ForwardKind::Local,
        'R' => ForwardKind::Remote,
        'D' => ForwardKind::Dynamic,
        _ => return None,
    };
    let args: Vec<String> = chars
        .as_str()
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect();
    parse_config(kind, &args)
}

/// What `-KL [bind:]port` and its `-KR`, `-KD` forms name, as `ssh`'s
/// `~C` command line takes them to cancel a forward: the kind, the
/// listening address if given, and the port; or a socket's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelSpec {
    pub kind: ForwardKind,
    pub bind: Option<String>,
    pub port: u16,
    pub socket: Option<String>,
}

impl CancelSpec {
    /// Whether this names `spec`. Without an address it names the
    /// forward on that port whatever it listens on; `*` and an empty
    /// address are the same.
    pub fn cancels(&self, spec: &ForwardSpec) -> bool {
        let every = |b: &str| {
            if b == "*" {
                String::new()
            } else {
                b.to_ascii_lowercase()
            }
        };
        if spec.kind != self.kind {
            return false;
        }
        if self.socket.is_some() || spec.socket.is_some() {
            return self.socket == spec.socket;
        }
        spec.port == self.port
            && match (&self.bind, &spec.bind) {
                (None, _) => true,
                (Some(want), Some(have)) => every(want) == every(have),
                (Some(_), None) => false,
            }
    }
}

/// Parses `-KL 8080`, `-KR 127.0.0.1:9000`, `-KD 1080` or
/// `-KR /tmp/app.sock` (the dash and the space are optional).
pub fn parse_cancel(text: &str) -> Option<CancelSpec> {
    let text = text.trim();
    let text = text.strip_prefix('-').unwrap_or(text);
    let mut chars = text.chars();
    if !chars.next()?.eq_ignore_ascii_case(&'K') {
        return None;
    }
    let kind = match chars.next()?.to_ascii_uppercase() {
        'L' => ForwardKind::Local,
        'R' => ForwardKind::Remote,
        'D' => ForwardKind::Dynamic,
        _ => return None,
    };
    let f = fields(chars.as_str().trim())?;
    let (bind, listen) = match &f[..] {
        [p] if is_path(p) && kind != ForwardKind::Dynamic => {
            return Some(CancelSpec {
                kind,
                bind: None,
                port: 0,
                socket: Some(socket_field(p)?),
            });
        }
        [p] => (None, p),
        [b, p] => (Some(bind_field(b)?), p),
        _ => return None,
    };
    Some(CancelSpec {
        kind,
        bind,
        port: port(listen)?,
        socket: None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_config_lines() {
        let l = parse_config(ForwardKind::Local, &args("8080 db.internal:5432")).unwrap();
        assert_eq!(l.bind, None);
        assert_eq!(l.port, 8080);
        assert_eq!(l.target, Some(Target::Tcp("db.internal".into(), 5432)));
        let l = parse_config(ForwardKind::Local, &args("*:8080 [::1]:80")).unwrap();
        assert_eq!(l.bind.as_deref(), Some("*"));
        assert_eq!(l.target, Some(Target::Tcp("::1".into(), 80)));
        let l = parse_config(ForwardKind::Local, &args("[::1]:8080 h:80")).unwrap();
        assert_eq!(l.bind.as_deref(), Some("::1"));
        let d = parse_config(ForwardKind::Dynamic, &args("1080")).unwrap();
        assert_eq!((d.bind, d.port, d.target), (None, 1080, None));
        let d = parse_config(ForwardKind::Dynamic, &args("0.0.0.0:1080")).unwrap();
        assert_eq!(d.bind.as_deref(), Some("0.0.0.0"));
        let r = parse_config(ForwardKind::Remote, &args("0 localhost:3000")).unwrap();
        assert_eq!(r.port, 0);
    }

    #[test]
    fn rejects_bad_specs() {
        for (kind, text) in [
            (ForwardKind::Local, "8080"),
            (ForwardKind::Local, "8080 host"),
            (ForwardKind::Local, "x:y:z:w:v"),
            (ForwardKind::Local, "70000 h:80"),
            (ForwardKind::Local, "0 h:80"),
            (ForwardKind::Local, "8080 h:0"),
            (ForwardKind::Local, "8080 -oProxy:80"),
            (ForwardKind::Local, "8080 [::1:80"),
            (ForwardKind::Local, "8080 *:80"),
            (ForwardKind::Remote, "a:b:9000"),
            (ForwardKind::Remote, "/tmp/s"),
            (ForwardKind::Remote, "/tmp/s host"),
            (ForwardKind::Dynamic, "/tmp/s"),
            (ForwardKind::Local, "/tmp/s"),
            (ForwardKind::Local, "8080 /tmp/a b"),
            (ForwardKind::Local, "8080 -/x"),
            (ForwardKind::Local, "8080 host:/x"),
            (ForwardKind::Dynamic, "1080 h:80"),
            (ForwardKind::Dynamic, "0"),
            (ForwardKind::Dynamic, "a b:1080"),
        ] {
            assert_eq!(parse_config(kind, &args(text)), None, "{text}");
        }
    }

    #[test]
    fn parses_cancels_and_matches_forwards() {
        let c = parse_cancel("-KL 8080").unwrap();
        assert_eq!(
            (c.kind, c.bind.as_deref(), c.port),
            (ForwardKind::Local, None, 8080)
        );
        let c = parse_cancel("KR127.0.0.1:9000").unwrap();
        assert_eq!(c.bind.as_deref(), Some("127.0.0.1"));
        assert_eq!(
            parse_cancel("-kd [::1]:1080").unwrap().bind.as_deref(),
            Some("::1")
        );
        for bad in [
            "-K",
            "-KX 80",
            "-KL",
            "-KL x",
            "-KL 8080:h:80",
            "-L 8080:h:80",
        ] {
            assert_eq!(parse_cancel(bad), None, "{bad}");
        }
        let l = parse_option("-L 8080:db:5432").unwrap();
        let star = parse_option("-L *:8080:db:5432").unwrap();
        assert!(parse_cancel("-KL 8080").unwrap().cancels(&l));
        assert!(parse_cancel("-KL 8080").unwrap().cancels(&star));
        assert!(parse_cancel("-KL :8080").unwrap().cancels(&star));
        assert!(!parse_cancel("-KL 127.0.0.1:8080").unwrap().cancels(&l));
        assert!(!parse_cancel("-KR 8080").unwrap().cancels(&l));
        assert!(!parse_cancel("-KL 8081").unwrap().cancels(&l));
    }

    #[test]
    fn empty_bind_reads_as_every_interface() {
        // An empty address means `*`; it must read back the same once
        // written out (found by the import_parse fuzz target).
        for text in ["-D :1080", "-L :8080:db:5432", "-R :9000:localhost:3000"] {
            let spec = parse_option(text).unwrap();
            assert_eq!(spec.bind.as_deref(), Some("*"), "{text}");
            let line = spec.config_line();
            let (_, rest) = line.split_once(' ').unwrap();
            assert_eq!(parse_config(spec.kind, &args(rest)), Some(spec), "{line}");
        }
        // PuTTY's `D=2`: an empty listening side and a port.
        let d = parse_config(ForwardKind::Dynamic, &[String::new(), "2".into()]).unwrap();
        assert_eq!(d.bind.as_deref(), Some("*"));
    }

    #[test]
    fn parses_options_and_prints_them_back() {
        for text in [
            "-L 8080:db:5432",
            "-L *:8080:db:5432",
            "-L [::1]:8080:[fe80::1%eth0]:22",
            "-R 9000:localhost:3000",
            "-R 0:localhost:3000",
            "-D 1080",
            "-D localhost:1080",
        ] {
            let spec = parse_option(text).unwrap();
            assert_eq!(spec.to_string(), text);
            let line = spec.config_line();
            let (keyword, rest) = line.split_once(' ').unwrap();
            let kind = match keyword {
                "LocalForward" => ForwardKind::Local,
                "RemoteForward" => ForwardKind::Remote,
                _ => ForwardKind::Dynamic,
            };
            assert_eq!(parse_config(kind, &args(rest)), Some(spec), "{line}");
        }
        assert_eq!(
            parse_option("l 8080 db:5432"),
            parse_option("-L 8080:db:5432")
        );
        assert_eq!(parse_option("-X 1"), None);
        assert_eq!(parse_option(""), None);
        assert_eq!(parse_option("-L"), None);
    }

    #[test]
    fn parses_remote_dynamic_and_socket_forwards() {
        let r = parse_option("-R 1080").unwrap();
        assert!(r.remote_dynamic());
        assert_eq!((r.bind.as_deref(), r.port), (None, 1080));
        let r = parse_option("-R localhost:1080").unwrap();
        assert_eq!(
            (r.bind.as_deref(), r.port, r.target),
            (Some("localhost"), 1080, None)
        );
        assert!(parse_option("-R 0").unwrap().remote_dynamic());
        assert!(!parse_option("-R 9000:h:80").unwrap().remote_dynamic());

        let l = parse_option("-L 2375:/var/run/docker.sock").unwrap();
        assert_eq!(l.port, 2375);
        assert_eq!(l.target, Some(Target::Unix("/var/run/docker.sock".into())));
        let l = parse_option("-L 127.0.0.1:2375:/run/d.sock").unwrap();
        assert_eq!(l.bind.as_deref(), Some("127.0.0.1"));
        let l = parse_option("-L /tmp/db.sock:db:5432").unwrap();
        assert_eq!(
            (l.socket.as_deref(), l.port, l.bind),
            (Some("/tmp/db.sock"), 0, None)
        );
        let l = parse_option(r"-L [C:\Users\me\db.sock]:/run/pg.sock").unwrap();
        assert_eq!(l.socket.as_deref(), Some(r"C:\Users\me\db.sock"));
        assert_eq!(l.target, Some(Target::Unix("/run/pg.sock".into())));
        let r = parse_option("-R /tmp/app.sock:localhost:80").unwrap();
        assert_eq!(r.socket.as_deref(), Some("/tmp/app.sock"));
        assert_eq!(r.target, Some(Target::Tcp("localhost".into(), 80)));
        let r = parse_config(
            ForwardKind::Remote,
            &args("/run/user/1000/gnupg/S.gpg-agent /tmp/x"),
        );
        assert!(r.is_some());
        let long = format!("/{}", "a".repeat(MAX_SOCKET_PATH));
        assert_eq!(parse_option(&format!("-L 80:{long}")), None);
        assert_eq!(parse_option("-D /tmp/s"), None);
        assert_eq!(parse_option("-R /tmp/s"), None);
    }

    #[test]
    fn socket_and_remote_dynamic_specs_read_back() {
        for text in [
            "-R 1080",
            "-R localhost:1080",
            "-R *:1080",
            "-L 2375:/var/run/docker.sock",
            "-L /tmp/db.sock:db:5432",
            "-L /tmp/a.sock:/tmp/b.sock",
            r"-L [C:\Users\me\db.sock]:db:5432",
            "-R /tmp/app.sock:localhost:80",
            "-R /tmp/app.sock:/run/app.sock",
        ] {
            let spec = parse_option(text).unwrap();
            assert_eq!(spec.to_string(), text);
            let line = spec.config_line();
            let (_, rest) = line.split_once(' ').unwrap();
            assert_eq!(parse_config(spec.kind, &args(rest)), Some(spec), "{line}");
        }
    }

    #[test]
    fn cancels_socket_forwards() {
        let s = parse_option("-R /tmp/app.sock:localhost:80").unwrap();
        let c = parse_cancel("-KR /tmp/app.sock").unwrap();
        assert!(c.cancels(&s));
        assert!(!parse_cancel("-KR /tmp/other.sock").unwrap().cancels(&s));
        assert!(!parse_cancel("-KR 0").unwrap().cancels(&s));
        assert_eq!(parse_cancel("-KD /tmp/s"), None);
    }

    /// What the forward_parse fuzz target checks, over many generated
    /// specs: whatever parses reads back the same, from both forms.
    #[test]
    fn generated_specs_read_back() {
        const PARTS: [&str; 14] = [
            "8080",
            "0",
            "1080",
            "db",
            "[::1]",
            "*",
            "",
            "/tmp/s.sock",
            r"[C:\x\a.sock]",
            "localhost",
            "/run/a b",
            "-x",
            "a/b",
            "65536",
        ];
        let mut seed = 0x2545_f491_u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        for _ in 0..50_000 {
            let n = 1 + next() % 4;
            let fields: Vec<&str> = (0..n).map(|_| PARTS[next() % PARTS.len()]).collect();
            let letter = ["L", "R", "D"][next() % 3];
            let text = format!("-{letter} {}", fields.join(":"));
            let Some(spec) = parse_option(&text) else {
                continue;
            };
            assert_eq!(
                parse_option(&spec.to_string()).as_ref(),
                Some(&spec),
                "{text}"
            );
            let line = spec.config_line();
            assert_eq!(
                parse_config(spec.kind, &args(line.split_once(' ').unwrap().1)).as_ref(),
                Some(&spec),
                "{line}"
            );
        }
    }
}
