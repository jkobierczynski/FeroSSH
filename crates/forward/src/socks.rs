//! The server side of a SOCKS handshake, for dynamic forwards: SOCKS 4,
//! SOCKS 4a (the client names a host instead of an address) and SOCKS 5
//! without authentication. Only CONNECT is supported; BIND and UDP
//! ASSOCIATE are refused, as OpenSSH does.
//!
//! It never does I/O: feed it what the client sent and it says what to
//! answer and, once the request is complete, where the client wants to go.
//! Everything is bounded: a handshake longer than [`MAX_HANDSHAKE`] bytes
//! is refused.

/// The most bytes a handshake may take before the request is complete.
pub const MAX_HANDSHAKE: usize = 1024;

/// SOCKS 5 reply codes (RFC 1928, section 6).
pub mod reply {
    pub const SUCCEEDED: u8 = 0;
    pub const GENERAL_FAILURE: u8 = 1;
    pub const HOST_UNREACHABLE: u8 = 4;
    pub const COMMAND_NOT_SUPPORTED: u8 = 7;
    pub const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 8;
}

/// The protocol version the client spoke, for the final reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V4,
    V5,
}

/// What to do after [`Handshake::feed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Send `reply` (it may be empty) and wait for more bytes.
    More { reply: Vec<u8> },
    /// The request is complete: connect to `host`:`port`, then send
    /// [`Handshake::finish`]'s reply. `rest` is what the client sent after
    /// the request, to pass on once connected.
    Connect {
        host: String,
        port: u16,
        rest: Vec<u8>,
    },
    /// Send `reply` and close: the client asked for something unsupported,
    /// or spoke something that is not SOCKS.
    Refuse { reply: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    /// SOCKS 5: the method was chosen, waiting for the request.
    Request5,
    Done(Version),
}

/// One client's handshake.
#[derive(Debug)]
pub struct Handshake {
    state: State,
    buf: Vec<u8>,
}

impl Default for Handshake {
    fn default() -> Self {
        Self::new()
    }
}

/// What parsing the buffered bytes found.
enum Parsed {
    /// Need more bytes.
    Short,
    /// The message took this many bytes.
    Took(usize, Step),
}

fn take<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at + N)?.try_into().ok()
}

/// The bytes up to a zero, and the index after the zero.
fn until_nul(b: &[u8], from: usize) -> Option<(&[u8], usize)> {
    let tail = b.get(from..)?;
    let end = tail.iter().position(|&c| c == 0)?;
    Some((tail.get(..end)?, from + end + 1))
}

/// A host name a client may ask for: printable ASCII without spaces, so
/// that it cannot smuggle anything into the server's request or a log.
fn host_name(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > 255 || !bytes.iter().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

fn v4_reply(ok: bool) -> Vec<u8> {
    vec![0, if ok { 0x5a } else { 0x5b }, 0, 0, 0, 0, 0, 0]
}

fn v5_reply(code: u8) -> Vec<u8> {
    vec![5, code, 0, 1, 0, 0, 0, 0, 0, 0]
}

impl Handshake {
    pub fn new() -> Self {
        Self {
            state: State::Start,
            buf: Vec::new(),
        }
    }

    /// Takes bytes from the client. After [`Step::Connect`] or
    /// [`Step::Refuse`] the handshake is over and must not be fed again.
    pub fn feed(&mut self, data: &[u8]) -> Step {
        if matches!(self.state, State::Done(_)) {
            return Step::Refuse { reply: Vec::new() };
        }
        self.buf.extend_from_slice(data);
        let mut replies = Vec::new();
        loop {
            let parsed = match self.state {
                State::Start => self.start(),
                State::Request5 => self.request5(),
                State::Done(_) => Parsed::Short,
            };
            match parsed {
                Parsed::Short => break,
                Parsed::Took(n, step) => {
                    self.buf.drain(..n.min(self.buf.len()));
                    match step {
                        Step::More { reply } => replies.extend(reply),
                        Step::Connect { host, port, .. } => {
                            return Step::Connect {
                                host,
                                port,
                                rest: std::mem::take(&mut self.buf),
                            };
                        }
                        Step::Refuse { reply } => {
                            replies.extend(reply);
                            self.state = State::Done(Version::V5);
                            return Step::Refuse { reply: replies };
                        }
                    }
                }
            }
        }
        if self.buf.len() > MAX_HANDSHAKE {
            self.state = State::Done(Version::V5);
            return Step::Refuse { reply: replies };
        }
        Step::More { reply: replies }
    }

    /// The reply to send once the connection to the target succeeded or
    /// failed (with a SOCKS 5 reply code from [`reply`]).
    pub fn finish(&self, code: u8) -> Vec<u8> {
        match self.state {
            State::Done(Version::V4) => v4_reply(code == reply::SUCCEEDED),
            _ => v5_reply(code),
        }
    }

    fn start(&mut self) -> Parsed {
        match self.buf.first() {
            None => Parsed::Short,
            Some(4) => self.request4(),
            Some(5) => {
                let Some(&n) = self.buf.get(1) else {
                    return Parsed::Short;
                };
                let n = usize::from(n);
                let Some(methods) = self.buf.get(2..2 + n) else {
                    return Parsed::Short;
                };
                // Only "no authentication": the listener is the user's own.
                if methods.contains(&0) {
                    self.state = State::Request5;
                    Parsed::Took(2 + n, Step::More { reply: vec![5, 0] })
                } else {
                    Parsed::Took(
                        2 + n,
                        Step::Refuse {
                            reply: vec![5, 0xff],
                        },
                    )
                }
            }
            Some(_) => Parsed::Took(0, Step::Refuse { reply: Vec::new() }),
        }
    }

    fn request4(&mut self) -> Parsed {
        let b = &self.buf;
        let (Some([_, cmd]), Some(port), Some(ip)) =
            (take::<2>(b, 0), take::<2>(b, 2), take::<4>(b, 4))
        else {
            return Parsed::Short;
        };
        // The user id, which is ignored.
        let Some((_, after_user)) = until_nul(b, 8) else {
            return Parsed::Short;
        };
        let port = u16::from_be_bytes(port);
        let (host, end) = if matches!(ip, [0, 0, 0, last] if last != 0) {
            // SOCKS 4a: the host name follows.
            let Some((name, end)) = until_nul(b, after_user) else {
                return Parsed::Short;
            };
            match host_name(name) {
                Some(h) => (h, end),
                None => {
                    return Parsed::Took(
                        end,
                        Step::Refuse {
                            reply: v4_reply(false),
                        },
                    );
                }
            }
        } else {
            (std::net::Ipv4Addr::from(ip).to_string(), after_user)
        };
        if cmd != 1 || port == 0 {
            return Parsed::Took(
                end,
                Step::Refuse {
                    reply: v4_reply(false),
                },
            );
        }
        self.state = State::Done(Version::V4);
        Parsed::Took(
            end,
            Step::Connect {
                host,
                port,
                rest: Vec::new(),
            },
        )
    }

    fn request5(&mut self) -> Parsed {
        let b = &self.buf;
        let Some([ver, cmd, _, atyp]) = take::<4>(b, 0) else {
            return Parsed::Short;
        };
        if ver != 5 {
            return Parsed::Took(
                0,
                Step::Refuse {
                    reply: v5_reply(reply::GENERAL_FAILURE),
                },
            );
        }
        let (host, at) = match atyp {
            1 => match take::<4>(b, 4) {
                Some(ip) => (Some(std::net::Ipv4Addr::from(ip).to_string()), 8),
                None => return Parsed::Short,
            },
            3 => {
                let Some(&len) = b.get(4) else {
                    return Parsed::Short;
                };
                let len = usize::from(len);
                match b.get(5..5 + len) {
                    Some(name) => (host_name(name), 5 + len),
                    None => return Parsed::Short,
                }
            }
            4 => match take::<16>(b, 4) {
                Some(ip) => (Some(std::net::Ipv6Addr::from(ip).to_string()), 20),
                None => return Parsed::Short,
            },
            _ => {
                return Parsed::Took(
                    b.len(),
                    Step::Refuse {
                        reply: v5_reply(reply::ADDRESS_TYPE_NOT_SUPPORTED),
                    },
                );
            }
        };
        let Some(port) = take::<2>(b, at) else {
            return Parsed::Short;
        };
        let end = at + 2;
        let port = u16::from_be_bytes(port);
        let refuse = |code| {
            Parsed::Took(
                end,
                Step::Refuse {
                    reply: v5_reply(code),
                },
            )
        };
        if cmd != 1 {
            return refuse(reply::COMMAND_NOT_SUPPORTED);
        }
        let Some(host) = host.filter(|_| port != 0) else {
            return refuse(reply::HOST_UNREACHABLE);
        };
        self.state = State::Done(Version::V5);
        Parsed::Took(
            end,
            Step::Connect {
                host,
                port,
                rest: Vec::new(),
            },
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn connect(step: Step) -> (String, u16, Vec<u8>) {
        match step {
            Step::Connect { host, port, rest } => (host, port, rest),
            other => panic!("not a connect: {other:?}"),
        }
    }

    #[test]
    fn socks5_domain_with_early_data() {
        let mut h = Handshake::new();
        assert_eq!(h.feed(&[5, 2, 2, 0]), Step::More { reply: vec![5, 0] });
        let mut req = vec![5, 1, 0, 3, 10];
        req.extend_from_slice(b"github.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        req.extend_from_slice(b"GET");
        let (host, port, rest) = connect(h.feed(&req));
        assert_eq!(
            (host.as_str(), port, rest.as_slice()),
            ("github.com", 443, &b"GET"[..])
        );
        assert_eq!(h.finish(reply::SUCCEEDED), [5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn socks5_greeting_and_request_together_byte_by_byte() {
        let mut wire = vec![5, 1, 0, 5, 1, 0, 4];
        wire.extend_from_slice(&[0; 15]);
        wire.push(1);
        wire.extend_from_slice(&22u16.to_be_bytes());
        let mut h = Handshake::new();
        let mut replies = Vec::new();
        let mut done = None;
        for b in wire {
            match h.feed(&[b]) {
                Step::More { reply } => replies.extend(reply),
                step => done = Some(step),
            }
        }
        assert_eq!(replies, [5, 0]);
        let (host, port, _) = connect(done.unwrap());
        assert_eq!((host.as_str(), port), ("::1", 22));
    }

    #[test]
    fn socks5_refusals() {
        let mut h = Handshake::new();
        assert_eq!(
            h.feed(&[5, 1, 2]),
            Step::Refuse {
                reply: vec![5, 0xff]
            }
        );
        let mut h = Handshake::new();
        h.feed(&[5, 1, 0]);
        // BIND.
        let step = h.feed(&[5, 2, 0, 1, 1, 2, 3, 4, 0, 80]);
        assert_eq!(
            step,
            Step::Refuse {
                reply: v5_reply(reply::COMMAND_NOT_SUPPORTED)
            }
        );
        let mut h = Handshake::new();
        h.feed(&[5, 1, 0]);
        assert_eq!(
            h.feed(&[5, 1, 0, 9]),
            Step::Refuse {
                reply: v5_reply(reply::ADDRESS_TYPE_NOT_SUPPORTED)
            }
        );
        // A host name with a space or a control character.
        let mut h = Handshake::new();
        h.feed(&[5, 1, 0]);
        let step = h.feed(&[5, 1, 0, 3, 3, b'a', b' ', b'b', 0, 80]);
        assert!(matches!(step, Step::Refuse { .. }));
        // Not SOCKS at all.
        assert!(matches!(
            Handshake::new().feed(b"GET / HTTP/1.1"),
            Step::Refuse { .. }
        ));
    }

    #[test]
    fn socks4_and_4a() {
        let mut h = Handshake::new();
        let (host, port, rest) = connect(h.feed(&[4, 1, 0, 80, 10, 0, 0, 1, b'u', 0, b'x']));
        assert_eq!(
            (host.as_str(), port, rest.as_slice()),
            ("10.0.0.1", 80, &b"x"[..])
        );
        assert_eq!(h.finish(reply::SUCCEEDED), [0, 0x5a, 0, 0, 0, 0, 0, 0]);
        assert_eq!(h.finish(reply::HOST_UNREACHABLE)[1], 0x5b);

        let mut h = Handshake::new();
        let mut req = vec![4, 1, 1, 187, 0, 0, 0, 7, 0];
        req.extend_from_slice(b"intranet.lan\0");
        let (host, port, _) = connect(h.feed(&req));
        assert_eq!((host.as_str(), port), ("intranet.lan", 443));

        let mut h = Handshake::new();
        assert_eq!(
            h.feed(&[4, 2, 0, 80, 1, 2, 3, 4, 0]),
            Step::Refuse {
                reply: v4_reply(false)
            }
        );
    }

    #[test]
    fn endless_user_ids_are_refused() {
        let mut h = Handshake::new();
        h.feed(&[4, 1, 0, 80, 1, 2, 3, 4]);
        let step = h.feed(&[b'a'; MAX_HANDSHAKE + 1]);
        assert!(matches!(step, Step::Refuse { .. }));
        assert!(matches!(h.feed(&[0]), Step::Refuse { .. }));
    }
}
