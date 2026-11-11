//! A SOCKS 5 or HTTP proxy that a connection goes through to reach its
//! host (the first jump host, when there are any), written as a URL:
//!
//! ```text
//! Host build
//!     ProxyServer socks5://me@proxy.corp:1080
//! Host web
//!     ProxyServer http://proxy.corp:3128
//! ```
//!
//! `ProxyServer` is FeroSSH's own keyword; OpenSSH stops at keywords it
//! does not know, so a `~/.ssh/config` it shares with OpenSSH needs
//! `IgnoreUnknown ProxyServer` before it. Like `ProxyJump` and
//! `ProxyCommand`, whichever of the three comes first counts.
//!
//! The usual `ProxyCommand`s for the same thing (`nc -X 5 -x proxy:1080
//! %h %p`, `ncat --proxy ... --proxy-type socks5 %h %p`, `connect -S ...
//! %h %p`) are read as a `ProxyServer` too, so they work on Windows, where
//! these programs are rarely installed, and no program runs.
//!
//! A URL never holds a password: the user types it at the first connection
//! and FeroSSH keeps it in the Windows Credential Manager.

use std::fmt;

use crate::ssh_config::parse_destination;

/// Which protocol the proxy speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Socks5,
    /// HTTP `CONNECT`.
    Http,
}

impl ProxyKind {
    /// The port such a proxy listens on when none is given.
    pub fn default_port(self) -> u16 {
        match self {
            ProxyKind::Socks5 => 1080,
            ProxyKind::Http => 8080,
        }
    }

    fn scheme(self) -> &'static str {
        match self {
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Http => "http",
        }
    }
}

/// A proxy to go through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyServer {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    /// The user name to log in to the proxy with; its password is asked
    /// for. `None`: no login.
    pub user: Option<String>,
}

/// Whether `user` can be a proxy user name: one line, no `:` (which HTTP
/// Basic authentication cannot carry), no `@`, at most 255 bytes.
fn valid_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 255
        && !user.contains([':', '@', '/', '"', '#'])
        && !user.chars().any(|c| c.is_control() || c.is_whitespace())
}

impl ProxyServer {
    /// Parses `socks5://[user@]host[:port]` or `http://[user@]host[:port]`
    /// (`socks5h://` and `socks://` mean SOCKS 5 too); IPv6 addresses go
    /// in brackets. `None` for anything else, a password in the URL
    /// included.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (scheme, rest) = text.split_once("://")?;
        let kind = match scheme.to_ascii_lowercase().as_str() {
            "socks5" | "socks5h" | "socks" => ProxyKind::Socks5,
            "http" => ProxyKind::Http,
            _ => return None,
        };
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        if rest.contains('/') {
            return None;
        }
        // `user:password@` would leave the password in a file.
        if rest
            .rsplit_once('@')
            .is_some_and(|(user, _)| !valid_user(user))
        {
            return None;
        }
        let (user, host, port) = parse_destination(rest)?;
        Some(Self {
            kind,
            host,
            port: port.unwrap_or(kind.default_port()),
            user,
        })
    }

    /// The proxy a `ProxyCommand` of a well-known proxy tool asks for,
    /// when the command does nothing else: OpenBSD `nc -X 5|connect -x
    /// host[:port] [-P user] %h %p`, `ncat --proxy host[:port]
    /// --proxy-type socks5|http %h %p` and `connect -S|-H host[:port] %h
    /// %p`. Programs may carry a path and `.exe`.
    pub fn from_command(argv: &[String]) -> Option<Self> {
        let (program, args) = argv.split_first()?;
        let name = program
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(program)
            .to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        // The destination comes last, as `%h %p`.
        let (opts, dest) = args.split_at(args.len().checked_sub(2)?);
        if dest != ["%h", "%p"] {
            return None;
        }
        let mut kind = None;
        let mut addr = None;
        let mut user = None;
        let mut it = opts.iter();
        match name {
            "nc" | "netcat" => {
                // Without -X, OpenBSD nc speaks SOCKS 5 to an -x proxy.
                kind = Some(ProxyKind::Socks5);
                while let Some(opt) = it.next() {
                    match opt.as_str() {
                        "-X" => {
                            kind = match it.next()?.to_ascii_lowercase().as_str() {
                                "5" => Some(ProxyKind::Socks5),
                                "connect" => Some(ProxyKind::Http),
                                _ => return None,
                            };
                        }
                        "-x" => addr = Some(it.next()?.clone()),
                        "-P" => user = Some(it.next()?.clone()),
                        _ => return None,
                    }
                }
            }
            "ncat" => {
                while let Some(opt) = it.next() {
                    match opt.as_str() {
                        "--proxy" => addr = Some(it.next()?.clone()),
                        "--proxy-type" => {
                            kind = match it.next()?.to_ascii_lowercase().as_str() {
                                "socks5" => Some(ProxyKind::Socks5),
                                "http" => Some(ProxyKind::Http),
                                _ => return None,
                            };
                        }
                        _ => return None,
                    }
                }
                // ncat's default proxy type is HTTP.
                kind = kind.or(Some(ProxyKind::Http));
            }
            "connect" => {
                while let Some(opt) = it.next() {
                    match opt.as_str() {
                        "-S" => {
                            kind = Some(ProxyKind::Socks5);
                            addr = Some(it.next()?.clone());
                        }
                        "-H" => {
                            kind = Some(ProxyKind::Http);
                            addr = Some(it.next()?.clone());
                        }
                        _ => return None,
                    }
                }
            }
            _ => return None,
        }
        let kind = kind?;
        let addr = addr?;
        // `connect` takes `user@host:port` itself.
        let (in_addr, host, port) = parse_destination(&addr)?;
        let user = user.or(in_addr);
        if user.as_deref().is_some_and(|u| !valid_user(u)) || host.contains('%') {
            return None;
        }
        Some(Self {
            kind,
            host,
            port: port.unwrap_or(kind.default_port()),
            user,
        })
    }
}

impl fmt::Display for ProxyServer {
    /// The URL [`ProxyServer::parse`] reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.kind.scheme())?;
        if let Some(user) = &self.user {
            write!(f, "{user}@")?;
        }
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn argv(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn urls_round_trip() {
        for (text, kind, host, port, user, back) in [
            (
                "socks5://proxy:1081",
                ProxyKind::Socks5,
                "proxy",
                1081,
                None,
                "socks5://proxy:1081",
            ),
            (
                "SOCKS5H://me@proxy/",
                ProxyKind::Socks5,
                "proxy",
                1080,
                Some("me"),
                "socks5://me@proxy:1080",
            ),
            (
                "http://[2001:db8::1]",
                ProxyKind::Http,
                "2001:db8::1",
                8080,
                None,
                "http://[2001:db8::1]:8080",
            ),
            (
                "http://10.0.0.1:3128",
                ProxyKind::Http,
                "10.0.0.1",
                3128,
                None,
                "http://10.0.0.1:3128",
            ),
        ] {
            let p = ProxyServer::parse(text).unwrap();
            assert_eq!(p.kind, kind, "{text}");
            assert_eq!(p.host, host, "{text}");
            assert_eq!(p.port, port, "{text}");
            assert_eq!(p.user.as_deref(), user, "{text}");
            assert_eq!(p.to_string(), back);
            assert_eq!(ProxyServer::parse(back), Some(p));
        }
    }

    #[test]
    fn bad_urls_and_passwords_are_refused() {
        for text in [
            "proxy:1080",
            "socks4://proxy",
            "https://proxy",
            "socks5://me:secret@proxy",
            "socks5://proxy:0",
            "socks5://proxy/path",
            "socks5://",
            "http://a b",
            "http://-oProxyCommand=calc",
            "socks5://@proxy",
        ] {
            assert_eq!(ProxyServer::parse(text), None, "{text}");
        }
    }

    #[test]
    fn proxy_tool_commands_are_read() {
        let p = ProxyServer::from_command(&argv("nc -X 5 -x proxy:1081 %h %p")).unwrap();
        assert_eq!(p.to_string(), "socks5://proxy:1081");
        let p = ProxyServer::from_command(&argv("/usr/bin/nc -x proxy %h %p")).unwrap();
        assert_eq!(p.to_string(), "socks5://proxy:1080");
        let p =
            ProxyServer::from_command(&argv("nc -X connect -x proxy:3128 -P me %h %p")).unwrap();
        assert_eq!(p.to_string(), "http://me@proxy:3128");
        let p = ProxyServer::from_command(&argv(
            r"C:\Tools\ncat.exe --proxy proxy:1080 --proxy-type socks5 %h %p",
        ))
        .unwrap();
        assert_eq!(p.to_string(), "socks5://proxy:1080");
        let p = ProxyServer::from_command(&argv("ncat --proxy proxy %h %p")).unwrap();
        assert_eq!(p.to_string(), "http://proxy:8080");
        let p = ProxyServer::from_command(&argv("connect -S me@proxy:1080 %h %p")).unwrap();
        assert_eq!(p.to_string(), "socks5://me@proxy:1080");
        let p = ProxyServer::from_command(&argv("connect -H proxy:3128 %h %p")).unwrap();
        assert_eq!(p.to_string(), "http://proxy:3128");
    }

    #[test]
    fn other_commands_stay_commands() {
        for line in [
            "nc %h %p",
            "nc -X 4 -x proxy %h %p",
            "nc -X 5 -x proxy -w 5 %h %p",
            "nc -x proxy %h:%p",
            "nc -x proxy %h %p extra",
            "ncat --proxy proxy --proxy-auth me:pw %h %p",
            "ncat --proxy proxy --proxy-type socks4 %h %p",
            "connect %h %p",
            "ssh -W %h:%p bastion",
            "nc -x %h %h %p",
            "corkscrew proxy 3128 %h %p",
        ] {
            assert_eq!(ProxyServer::from_command(&argv(line)), None, "{line}");
        }
    }
}
