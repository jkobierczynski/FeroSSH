//! Telnet (RFC 854) for old network gear and console servers: the option
//! negotiation, and the session that runs it over TCP in the worker.
//!
//! Telnet has no encryption and no host authentication. It runs here, in
//! the sandboxed worker, for the same reason SSH does: everything a server
//! sends is parsed by a process that can reach nothing but the network.
//! [`Telnet`] is the protocol without the socket, so it is tested and
//! fuzzed on its own.
//!
//! FeroSSH agrees to a small set of options and refuses every other:
//!
//! - BINARY (0) in both directions, when the server asks;
//! - ECHO (1) by the server, so passwords do not show;
//! - SUPPRESS-GO-AHEAD (3) in both directions (character at a time);
//! - TERMINAL-TYPE (24), answered with `xterm-256color`;
//! - NAWS (31), the window size, sent again on every resize.
//!
//! It never offers NEW-ENVIRON or the old ENVIRON, so no user name or
//! environment variable leaves the PC unasked. Negotiation follows
//! RFC 1143: a request is answered only when it changes an option, so two
//! sides can never loop, and a server repeating itself gets nothing back.
//! Subnegotiations are capped in length; a longer one is skipped whole.
//!
//! ZMODEM runs in a Telnet shell as in an SSH one ([`crate::zmodem`]); its
//! bytes go out with [`Telnet::escape`], which changes nothing but IAC
//! and, outside BINARY, a bare CR.

use std::time::Duration;

use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::zmodem::{Zmodem, sleep_until};

/// Interpret As Command: starts every Telnet command.
pub const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
/// Subnegotiation begin and end.
const SB: u8 = 250;
const SE: u8 = 240;

const OPT_BINARY: u8 = 0;
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_TTYPE: u8 = 24;
const OPT_NAWS: u8 = 31;

/// TERMINAL-TYPE subnegotiation verbs (RFC 1091).
const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// The terminal type sent, the same as SSH sessions ask for.
pub const TERM: &str = "xterm-256color";

/// The longest subnegotiation kept. The only one FeroSSH reads is
/// TERMINAL-TYPE SEND (two bytes); anything much longer is skipped.
const MAX_SUBNEG: usize = 64;

/// Where the parser is in the byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parse {
    Data,
    /// After a CR in data: a NUL after it is padding (RFC 854).
    Cr,
    Iac,
    /// After IAC WILL, WONT, DO or DONT: the option byte comes next.
    Verb(u8),
    /// Inside IAC SB ... IAC SE.
    Sub,
    SubIac,
}

/// One side of one option, after RFC 1143's "Q method" (without the
/// queue bit: FeroSSH never changes its mind while a request is out).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Opt {
    #[default]
    No,
    Yes,
    /// We asked to enable it and wait for the answer.
    WantYes,
}

/// What the server sent, parsed: terminal bytes for the screen and the
/// Telnet answers to write back.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Fed {
    pub data: Vec<u8>,
    pub reply: Vec<u8>,
}

/// The Telnet protocol for one connection, without the socket.
#[derive(Debug)]
pub struct Telnet {
    parse: Parse,
    sub: Vec<u8>,
    /// The subnegotiation grew past [`MAX_SUBNEG`] and is skipped.
    sub_long: bool,
    /// Options this side performs (answers DO / DONT).
    us: [Opt; 256],
    /// Options the server performs (answers WILL / WONT).
    him: [Opt; 256],
    cols: u16,
    rows: u16,
}

impl Telnet {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            parse: Parse::Data,
            sub: Vec::new(),
            sub_long: false,
            us: [Opt::No; 256],
            him: [Opt::No; 256],
            cols,
            rows,
        }
    }

    /// What FeroSSH asks for when the connection opens, as PuTTY does:
    /// it will send the window size and terminal type, and asks the server
    /// to echo and to drop go-aheads.
    pub fn start(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for opt in [OPT_NAWS, OPT_TTYPE, OPT_SGA] {
            set(&mut self.us, opt, Opt::WantYes);
            out.extend_from_slice(&[IAC, WILL, opt]);
        }
        for opt in [OPT_ECHO, OPT_SGA] {
            set(&mut self.him, opt, Opt::WantYes);
            out.extend_from_slice(&[IAC, DO, opt]);
        }
        out
    }

    /// Whether the server echoes what is typed (it then hides passwords).
    pub fn remote_echo(&self) -> bool {
        get(&self.him, OPT_ECHO) == Opt::Yes
    }

    fn binary_out(&self) -> bool {
        get(&self.us, OPT_BINARY) == Opt::Yes
    }

    fn binary_in(&self) -> bool {
        get(&self.him, OPT_BINARY) == Opt::Yes
    }

    /// Parses bytes from the server.
    pub fn feed(&mut self, bytes: &[u8]) -> Fed {
        let mut fed = Fed::default();
        for &b in bytes {
            self.byte(b, &mut fed);
        }
        fed
    }

    fn byte(&mut self, b: u8, fed: &mut Fed) {
        match self.parse {
            Parse::Data | Parse::Cr => {
                let after_cr = self.parse == Parse::Cr;
                self.parse = Parse::Data;
                match b {
                    IAC => self.parse = Parse::Iac,
                    // CR NUL is a bare carriage return.
                    0 if after_cr => {}
                    b'\r' if !self.binary_in() => {
                        fed.data.push(b);
                        self.parse = Parse::Cr;
                    }
                    _ => fed.data.push(b),
                }
            }
            Parse::Iac => {
                self.parse = Parse::Data;
                match b {
                    IAC => fed.data.push(IAC),
                    WILL | WONT | DO | DONT => self.parse = Parse::Verb(b),
                    SB => {
                        self.sub.clear();
                        self.sub_long = false;
                        self.parse = Parse::Sub;
                    }
                    // NOP, GA, DM, BRK, IP, AO, AYT, EC, EL: nothing to do
                    // for a terminal.
                    _ => {}
                }
            }
            Parse::Verb(verb) => {
                self.parse = Parse::Data;
                self.negotiate(verb, b, &mut fed.reply);
            }
            Parse::Sub => match b {
                IAC => self.parse = Parse::SubIac,
                _ => self.sub_push(b),
            },
            Parse::SubIac => match b {
                SE => {
                    self.parse = Parse::Data;
                    if !self.sub_long {
                        self.subnegotiation(&mut fed.reply);
                    }
                    self.sub.clear();
                }
                IAC => {
                    self.parse = Parse::Sub;
                    self.sub_push(IAC);
                }
                // A command inside a subnegotiation ends it (RFC 855 has
                // the sender at fault); read the command as one.
                _ => {
                    self.sub.clear();
                    self.parse = Parse::Iac;
                    self.byte(b, fed);
                }
            },
        }
    }

    fn sub_push(&mut self, b: u8) {
        if self.sub.len() < MAX_SUBNEG {
            self.sub.push(b);
        } else {
            self.sub_long = true;
        }
    }

    fn subnegotiation(&self, reply: &mut Vec<u8>) {
        if self.sub.as_slice() == [OPT_TTYPE, TTYPE_SEND] && get(&self.us, OPT_TTYPE) == Opt::Yes {
            reply.extend_from_slice(&[IAC, SB, OPT_TTYPE, TTYPE_IS]);
            reply.extend_from_slice(TERM.as_bytes());
            reply.extend_from_slice(&[IAC, SE]);
        }
    }

    /// Whether this side agrees to perform `opt` when the server asks.
    fn we_will(opt: u8) -> bool {
        matches!(opt, OPT_BINARY | OPT_SGA | OPT_TTYPE | OPT_NAWS)
    }

    /// Whether this side lets the server perform `opt`.
    fn he_may(opt: u8) -> bool {
        matches!(opt, OPT_BINARY | OPT_ECHO | OPT_SGA)
    }

    fn negotiate(&mut self, verb: u8, opt: u8, reply: &mut Vec<u8>) {
        match verb {
            WILL => match get(&self.him, opt) {
                Opt::Yes => {}
                Opt::WantYes => set(&mut self.him, opt, Opt::Yes),
                Opt::No if Self::he_may(opt) => {
                    set(&mut self.him, opt, Opt::Yes);
                    reply.extend_from_slice(&[IAC, DO, opt]);
                }
                Opt::No => reply.extend_from_slice(&[IAC, DONT, opt]),
            },
            WONT => match get(&self.him, opt) {
                Opt::No => {}
                // A refusal of what we asked for: no answer.
                Opt::WantYes => set(&mut self.him, opt, Opt::No),
                Opt::Yes => {
                    set(&mut self.him, opt, Opt::No);
                    reply.extend_from_slice(&[IAC, DONT, opt]);
                }
            },
            DO => match get(&self.us, opt) {
                Opt::Yes => {}
                Opt::WantYes => {
                    set(&mut self.us, opt, Opt::Yes);
                    self.enabled(opt, reply);
                }
                Opt::No if Self::we_will(opt) => {
                    set(&mut self.us, opt, Opt::Yes);
                    reply.extend_from_slice(&[IAC, WILL, opt]);
                    self.enabled(opt, reply);
                }
                Opt::No => reply.extend_from_slice(&[IAC, WONT, opt]),
            },
            // DONT
            _ => match get(&self.us, opt) {
                Opt::No => {}
                Opt::WantYes => set(&mut self.us, opt, Opt::No),
                Opt::Yes => {
                    set(&mut self.us, opt, Opt::No);
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                }
            },
        }
    }

    /// This side now performs `opt`: NAWS sends the size straight away.
    fn enabled(&self, opt: u8, reply: &mut Vec<u8>) {
        if opt == OPT_NAWS {
            self.naws(reply);
        }
    }

    fn naws(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[IAC, SB, OPT_NAWS]);
        for half in [self.cols.to_be_bytes(), self.rows.to_be_bytes()] {
            for b in half {
                out.push(b);
                if b == IAC {
                    out.push(IAC);
                }
            }
        }
        out.extend_from_slice(&[IAC, SE]);
    }

    /// The terminal was resized: the NAWS message to send, if the server
    /// asked for sizes.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Vec<u8> {
        let changed = (cols, rows) != (self.cols, self.rows);
        self.cols = cols;
        self.rows = rows;
        let mut out = Vec::new();
        if changed && get(&self.us, OPT_NAWS) == Opt::Yes {
            self.naws(&mut out);
        }
        out
    }

    /// Bytes of a file transfer, made safe for the wire without changing
    /// them: IAC doubled, and outside BINARY a carriage return sent as
    /// CR NUL, which the other side reads back as a bare CR.
    pub fn escape(&self, bytes: &[u8]) -> Vec<u8> {
        let binary = self.binary_out();
        let mut out = Vec::with_capacity(bytes.len() + 8);
        for &b in bytes {
            match b {
                IAC => out.extend_from_slice(&[IAC, IAC]),
                b'\r' if !binary => out.extend_from_slice(&[b'\r', 0]),
                _ => out.push(b),
            }
        }
        out
    }

    /// Typed bytes, made safe for the wire: IAC doubled, and outside
    /// BINARY a carriage return sent as the Telnet newline CR LF (as
    /// PuTTY does), or as CR NUL when the terminal already sent CR LF.
    pub fn encode(&self, bytes: &[u8]) -> Vec<u8> {
        let binary = self.binary_out();
        let mut out = Vec::with_capacity(bytes.len() + 8);
        let mut it = bytes.iter().copied().peekable();
        while let Some(b) = it.next() {
            match b {
                IAC => out.extend_from_slice(&[IAC, IAC]),
                b'\r' if !binary => {
                    out.push(b'\r');
                    if it.peek() == Some(&b'\n') {
                        it.next();
                    }
                    out.push(b'\n');
                }
                _ => out.push(b),
            }
        }
        out
    }
}

fn get(table: &[Opt; 256], opt: u8) -> Opt {
    table.get(usize::from(opt)).copied().unwrap_or_default()
}

fn set(table: &mut [Opt; 256], opt: u8, value: Opt) {
    if let Some(o) = table.get_mut(usize::from(opt)) {
        *o = value;
    }
}

/// Where a Telnet session goes.
#[derive(Debug, Clone)]
pub struct Params {
    pub host: String,
    pub port: u16,
    pub cols: u16,
    pub rows: u16,
    pub connect_timeout: u32,
}

/// Bytes read from the socket at a time; below the IPC data chunk limit.
const READ_CHUNK: usize = 16 * 1024;

/// Runs one Telnet session for the UI's `channel` until either side
/// closes it.
pub async fn run(
    channel: ChannelId,
    params: Params,
    mut inbox: mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
) {
    let reason = session(channel, &params, &mut inbox, &out).await;
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}

async fn session(
    channel: ChannelId,
    params: &Params,
    inbox: &mut mpsc::Receiver<Message>,
    out: &mpsc::Sender<Message>,
) -> String {
    let timeout = Duration::from_secs(u64::from(params.connect_timeout.max(1)));
    let addr = (params.host.as_str(), params.port);
    let mut stream = tokio::select! {
        r = tokio::time::timeout(timeout, TcpStream::connect(addr)) => match r {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return format!("Could not connect: {e}."),
            Err(_) => return format!("No answer within {} s.", timeout.as_secs()),
        },
        // The UI closed the pane while connecting.
        _ = closed(inbox) => return "Closed.".to_owned(),
    };
    let _ = stream.set_nodelay(true);
    let mut telnet = Telnet::new(params.cols, params.rows);
    let hello = telnet.start();
    if let Err(e) = stream.write_all(&hello).await {
        return format!("The connection failed: {e}.");
    }
    let connected = Message::Connected {
        channel,
        kex: "telnet".into(),
        cipher: "none".into(),
        host_key: "none".into(),
    };
    if out.send(connected).await.is_err() {
        return String::new();
    }
    let mut buf = vec![0u8; READ_CHUNK];
    let mut zm = Zmodem::new(channel);
    let mut heard = Instant::now();
    loop {
        let wake = zm.patience().map(|p| heard + p);
        let step = tokio::select! {
            n = stream.read(&mut buf) => {
                let n = match n {
                    Ok(0) => return "The host closed the connection.".to_owned(),
                    Ok(n) => n,
                    Err(e) => return format!("The connection failed: {e}."),
                };
                heard = Instant::now();
                let fed = telnet.feed(buf.get(..n).unwrap_or_default());
                if !fed.reply.is_empty()
                    && let Err(e) = stream.write_all(&fed.reply).await
                {
                    return format!("The connection failed: {e}.");
                }
                zm.on_server(&fed.data)
            }
            msg = inbox.recv() => {
                if let Some(m) = &msg
                    && let Some(step) = zm.on_ui(m)
                {
                    heard = Instant::now();
                    step
                } else {
                    let wire = match msg {
                        Some(Message::Data { bytes, .. }) => telnet.encode(&bytes),
                        Some(Message::Resize { cols, rows, .. }) => telnet.resize(cols, rows),
                        Some(Message::Close { .. }) | None => return "Closed.".to_owned(),
                        // Nothing else belongs to a Telnet session.
                        Some(_) => continue,
                    };
                    if !wire.is_empty()
                        && let Err(e) = stream.write_all(&wire).await
                    {
                        return format!("The connection failed: {e}.");
                    }
                    continue;
                }
            }
            () = sleep_until(wake) => {
                heard = Instant::now();
                zm.timeout()
            }
        };
        for chunk in step.terminal.chunks(MAX_DATA_CHUNK) {
            let msg = Message::Data {
                channel,
                bytes: chunk.to_vec(),
            };
            if out.send(msg).await.is_err() {
                return String::new();
            }
        }
        for msg in step.ui {
            if out.send(msg).await.is_err() {
                return String::new();
            }
        }
        if !step.server.is_empty()
            && let Err(e) = stream.write_all(&telnet.escape(&step.server)).await
        {
            return format!("The connection failed: {e}.");
        }
    }
}

/// Waits until the UI closes the channel; other messages before the
/// connection is up are dropped.
async fn closed(inbox: &mut mpsc::Receiver<Message>) {
    loop {
        match inbox.recv().await {
            Some(Message::Close { .. }) | None => return,
            Some(_) => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn start_offers_size_type_and_asks_for_echo() {
        let mut t = Telnet::new(80, 24);
        assert_eq!(
            t.start(),
            [
                IAC, WILL, OPT_NAWS, IAC, WILL, OPT_TTYPE, IAC, WILL, OPT_SGA, IAC, DO, OPT_ECHO,
                IAC, DO, OPT_SGA
            ]
        );
        // The server agrees: the acks need no answer, except that NAWS
        // now sends the size.
        let fed = t.feed(&[IAC, DO, OPT_NAWS, IAC, WILL, OPT_ECHO, IAC, DO, OPT_SGA]);
        assert_eq!(fed.reply, [IAC, SB, OPT_NAWS, 0, 80, 0, 24, IAC, SE]);
        assert!(fed.data.is_empty());
        assert!(t.remote_echo());
        // Asked again: silence, so the two sides cannot loop.
        let fed = t.feed(&[IAC, DO, OPT_NAWS, IAC, WILL, OPT_ECHO]);
        assert!(fed.reply.is_empty());
    }

    #[test]
    fn refuses_options_it_does_not_know() {
        let mut t = Telnet::new(80, 24);
        // NEW-ENVIRON (39) and LINEMODE (34) are refused, every time.
        let fed = t.feed(&[IAC, DO, 39, IAC, WILL, 34]);
        assert_eq!(fed.reply, [IAC, WONT, 39, IAC, DONT, 34]);
        // A WONT/DONT for something already off is ignored.
        let fed = t.feed(&[IAC, DONT, 39, IAC, WONT, 34]);
        assert!(fed.reply.is_empty());
    }

    #[test]
    fn turning_off_is_acknowledged_once() {
        let mut t = Telnet::new(80, 24);
        t.feed(&[IAC, WILL, OPT_ECHO]);
        assert_eq!(t.feed(&[IAC, WONT, OPT_ECHO]).reply, [IAC, DONT, OPT_ECHO]);
        assert!(t.feed(&[IAC, WONT, OPT_ECHO]).reply.is_empty());
        assert!(!t.remote_echo());
    }

    #[test]
    fn answers_terminal_type() {
        let mut t = Telnet::new(80, 24);
        t.feed(&[IAC, DO, OPT_TTYPE]);
        let fed = t.feed(&[IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, SE]);
        let mut want = vec![IAC, SB, OPT_TTYPE, TTYPE_IS];
        want.extend_from_slice(TERM.as_bytes());
        want.extend_from_slice(&[IAC, SE]);
        assert_eq!(fed.reply, want);
        // Not before the option is on.
        let mut fresh = Telnet::new(80, 24);
        let fed = fresh.feed(&[IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, SE]);
        assert!(fed.reply.is_empty());
    }

    #[test]
    fn data_survives_commands_and_escapes() {
        let mut t = Telnet::new(80, 24);
        let fed = t.feed(b"ab\xff\xffc\xff\xf1d\r\0e\r\nf");
        assert_eq!(fed.data, b"ab\xffcd\re\r\nf");
        // Split anywhere, the same.
        let mut t = Telnet::new(80, 24);
        let mut data = Vec::new();
        for b in b"ab\xff\xffc\xff\xf1d\r\0e\r\nf" {
            data.extend(t.feed(&[*b]).data);
        }
        assert_eq!(data, b"ab\xffcd\re\r\nf");
    }

    #[test]
    fn long_subnegotiations_are_skipped_whole() {
        let mut t = Telnet::new(80, 24);
        t.feed(&[IAC, DO, OPT_TTYPE]);
        let mut wire = vec![IAC, SB, OPT_TTYPE, TTYPE_SEND];
        wire.extend(std::iter::repeat_n(b'x', 10_000));
        wire.extend_from_slice(&[IAC, SE]);
        wire.extend_from_slice(b"ok");
        let fed = t.feed(&wire);
        assert!(fed.reply.is_empty());
        assert_eq!(fed.data, b"ok");
        assert!(t.sub.len() <= MAX_SUBNEG);
    }

    #[test]
    fn size_escapes_iac_and_follows_resizes() {
        let mut t = Telnet::new(80, 24);
        // Not asked for sizes yet: nothing to send.
        assert!(t.resize(100, 30).is_empty());
        t.feed(&[IAC, DO, OPT_NAWS]);
        assert_eq!(
            t.resize(255, 40),
            [IAC, SB, OPT_NAWS, 0, IAC, IAC, 0, 40, IAC, SE]
        );
        assert!(t.resize(255, 40).is_empty());
    }

    #[test]
    fn typing_is_escaped_and_newlines_follow_the_mode() {
        let mut t = Telnet::new(80, 24);
        assert_eq!(t.encode(b"ls\r"), b"ls\r\n");
        assert_eq!(t.encode(b"a\r\nb"), b"a\r\nb");
        assert_eq!(t.encode(&[1, IAC, 2]), [1, IAC, IAC, 2]);
        // In BINARY, a CR is a CR.
        t.feed(&[IAC, DO, OPT_BINARY, IAC, WILL, OPT_BINARY]);
        assert_eq!(t.encode(b"ls\r"), b"ls\r");
        assert_eq!(t.feed(b"x\r\0").data, b"x\r\0");
    }

    #[tokio::test]
    async fn runs_over_tcp() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut hello = [0u8; 15];
            s.read_exact(&mut hello).await.unwrap();
            s.write_all(&[IAC, DO, OPT_NAWS]).await.unwrap();
            s.write_all(b"login: ").await.unwrap();
            let mut naws = [0u8; 9];
            s.read_exact(&mut naws).await.unwrap();
            let mut typed = [0u8; 6];
            s.read_exact(&mut typed).await.unwrap();
            (hello, naws, typed)
        });
        let (to_worker, inbox) = mpsc::channel(8);
        let (out, mut from_worker) = mpsc::channel(8);
        let params = Params {
            host: "127.0.0.1".into(),
            port,
            cols: 80,
            rows: 24,
            connect_timeout: 5,
        };
        let task = tokio::spawn(run(1, params, inbox, out));
        assert!(matches!(
            from_worker.recv().await,
            Some(Message::Connected { channel: 1, .. })
        ));
        let mut shown = Vec::new();
        while shown.len() < 7 {
            match from_worker.recv().await {
                Some(Message::Data { bytes, .. }) => shown.extend(bytes),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(shown, b"login: ");
        to_worker
            .send(Message::Data {
                channel: 1,
                bytes: b"root\r".to_vec(),
            })
            .await
            .unwrap();
        let (hello, naws, typed) = server.await.unwrap();
        assert_eq!(hello[..3], [IAC, WILL, OPT_NAWS]);
        assert_eq!(naws, [IAC, SB, OPT_NAWS, 0, 80, 0, 24, IAC, SE]);
        assert_eq!(&typed, b"root\r\n");
        assert!(matches!(
            from_worker.recv().await,
            Some(Message::Disconnected { channel: 1, .. })
        ));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_port_reports_why() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let (_to_worker, inbox) = mpsc::channel(8);
        let (out, mut from_worker) = mpsc::channel(8);
        let params = Params {
            host: "127.0.0.1".into(),
            port,
            cols: 80,
            rows: 24,
            connect_timeout: 5,
        };
        run(3, params, inbox, out).await;
        match from_worker.recv().await {
            Some(Message::Disconnected { channel: 3, reason }) => {
                assert!(reason.starts_with("Could not connect"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }
}
