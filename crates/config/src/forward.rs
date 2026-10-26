//! Port forward specifications, as `ssh_config` and the `ssh` command line
//! write them: `LocalForward`/`-L`, `RemoteForward`/`-R` and
//! `DynamicForward`/`-D`.
//!
//! `-L [bind:]port:host:hostport`, `-R [bind:]port:host:hostport` and
//! `-D [bind:]port`; in `ssh_config` the listening side and the target are
//! two arguments (`LocalForward 8080 db:5432`). IPv6 addresses go in
//! brackets. Unix sockets and remote dynamic forwards (`-R port` alone) are
//! not supported.

use std::fmt;

/// Which side listens, and what happens to a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForwardKind {
    /// Listen on this PC; the server connects to the target.
    Local,
    /// The server listens; this PC connects to the target.
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

/// One forward.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForwardSpec {
    pub kind: ForwardKind,
    /// The listening address as written: `None` when left out, `Some("")`
    /// or `Some("*")` for every interface.
    pub bind: Option<String>,
    /// The listening port; 0 lets the listening side choose.
    pub port: u16,
    /// Where connections go; `None` for a dynamic forward.
    pub target: Option<(String, u16)>,
}

/// Wraps an IPv6 address in brackets, as forward specs write it.
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
        write!(f, "-{} ", self.kind.letter())?;
        if let Some(bind) = &self.bind {
            let bind = if bind.is_empty() { "*" } else { bind };
            write!(f, "{}:", host_text(bind))?;
        }
        write!(f, "{}", self.port)?;
        if let Some((host, port)) = &self.target {
            write!(f, ":{}:{port}", host_text(host))?;
        }
        Ok(())
    }
}

impl ForwardSpec {
    /// The `ssh_config` line: `LocalForward 8080 db:5432`.
    pub fn config_line(&self) -> String {
        let keyword = match self.kind {
            ForwardKind::Local => "LocalForward",
            ForwardKind::Remote => "RemoteForward",
            ForwardKind::Dynamic => "DynamicForward",
        };
        let mut out = format!("{keyword} ");
        if let Some(bind) = &self.bind {
            let bind = if bind.is_empty() { "*" } else { bind };
            out.push_str(&host_text(bind));
            out.push(':');
        }
        out.push_str(&self.port.to_string());
        if let Some((host, port)) = &self.target {
            out.push_str(&format!(" {}:{port}", host_text(host)));
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

fn bind_field(text: &str) -> Option<String> {
    if text.is_empty() || text == "*" || valid_host(text) {
        Some(text.to_owned())
    } else {
        None
    }
}

/// Parses the listening side and the target, already split into fields:
/// `[bind, port, host, hostport]` with the bind optional, or for a dynamic
/// forward `[bind, port]` with the bind optional.
fn from_fields(kind: ForwardKind, f: &[String]) -> Option<ForwardSpec> {
    let (bind, rest) = match (kind, f.len()) {
        (ForwardKind::Dynamic, 1) | (ForwardKind::Local | ForwardKind::Remote, 3) => (None, f),
        (ForwardKind::Dynamic, 2) | (ForwardKind::Local | ForwardKind::Remote, 4) => {
            let (b, rest) = f.split_first()?;
            (Some(bind_field(b)?), rest)
        }
        _ => return None,
    };
    let (listen, target) = rest.split_first()?;
    let listen = port(listen)?;
    let target = match target {
        [] => None,
        [host, p] if valid_host(host) && !host.contains('*') => {
            Some((host.clone(), port(p).filter(|&p| p != 0)?))
        }
        _ => return None,
    };
    // A local listener on port 0 would move every time: refuse it, as ssh
    // does for -L and -D. The server may choose a remote port.
    if listen == 0 && kind != ForwardKind::Remote {
        return None;
    }
    Some(ForwardSpec {
        kind,
        bind,
        port: listen,
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
/// listening address if given, and the port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelSpec {
    pub kind: ForwardKind,
    pub bind: Option<String>,
    pub port: u16,
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
        spec.kind == self.kind
            && spec.port == self.port
            && match (&self.bind, &spec.bind) {
                (None, _) => true,
                (Some(want), Some(have)) => every(want) == every(have),
                (Some(_), None) => false,
            }
    }
}

/// Parses `-KL 8080`, `-KR 127.0.0.1:9000` or `-KD 1080` (the dash and the
/// space are optional).
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
        [p] => (None, p),
        [b, p] => (Some(bind_field(b)?), p),
        _ => return None,
    };
    Some(CancelSpec {
        kind,
        bind,
        port: port(listen)?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        assert_eq!(l.target, Some(("db.internal".into(), 5432)));
        let l = parse_config(ForwardKind::Local, &args("*:8080 [::1]:80")).unwrap();
        assert_eq!(l.bind.as_deref(), Some("*"));
        assert_eq!(l.target, Some(("::1".into(), 80)));
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
            (ForwardKind::Remote, "9000"),
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
}
