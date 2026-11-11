//! The client side of a proxy handshake, for reaching an SSH server
//! through a SOCKS 5 or HTTP proxy: SOCKS 5 (RFC 1928) with no
//! authentication or a user name and password (RFC 1929), and HTTP
//! `CONNECT` (RFC 9110, section 9.3.6) with or without Basic
//! authentication.
//!
//! It never does I/O. [`Client::start`] gives the first bytes to send;
//! then read exactly [`Client::wants`] bytes, [`Client::feed`] them, send
//! what it says to and repeat until it says the tunnel is open or why it
//! is not. Reading only what it wants leaves the SSH server's first bytes
//! in the socket; [`Step::Open`] still hands back any that were fed with
//! the reply.
//!
//! What the proxy sends is untrusted: everything is bounded (an HTTP
//! reply by [`MAX_HTTP_REPLY`]), and the proxy's own words only reach the
//! user cut to printable ASCII.

use std::net::IpAddr;

use zeroize::{Zeroize, Zeroizing};

/// The most bytes an HTTP proxy's reply to `CONNECT` may take, headers
/// included.
pub const MAX_HTTP_REPLY: usize = 16 * 1024;

/// How much of an HTTP proxy's status line an error message keeps.
const MAX_STATUS_TEXT: usize = 120;

/// Which protocol the proxy speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Socks5,
    Http,
}

impl Kind {
    /// The port such a proxy listens on when none is given.
    pub fn default_port(self) -> u16 {
        match self {
            Kind::Socks5 => 1080,
            Kind::Http => 8080,
        }
    }

    /// The URL scheme: `socks5` or `http`.
    pub fn scheme(self) -> &'static str {
        match self {
            Kind::Socks5 => "socks5",
            Kind::Http => "http",
        }
    }
}

/// What to do after [`Client::start`] or [`Client::feed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Send these bytes (possibly none), then read [`Client::wants`] more.
    Send(Vec<u8>),
    /// The tunnel is open: the SSH connection starts now. `rest` is what
    /// was fed after the proxy's reply, the server's first bytes.
    Open { rest: Vec<u8> },
    /// The proxy wants a user name and password, or refused the ones
    /// given.
    AuthFailed(String),
    /// Anything else that went wrong, worded for the user.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// SOCKS 5: waiting for the method the proxy chose.
    Method,
    /// SOCKS 5: waiting for the answer to the user name and password.
    Auth,
    /// SOCKS 5: waiting for the reply to `CONNECT`.
    Reply,
    /// HTTP: reading the reply up to the empty line.
    Http,
    Done,
}

/// One handshake with a proxy, to reach `host`:`port` through it.
pub struct Client {
    kind: Kind,
    host: String,
    port: u16,
    user: String,
    password: Zeroizing<String>,
    state: State,
    buf: Vec<u8>,
    /// HTTP: how far [`Client::buf`] was searched for the empty line.
    scanned: usize,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// Whether `host` can go in a request: a name or address without spaces
/// or control characters, which could otherwise add lines to an HTTP
/// request.
pub fn valid_host(host: &str) -> bool {
    !host.is_empty() && host.len() <= 255 && host.bytes().all(|b| b.is_ascii_graphic())
}

/// Printable ASCII of `bytes`, at most `max` characters, for messages.
fn printable(bytes: &[u8], max: usize) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .take(max)
        .collect()
}

/// Standard Base64 with padding, for `Proxy-Authorization: Basic`.
pub fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let six = (n >> (18 - 6 * i)) & 63;
                out.push(ABC.get(six as usize).copied().unwrap_or(b'A') as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// SOCKS 5 reply codes in words (RFC 1928, section 6).
fn socks_error(code: u8) -> &'static str {
    match code {
        1 => "a general failure",
        2 => "its rules do not allow this connection",
        3 => "the network is unreachable",
        4 => "the host is unreachable",
        5 => "the host refused the connection",
        6 => "the time to live ran out",
        7 => "it does not support CONNECT",
        8 => "it does not support this address type",
        _ => "an unknown error",
    }
}

impl Client {
    /// A handshake to reach `host`:`port` through a proxy of `kind`;
    /// `user` empty means no authentication. `None` when `host` could not
    /// be sent ([`valid_host`]) or, for SOCKS 5, the user name or password
    /// is longer than 255 bytes.
    pub fn new(kind: Kind, host: &str, port: u16, user: &str, password: &str) -> Option<Self> {
        if !valid_host(host) || port == 0 {
            return None;
        }
        if kind == Kind::Socks5 && (user.len() > 255 || password.len() > 255) {
            return None;
        }
        if kind == Kind::Http && user.contains(':') {
            // Basic authentication cannot carry a colon in the user name.
            return None;
        }
        if user.chars().any(char::is_control) {
            return None;
        }
        Some(Self {
            kind,
            host: host.to_owned(),
            port,
            user: user.to_owned(),
            password: Zeroizing::new(password.to_owned()),
            state: State::Done,
            buf: Vec::new(),
            scanned: 0,
        })
    }

    /// The first bytes to send.
    pub fn start(&mut self) -> Vec<u8> {
        self.buf.clear();
        self.scanned = 0;
        match self.kind {
            Kind::Socks5 => {
                self.state = State::Method;
                if self.user.is_empty() {
                    vec![5, 1, 0]
                } else {
                    // Offer both: a proxy that needs none picks none.
                    vec![5, 2, 0, 2]
                }
            }
            Kind::Http => {
                self.state = State::Http;
                let target = match self.host.parse::<IpAddr>() {
                    Ok(IpAddr::V6(a)) => format!("[{a}]:{}", self.port),
                    _ => format!("{}:{}", self.host, self.port),
                };
                let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
                if !self.user.is_empty() {
                    let mut pair = Vec::with_capacity(self.user.len() + self.password.len() + 1);
                    pair.extend_from_slice(self.user.as_bytes());
                    pair.push(b':');
                    pair.extend_from_slice(self.password.as_bytes());
                    req.push_str("Proxy-Authorization: Basic ");
                    req.push_str(&base64(&pair));
                    req.push_str("\r\n");
                    pair.zeroize();
                }
                req.push_str("\r\n");
                req.into_bytes()
            }
        }
    }

    /// How many more bytes to read before feeding: at least 1 while the
    /// handshake runs, 0 once it is over. Reading no more than this keeps
    /// the server's bytes out of the handshake.
    pub fn wants(&self) -> usize {
        let have = self.buf.len();
        let need = match self.state {
            State::Method | State::Auth => 2,
            State::Reply => match (self.buf.get(1), self.buf.get(3)) {
                // Not a success: the header says enough.
                (Some(&code), _) if code != 0 => 4,
                (_, None) => 4,
                (_, Some(1)) => 4 + 4 + 2,
                (_, Some(4)) => 4 + 16 + 2,
                (_, Some(3)) => match self.buf.get(4) {
                    None => 5,
                    Some(&len) => 5 + usize::from(len) + 2,
                },
                // An unknown address type: [`feed`] fails on it.
                (_, Some(_)) => 4,
            },
            // One byte at a time: the reply's end is the empty line.
            State::Http => have + 1,
            State::Done => have,
        };
        need.saturating_sub(have)
    }

    /// Takes bytes from the proxy. After [`Step::Open`],
    /// [`Step::AuthFailed`] or [`Step::Failed`] it is over and must not be
    /// fed again.
    pub fn feed(&mut self, data: &[u8]) -> Step {
        let mut sent = Vec::new();
        self.buf.extend_from_slice(data);
        loop {
            let step = match self.state {
                State::Done => return Step::Failed("The proxy handshake is over.".into()),
                State::Method => self.method(),
                State::Auth => self.auth(),
                State::Reply => self.reply(),
                State::Http => self.http(),
            };
            match step {
                // Need more bytes.
                None => return Step::Send(sent),
                Some(Step::Send(bytes)) => sent.extend(bytes),
                Some(done) => {
                    self.state = State::Done;
                    return done;
                }
            }
        }
    }

    /// The SOCKS 5 method choice.
    fn method(&mut self) -> Option<Step> {
        let [v, method] = *self.buf.get(..2)? else {
            return None;
        };
        if v != 5 {
            return Some(Step::Failed("The proxy does not speak SOCKS 5.".into()));
        }
        self.buf.drain(..2);
        match method {
            0 => Some(Step::Send(self.connect5())),
            2 if !self.user.is_empty() => {
                self.state = State::Auth;
                let mut out = Vec::with_capacity(3 + self.user.len() + self.password.len());
                out.push(1);
                out.push(self.user.len() as u8);
                out.extend_from_slice(self.user.as_bytes());
                out.push(self.password.len() as u8);
                out.extend_from_slice(self.password.as_bytes());
                Some(Step::Send(out))
            }
            0xff if self.user.is_empty() => Some(Step::AuthFailed(
                "The SOCKS proxy wants a user name and password.".into(),
            )),
            _ => Some(Step::Failed(
                "The SOCKS proxy offers no login method FeroSSH supports.".into(),
            )),
        }
    }

    /// The answer to the user name and password.
    fn auth(&mut self) -> Option<Step> {
        let [_, status] = *self.buf.get(..2)? else {
            return None;
        };
        self.buf.drain(..2);
        if status != 0 {
            return Some(Step::AuthFailed(
                "The SOCKS proxy refused the user name or password.".into(),
            ));
        }
        Some(Step::Send(self.connect5()))
    }

    /// The SOCKS 5 `CONNECT` request.
    fn connect5(&mut self) -> Vec<u8> {
        self.state = State::Reply;
        let mut out = vec![5, 1, 0];
        match self.host.parse::<IpAddr>() {
            Ok(IpAddr::V4(a)) => {
                out.push(1);
                out.extend_from_slice(&a.octets());
            }
            Ok(IpAddr::V6(a)) => {
                out.push(4);
                out.extend_from_slice(&a.octets());
            }
            // The proxy looks the name up, as `socks5h` does: the PC may
            // not be able to.
            Err(_) => {
                out.push(3);
                out.push(self.host.len() as u8);
                out.extend_from_slice(self.host.as_bytes());
            }
        }
        out.extend_from_slice(&self.port.to_be_bytes());
        out
    }

    /// The reply to `CONNECT`.
    fn reply(&mut self) -> Option<Step> {
        let [v, code, _, atyp] = *self.buf.get(..4)? else {
            return None;
        };
        if v != 5 {
            return Some(Step::Failed(
                "The SOCKS proxy sent something that is not a SOCKS 5 reply.".into(),
            ));
        }
        if code != 0 {
            return Some(Step::Failed(format!(
                "The SOCKS proxy could not connect to {}:{}: {}.",
                self.host,
                self.port,
                socks_error(code)
            )));
        }
        let len = match atyp {
            1 => 4 + 4 + 2,
            4 => 4 + 16 + 2,
            3 => 5 + usize::from(*self.buf.get(4)?) + 2,
            _ => {
                return Some(Step::Failed(
                    "The SOCKS proxy sent a reply with an unknown address type.".into(),
                ));
            }
        };
        if self.buf.len() < len {
            return None;
        }
        Some(Step::Open {
            rest: self.buf.split_off(len),
        })
    }

    /// The HTTP proxy's reply, once its headers ended.
    fn http(&mut self) -> Option<Step> {
        // Only the new bytes are searched, from a little before them.
        let from = self.scanned.saturating_sub(3);
        self.scanned = self.buf.len();
        let tail = self.buf.get(from..).unwrap_or_default();
        let end = tail.iter().enumerate().find_map(|(i, &b)| {
            let seen = tail.get(..=i)?;
            // CR LF CR LF, or bare line feeds, as some proxies send.
            (b == b'\n' && (seen.ends_with(b"\r\n\r\n") || seen.ends_with(b"\n\n")))
                .then_some(from + i + 1)
        });
        let Some(end) = end else {
            if self.buf.len() > MAX_HTTP_REPLY {
                return Some(Step::Failed("The HTTP proxy's reply is too long.".into()));
            }
            return None;
        };
        if end > MAX_HTTP_REPLY {
            return Some(Step::Failed("The HTTP proxy's reply is too long.".into()));
        }
        let rest = self.buf.split_off(end);
        let line_end = self
            .buf
            .iter()
            .position(|&b| b == b'\r' || b == b'\n')
            .unwrap_or(self.buf.len());
        let line = self.buf.get(..line_end).unwrap_or_default();
        let mut words = line.split(|&b| b == b' ').filter(|w| !w.is_empty());
        let version = words.next().unwrap_or_default();
        let code = words.next().unwrap_or_default();
        if !version.starts_with(b"HTTP/1.") || code.len() != 3 {
            return Some(Step::Failed(
                "The HTTP proxy sent something that is not an HTTP reply.".into(),
            ));
        }
        let text = printable(line, MAX_STATUS_TEXT);
        match code.first() {
            Some(b'2') => Some(Step::Open { rest }),
            _ if code == b"407" => Some(Step::AuthFailed(if self.user.is_empty() {
                format!("The HTTP proxy wants a user name and password ({text}).")
            } else {
                format!("The HTTP proxy refused the user name or password ({text}).")
            })),
            _ => Some(Step::Failed(format!(
                "The HTTP proxy could not connect to {}:{} ({text}).",
                self.host, self.port
            ))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// Runs `c` against the proxy's reply bytes, reading what it wants
    /// at a time; returns what it sent and how it ended.
    fn run(c: &mut Client, mut proxy: &[u8]) -> (Vec<u8>, Step) {
        let mut sent = c.start();
        loop {
            let n = c.wants().min(proxy.len());
            assert!(c.wants() > 0);
            if n == 0 {
                return (sent, Step::Send(Vec::new()));
            }
            let (now, later) = proxy.split_at(n);
            proxy = later;
            match c.feed(now) {
                Step::Send(b) => sent.extend(b),
                done => return (sent, done),
            }
        }
    }

    #[test]
    fn base64_matches_rfc_4648() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("Aladdin:open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        ] {
            assert_eq!(base64(plain.as_bytes()), coded);
        }
    }

    #[test]
    fn socks5_without_login_sends_the_name_for_the_proxy_to_look_up() {
        let mut c = Client::new(Kind::Socks5, "srv.example", 22, "", "").unwrap();
        let reply = [
            &[5, 0][..],
            &[5, 0, 0, 1, 10, 0, 0, 1, 0x1f, 0x90],
            b"SSH-2.0-x",
        ]
        .concat();
        let (sent, step) = run(&mut c, &reply);
        let mut want = vec![5, 1, 0, 5, 1, 0, 3, 11];
        want.extend_from_slice(b"srv.example");
        want.extend_from_slice(&[0, 22]);
        assert_eq!(sent, want);
        // Only what it wanted was read: the banner stays in the socket.
        assert_eq!(step, Step::Open { rest: Vec::new() });
    }

    #[test]
    fn socks5_with_login_and_addresses() {
        let mut c = Client::new(Kind::Socks5, "2001:db8::1", 2222, "me", "pw").unwrap();
        let mut reply = vec![5, 2, 1, 0, 5, 0, 0, 3, 3];
        reply.extend_from_slice(b"abc");
        reply.extend_from_slice(&[0, 1]);
        let (sent, step) = run(&mut c, &reply);
        let mut want = vec![5, 2, 0, 2, 1, 2, b'm', b'e', 2, b'p', b'w', 5, 1, 0, 4];
        want.extend_from_slice(
            &"2001:db8::1"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
                .octets(),
        );
        want.extend_from_slice(&2222u16.to_be_bytes());
        assert_eq!(sent, want);
        assert_eq!(step, Step::Open { rest: Vec::new() });

        let mut c = Client::new(Kind::Socks5, "192.0.2.7", 22, "", "").unwrap();
        let start = c.start();
        assert_eq!(start, [5, 1, 0]);
        assert_eq!(
            c.feed(&[5, 0]),
            Step::Send(vec![5, 1, 0, 1, 192, 0, 2, 7, 0, 22])
        );
        // All at once, with the server's first bytes after the reply.
        let step = c.feed(&[
            5, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b'S',
        ]);
        assert_eq!(
            step,
            Step::Open {
                rest: b"S".to_vec()
            }
        );
    }

    #[test]
    fn socks5_refusals_are_worded() {
        let mut c = Client::new(Kind::Socks5, "h", 22, "me", "bad").unwrap();
        let (_, step) = run(&mut c, &[5, 2, 1, 1]);
        assert!(
            matches!(step, Step::AuthFailed(ref t) if t.contains("refused")),
            "{step:?}"
        );

        let mut c = Client::new(Kind::Socks5, "h", 22, "", "").unwrap();
        let (_, step) = run(&mut c, &[5, 0xff]);
        assert!(matches!(step, Step::AuthFailed(_)), "{step:?}");

        let mut c = Client::new(Kind::Socks5, "h", 22, "", "").unwrap();
        let (_, step) = run(&mut c, &[5, 0, 5, 5, 0, 1]);
        assert_eq!(
            step,
            Step::Failed(
                "The SOCKS proxy could not connect to h:22: the host refused the connection."
                    .into()
            )
        );

        let mut c = Client::new(Kind::Socks5, "h", 22, "", "").unwrap();
        let (_, step) = run(&mut c, b"HTTP/1.1 400 Bad");
        assert!(matches!(step, Step::Failed(_)), "{step:?}");

        let mut c = Client::new(Kind::Socks5, "h", 22, "", "").unwrap();
        let (_, step) = run(&mut c, &[5, 0, 5, 0, 0, 9]);
        assert!(
            matches!(step, Step::Failed(ref t) if t.contains("address type")),
            "{step:?}"
        );
    }

    #[test]
    fn http_connect_with_basic_login() {
        let mut c = Client::new(Kind::Http, "srv.example", 22, "Aladdin", "open sesame").unwrap();
        let reply = b"HTTP/1.1 200 Connection established\r\nVia: x\r\n\r\nSSH-2.0-x";
        let (sent, step) = run(&mut c, reply);
        assert_eq!(
            String::from_utf8(sent).unwrap(),
            "CONNECT srv.example:22 HTTP/1.1\r\nHost: srv.example:22\r\n\
             Proxy-Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==\r\n\r\n"
        );
        assert_eq!(step, Step::Open { rest: Vec::new() });

        let mut c = Client::new(Kind::Http, "::1", 22, "", "").unwrap();
        assert!(
            String::from_utf8(c.start())
                .unwrap()
                .starts_with("CONNECT [::1]:22 HTTP/1.1\r\n")
        );
        // Bare line feeds, all at once, the banner after.
        assert_eq!(
            c.feed(b"HTTP/1.0 200 OK\n\nSSH-"),
            Step::Open {
                rest: b"SSH-".to_vec()
            }
        );
    }

    #[test]
    fn http_refusals_are_worded_and_bounded() {
        let mut c = Client::new(Kind::Http, "h", 22, "", "").unwrap();
        let (_, step) = run(
            &mut c,
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
        );
        assert!(
            matches!(step, Step::AuthFailed(ref t) if t.contains("wants")),
            "{step:?}"
        );

        let mut c = Client::new(Kind::Http, "h", 22, "u", "p").unwrap();
        let (_, step) = run(&mut c, b"HTTP/1.1 407 No\r\n\r\n");
        assert!(
            matches!(step, Step::AuthFailed(ref t) if t.contains("refused")),
            "{step:?}"
        );

        let mut c = Client::new(Kind::Http, "h", 22, "", "").unwrap();
        let (_, step) = run(&mut c, b"HTTP/1.1 403 Forbidden \x1b[31m\r\n\r\n");
        assert_eq!(
            step,
            Step::Failed(
                "The HTTP proxy could not connect to h:22 (HTTP/1.1 403 Forbidden ?[31m).".into()
            )
        );

        let mut c = Client::new(Kind::Http, "h", 22, "", "").unwrap();
        let (_, step) = run(&mut c, b"SSH-2.0-OpenSSH\r\n\r\n");
        assert!(
            matches!(step, Step::Failed(ref t) if t.contains("not an HTTP")),
            "{step:?}"
        );

        let mut c = Client::new(Kind::Http, "h", 22, "", "").unwrap();
        let _ = c.start();
        let mut step = Step::Send(Vec::new());
        for _ in 0..=MAX_HTTP_REPLY {
            step = c.feed(b"a");
            if step != Step::Send(Vec::new()) {
                break;
            }
        }
        assert!(
            matches!(step, Step::Failed(ref t) if t.contains("too long")),
            "{step:?}"
        );
    }

    #[test]
    fn hosts_and_users_that_could_inject_are_refused() {
        assert!(Client::new(Kind::Http, "h\r\nX: y", 22, "", "").is_none());
        assert!(Client::new(Kind::Http, "h x", 22, "", "").is_none());
        assert!(Client::new(Kind::Http, "", 22, "", "").is_none());
        assert!(Client::new(Kind::Http, "h", 0, "", "").is_none());
        assert!(Client::new(Kind::Http, "h", 22, "a:b", "").is_none());
        assert!(Client::new(Kind::Http, "h", 22, "a\nb", "").is_none());
        assert!(Client::new(Kind::Socks5, "h", 22, &"u".repeat(256), "").is_none());
        assert!(Client::new(Kind::Socks5, &"h".repeat(256), 22, "", "").is_none());
    }

    #[test]
    fn feeding_after_the_end_fails() {
        let mut c = Client::new(Kind::Socks5, "h", 22, "", "").unwrap();
        let _ = c.start();
        let _ = c.feed(&[5, 0xff]);
        assert_eq!(c.wants(), 0);
        assert!(matches!(c.feed(&[1]), Step::Failed(_)));
    }
}
