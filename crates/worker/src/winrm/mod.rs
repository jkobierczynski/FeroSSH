//! WinRM sessions: a Windows Remote Shell (WS-Management over HTTP) that
//! runs one command, PowerShell or cmd, with its input and output relayed
//! to the pane.
//!
//! Everything the server sends (HTTP, the encryption framing, SOAP and
//! XML, the output) is parsed here, in the sandboxed worker. The worker
//! never sees the user's credentials: the UI holds them and the security
//! contexts (Negotiate: Kerberos or NTLM, through SSPI), and the worker
//! asks it for each login step and to seal and unseal each message
//! ([`Message::SspiRequest`]). Nothing goes out or is believed
//! unencrypted: a reply that is not sealed is refused.
//!
//! WinRS has no terminal: the command's input and output are plain
//! streams. A Receive waits on the server for output, so typed input
//! goes over a second HTTP connection of the session's own, with its own
//! login and security context.

mod http;
pub mod sealed;
pub mod soap;
pub mod xml;

use std::collections::VecDeque;
use std::time::Duration;

use base64ct::{Base64, Encoding};
use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message, SspiOp, SspiResult};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use self::soap::{Fault, Header};
use self::xml::Element;

/// The connection Receive runs on; input and signals go on [`INPUT`].
const OUTPUT: u8 = 0;
const INPUT: u8 = 1;
/// The most input sent in one Send, so its envelope stays well under
/// the server's limit once in base64.
const SEND_CHUNK: usize = 32 * 1024;
/// How long a reply may take beyond what the request asked the server
/// to wait.
const REPLY_GRACE: Duration = Duration::from_secs(40);

pub(crate) fn base64_encode(data: &[u8]) -> String {
    Base64::encode_string(data)
}

pub(crate) fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let clean: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    Base64::decode_vec(&clean).ok()
}

#[derive(Debug, Clone)]
pub struct Params {
    pub host: String,
    pub port: u16,
    pub connect_timeout: u32,
    pub command: String,
}

impl Params {
    /// `host:port`, bracketed for IPv6, as the `Host` header and the
    /// endpoint name it.
    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Aborts a task when dropped, so a session's helpers end with it.
struct Task<T>(JoinHandle<T>);

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs a WinRM session until the command ends, the UI closes it or the
/// server goes away; always ends with [`Message::Disconnected`].
pub async fn run(
    channel: ChannelId,
    params: Params,
    inbox: mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
) {
    // SSPI answers go to the connection that asked; the rest is input.
    let (sspi_tx, sspi_rx) = (mpsc::channel(4), mpsc::channel(4));
    let (input_tx, mut input) = mpsc::channel::<Message>(64);
    let sspi = [sspi_tx.0, sspi_rx.0];
    let _demux = Task(tokio::spawn(demux(channel, inbox, sspi, input_tx)));
    let replies = [sspi_tx.1, sspi_rx.1];
    let reason = session(channel, &params, replies, &mut input, &out).await;
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}

/// The most typed input held while the session is busy sending earlier
/// input; past it the session is closed rather than grow without bound.
const MAX_PENDING: usize = 32 * 1024 * 1024;

/// Splits the UI's messages: SSPI answers to the connection that asked,
/// the rest to the session in order. Input waits in a queue of its own
/// so a session busy sending (and waiting on SSPI to seal it) never
/// keeps an SSPI answer from getting through.
async fn demux(
    channel: ChannelId,
    mut inbox: mpsc::Receiver<Message>,
    sspi: [mpsc::Sender<SspiResult>; 2],
    input_tx: mpsc::Sender<Message>,
) {
    let mut pending: VecDeque<Message> = VecDeque::new();
    let mut held = 0usize;
    let mut overflowed = false;
    loop {
        tokio::select! {
            msg = inbox.recv() => {
                let Some(msg) = msg else { break };
                match msg {
                    Message::SspiReply { conn, result, .. } => {
                        if let Some(tx) = sspi.get(usize::from(conn))
                            && tx.send(result).await.is_err()
                        {
                            break;
                        }
                    }
                    _ if overflowed => {}
                    other => {
                        held = held.saturating_add(input_size(&other));
                        pending.push_back(other);
                        if held > MAX_PENDING {
                            // Too much typed input waiting: end the
                            // session, which still ends the shell on the
                            // server through the SSPI answers above.
                            overflowed = true;
                            pending.clear();
                            held = 0;
                            pending.push_back(Message::Close { channel });
                        }
                    }
                }
            }
            permit = input_tx.reserve(), if !pending.is_empty() => {
                let Ok(permit) = permit else { break };
                if let Some(msg) = pending.pop_front() {
                    held = held.saturating_sub(input_size(&msg));
                    permit.send(msg);
                }
            }
        }
    }
}

fn input_size(msg: &Message) -> usize {
    match msg {
        Message::Data { bytes, .. } => bytes.len(),
        _ => 0,
    }
}

/// Waits for the UI to close the session; input meanwhile is dropped.
async fn closed(input: &mut mpsc::Receiver<Message>) {
    while let Some(msg) = input.recv().await {
        if matches!(msg, Message::Close { .. }) {
            return;
        }
    }
}

async fn session(
    channel: ChannelId,
    params: &Params,
    replies: [mpsc::Receiver<SspiResult>; 2],
    input: &mut mpsc::Receiver<Message>,
    out: &mpsc::Sender<Message>,
) -> String {
    let [replies_out, replies_in] = replies;
    let setup = async {
        let mut main = Conn::open(channel, OUTPUT, params, replies_out, out).await?;
        let endpoint = main.endpoint.clone();
        let shell = main
            .call(&soap::create_shell(&header(&endpoint, None)))
            .await
            .map_err(|e| e.say("open a shell"))?;
        let shell = soap::shell_id(&shell).ok_or("The server did not say which shell it made.")?;
        let started = main
            .call(&soap::command(
                &header(&endpoint, Some(&shell)),
                &params.command,
            ))
            .await
            .map_err(|e| e.say("start the command"))?;
        let command = soap::command_id(&started).ok_or("The server did not start the command.")?;
        let typing = Conn::open(channel, INPUT, params, replies_in, out).await?;
        Ok::<_, String>((main, typing, shell, command, endpoint))
    };
    let (main, mut typing, shell, command, endpoint) = tokio::select! {
        r = setup => match r {
            Ok(s) => s,
            Err(e) => return e,
        },
        () = closed(input) => return "Closed.".to_owned(),
    };
    let connected = Message::Connected {
        channel,
        kex: "WinRM".into(),
        cipher: "SSPI".into(),
        host_key: "none".into(),
    };
    if out.send(connected).await.is_err() {
        return String::new();
    }
    let mut output = Task(tokio::spawn(relay_output(
        channel,
        main,
        shell.clone(),
        command.clone(),
        out.clone(),
    )));
    loop {
        tokio::select! {
            ended = &mut output.0 => {
                break ended.unwrap_or_else(|_| "The session ended.".to_owned());
            }
            msg = input.recv() => match msg {
                Some(Message::Data { bytes, .. }) => {
                    let h = header(&endpoint, Some(&shell));
                    let sent = if bytes == [0x03] {
                        typing.call(&soap::signal(&h, &command, soap::CTRL_C)).await.map(drop)
                    } else {
                        let mut r = Ok(());
                        for chunk in bytes.chunks(SEND_CHUNK) {
                            r = typing.call(&soap::send(&h, &command, chunk, false)).await.map(drop);
                            if r.is_err() {
                                break;
                            }
                        }
                        r
                    };
                    if let Err(e) = sent {
                        break e.say("send the input");
                    }
                }
                Some(Message::Close { .. }) | None => {
                    // Best effort: end the command and the shell on the
                    // server rather than leave them to its idle timeout.
                    let h = header(&endpoint, Some(&shell));
                    let tidy = async {
                        let _ = typing.call(&soap::signal(&h, &command, soap::TERMINATE)).await;
                        let _ = typing.call(&soap::delete_shell(&h)).await;
                    };
                    let _ = tokio::time::timeout(Duration::from_secs(5), tidy).await;
                    break "Closed.".to_owned();
                }
                // Nothing else belongs to a WinRM session.
                Some(_) => {}
            }
        }
    }
}

/// A request header for endpoint `to`; [`Conn::call`] gives it its
/// message id.
fn header<'a>(to: &'a str, shell: Option<&'a str>) -> Header<'a> {
    Header {
        to,
        message_id: "",
        shell,
    }
}

/// Receives the command's output until it ends; returns why the session
/// ends.
async fn relay_output(
    channel: ChannelId,
    mut conn: Conn,
    shell: String,
    command: String,
    out: mpsc::Sender<Message>,
) -> String {
    let endpoint = conn.endpoint.clone();
    loop {
        let h = header(&endpoint, Some(&shell));
        let reply = match conn.call(&soap::receive(&h, &command)).await {
            Ok(r) => r,
            Err(CallError::Fault(f)) if f.timed_out() => continue,
            Err(e) => return e.say("read the output"),
        };
        let got = match soap::received(&reply, &command) {
            Ok(g) => g,
            Err(e) => return format!("The server sent bad output ({e})."),
        };
        for chunk in got.stdout.chunks(MAX_DATA_CHUNK) {
            let msg = Message::Data {
                channel,
                bytes: chunk.to_vec(),
            };
            if out.send(msg).await.is_err() {
                return String::new();
            }
        }
        for chunk in got.stderr.chunks(MAX_DATA_CHUNK) {
            let msg = Message::ExecStderr {
                channel,
                bytes: chunk.to_vec(),
            };
            if out.send(msg).await.is_err() {
                return String::new();
            }
        }
        if let Some(code) = got.exit {
            // Exit codes are 32-bit on Windows; negative ones come as
            // their two's complement.
            let code = code as u32;
            let _ = out.send(Message::ExitStatus { channel, code }).await;
            let h = header(&endpoint, Some(&shell));
            let _ =
                tokio::time::timeout(Duration::from_secs(5), conn.call(&soap::delete_shell(&h)))
                    .await;
            return format!("The command ended (exit code {code}).");
        }
    }
}

/// Why a call failed.
#[derive(Debug)]
enum CallError {
    Fault(Fault),
    Other(String),
}

impl CallError {
    /// The reason shown to the user, for a step `doing` something.
    fn say(self, doing: &str) -> String {
        match self {
            CallError::Fault(f) => format!("The server refused to {doing}: {}", f.text),
            CallError::Other(e) => e,
        }
    }
}

impl From<String> for CallError {
    fn from(e: String) -> Self {
        CallError::Other(e)
    }
}

/// One logged-in HTTP connection to the server.
struct Conn {
    channel: ChannelId,
    conn: u8,
    stream: TcpStream,
    buf: Vec<u8>,
    authority: String,
    endpoint: String,
    out: mpsc::Sender<Message>,
    replies: mpsc::Receiver<SspiResult>,
    connect_timeout: Duration,
}

impl Conn {
    /// Connects and logs in.
    async fn open(
        channel: ChannelId,
        conn: u8,
        params: &Params,
        replies: mpsc::Receiver<SspiResult>,
        out: &mpsc::Sender<Message>,
    ) -> Result<Self, String> {
        let timeout = Duration::from_secs(u64::from(params.connect_timeout.max(1)));
        let addr = (params.host.as_str(), params.port);
        let stream = match tokio::time::timeout(timeout, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(format!("Could not connect: {e}.")),
            Err(_) => return Err(format!("No answer within {} s.", timeout.as_secs())),
        };
        let _ = stream.set_nodelay(true);
        let authority = params.authority();
        let mut c = Self {
            channel,
            conn,
            stream,
            buf: Vec::new(),
            endpoint: format!("http://{authority}/wsman"),
            authority,
            out: out.clone(),
            replies,
            connect_timeout: timeout,
        };
        c.login().await?;
        Ok(c)
    }

    /// Asks the UI for an SSPI step.
    async fn sspi(&mut self, op: SspiOp) -> Result<SspiResult, String> {
        let msg = Message::SspiRequest {
            channel: self.channel,
            conn: self.conn,
            op,
        };
        self.out
            .send(msg)
            .await
            .map_err(|_| "The window went away.".to_owned())?;
        match self.replies.recv().await {
            Some(SspiResult::Failed(why)) => Err(why),
            Some(r) => Ok(r),
            None => Err("Closed.".to_owned()),
        }
    }

    /// Sends one request and reads its response, within `wait`.
    async fn exchange(
        &mut self,
        headers: &[(&str, &str)],
        body: &[u8],
        wait: Duration,
    ) -> Result<http::Response, String> {
        let req = http::post(&self.authority, "/wsman", headers, body);
        let exchange = async {
            self.stream
                .write_all(&req)
                .await
                .map_err(|e| format!("The connection failed: {e}."))?;
            let mut chunk = vec![0u8; 16 * 1024];
            loop {
                if let Some(r) = http::take_response(&mut self.buf).map_err(|e| format!("{e}."))? {
                    return Ok(r);
                }
                let n = self
                    .stream
                    .read(&mut chunk)
                    .await
                    .map_err(|e| format!("The connection failed: {e}."))?;
                if n == 0 {
                    return Err("The server closed the connection.".to_owned());
                }
                self.buf
                    .extend_from_slice(chunk.get(..n).unwrap_or_default());
            }
        };
        match tokio::time::timeout(wait, exchange).await {
            Ok(r) => r,
            Err(_) => Err(format!(
                "The server did not answer within {} s.",
                wait.as_secs()
            )),
        }
    }

    /// The Negotiate login (SPNEGO over HTTP, RFC 4559), with empty
    /// requests, so that every request after it can be sealed.
    async fn login(&mut self) -> Result<(), String> {
        let mut step = self.sspi(SspiOp::Start).await?;
        // A Kerberos exchange takes two rounds at most, NTLM's two;
        // anything longer is a confused server.
        for _ in 0..6 {
            let (token, done) = match step {
                SspiResult::Continue(t) => (t, false),
                SspiResult::Done(t) => (t, true),
                _ => return Err("The login went wrong.".to_owned()),
            };
            if done && token.is_empty() {
                return Ok(());
            }
            let auth = format!("Negotiate {}", base64_encode(&token));
            let resp = self
                .exchange(
                    &[
                        ("Authorization", &auth),
                        ("Content-Type", "application/soap+xml;charset=UTF-8"),
                    ],
                    &[],
                    self.connect_timeout + REPLY_GRACE,
                )
                .await?;
            let server = resp.negotiate_token().map_err(|e| format!("{e}."))?;
            match (resp.status, server) {
                (200, None) if done => return Ok(()),
                (200, Some(t)) if done && t.is_empty() => return Ok(()),
                // The server's last token: mutual authentication.
                (200, Some(t)) if !t.is_empty() => {
                    return match self.sspi(SspiOp::Step(t)).await? {
                        SspiResult::Done(last) if last.is_empty() => Ok(()),
                        _ => Err("The server's login answer did not check out.".to_owned()),
                    };
                }
                (200, _) => {
                    return Err("The server let FeroSSH in without proving who it is.".to_owned());
                }
                (401, Some(t)) if !t.is_empty() && !done => {
                    step = self.sspi(SspiOp::Step(t)).await?;
                }
                (401, None) => {
                    return Err(
                        "The server does not take Negotiate (Kerberos or NTLM) logins.".to_owned(),
                    );
                }
                (401, _) => return Err("Access denied: the server refused the login.".to_owned()),
                (status, _) => {
                    return Err(format!("The server answered the login with HTTP {status}."));
                }
            }
        }
        Err("The login took too many rounds.".to_owned())
    }

    /// Sends one sealed request and returns its unsealed reply, or the
    /// fault in it.
    async fn call(&mut self, envelope: &str) -> Result<Element, CallError> {
        let id = message_id();
        let envelope = envelope.replacen(
            "<a:MessageID>uuid:</a:MessageID>",
            &format!("<a:MessageID>uuid:{id}</a:MessageID>"),
            1,
        );
        let plain = envelope.into_bytes();
        let plain_len = plain.len();
        let (header, data) = match self.sspi(SspiOp::Seal(plain)).await? {
            SspiResult::Sealed { header, data } => (header, data),
            _ => return Err("Sealing a message went wrong.".to_owned().into()),
        };
        let body = sealed::frame(plain_len, &header, &data);
        let wait = Duration::from_secs(u64::from(soap::RECEIVE_WAIT)) + REPLY_GRACE;
        let resp = self
            .exchange(&[("Content-Type", sealed::CONTENT_TYPE)], &body, wait)
            .await?;
        if !sealed::is_sealed(resp.header("Content-Type")) {
            // Anyone on the path can write an unsealed reply: believe
            // nothing in it but that the request failed.
            return Err(match resp.status {
                401 => "The server no longer accepts the login.".to_owned(),
                s => format!("The server answered with HTTP {s}, unencrypted."),
            }
            .into());
        }
        let parts = sealed::unframe(&resp.body)
            .map_err(|e| format!("The server sent a broken encrypted reply ({e})."))?;
        let plain_len = parts.plain_len;
        let unseal = SspiOp::Unseal {
            header: parts.header.to_vec(),
            data: parts.data.to_vec(),
        };
        let plain = match self.sspi(unseal).await? {
            SspiResult::Unsealed(p) => p,
            _ => return Err("Unsealing a reply went wrong.".to_owned().into()),
        };
        if plain.len() != plain_len {
            return Err("The server's encrypted reply has the wrong length."
                .to_owned()
                .into());
        }
        let root = soap::parse(&plain).map_err(|e| format!("The server sent {e}."))?;
        if let Some(f) = soap::fault(&root) {
            return Err(CallError::Fault(f));
        }
        if resp.status != 200 {
            return Err(format!("The server answered with HTTP {}.", resp.status).into());
        }
        Ok(root)
    }
}

/// A random (version 4) UUID for a request's `MessageID`.
fn message_id() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        hex.get(..8).unwrap_or_default(),
        hex.get(8..12).unwrap_or_default(),
        hex.get(12..16).unwrap_or_default(),
        hex.get(16..20).unwrap_or_default(),
        hex.get(20..).unwrap_or_default()
    )
}

/// Everything parsed from a server's reply, for the fuzzer: an HTTP
/// response, the encryption framing in its body, and the SOAP inside.
pub fn fuzz_reply(data: &[u8]) {
    let mut buf = data.to_vec();
    while let Ok(Some(resp)) = http::take_response(&mut buf) {
        let _ = resp.negotiate_token();
        let _ = sealed::is_sealed(resp.header("Content-Type"));
        if let Ok(parts) = sealed::unframe(&resp.body) {
            let _ = (parts.plain_len, parts.header.len(), parts.data.len());
        }
        if let Ok(root) = soap::parse(&resp.body) {
            let _ = soap::fault(&root);
            let _ = soap::shell_id(&root);
            let _ = soap::command_id(&root);
            let _ = soap::received(&root, "C");
        }
    }
}

/// [`fuzz_reply`]'s HTTP parsing, with the bytes arriving `step` at a
/// time: the responses must be the same as from the bytes whole.
pub fn fuzz_reply_chunked(data: &[u8], step: usize) {
    let mut whole = data.to_vec();
    let mut want = Vec::new();
    let whole_end = loop {
        match http::take_response(&mut whole) {
            Ok(Some(r)) => want.push(r),
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    let mut buf = Vec::new();
    let mut got = Vec::new();
    let mut end = Ok(());
    'feed: for chunk in data.chunks(step.max(1)) {
        buf.extend_from_slice(chunk);
        loop {
            match http::take_response(&mut buf) {
                Ok(Some(r)) => got.push(r),
                Ok(None) => break,
                Err(e) => {
                    end = Err(e);
                    break 'feed;
                }
            }
        }
    }
    // Bytes in pieces may stop short of an error the whole input shows
    // (a size checked only once the head is complete), never the reverse.
    assert!(got.len() <= want.len());
    assert_eq!(got.as_slice(), want.get(..got.len()).unwrap_or_default());
    if whole_end.is_ok() {
        assert!(end.is_ok());
        assert_eq!(got, want);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn data(n: usize) -> Message {
        Message::Data {
            channel: 1,
            bytes: vec![b'x'; n],
        }
    }

    #[tokio::test]
    async fn sspi_answers_pass_queued_input() {
        let (ui, inbox) = mpsc::channel(8);
        let (a, mut a_rx) = mpsc::channel(4);
        let (b, _b_rx) = mpsc::channel(4);
        let (input_tx, mut input) = mpsc::channel(1);
        let _d = Task(tokio::spawn(demux(1, inbox, [a, b], input_tx)));
        // The session is not reading input: far more than the input
        // channel holds is waiting.
        for _ in 0..100 {
            ui.send(data(10)).await.unwrap();
        }
        ui.send(Message::SspiReply {
            channel: 1,
            conn: 0,
            result: SspiResult::Unsealed(b"ok".to_vec()),
        })
        .await
        .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), a_rx.recv())
            .await
            .unwrap();
        assert_eq!(got, Some(SspiResult::Unsealed(b"ok".to_vec())));
        // The input still arrives, all of it and in order.
        for _ in 0..100 {
            assert!(matches!(input.recv().await, Some(Message::Data { .. })));
        }
    }

    #[tokio::test]
    async fn too_much_waiting_input_closes_the_session() {
        let (ui, inbox) = mpsc::channel(64);
        let (a, _a_rx) = mpsc::channel(4);
        let (b, _b_rx) = mpsc::channel(4);
        let (input_tx, mut input) = mpsc::channel(1);
        let _d = Task(tokio::spawn(demux(1, inbox, [a, b], input_tx)));
        let chunk = 1024 * 1024;
        for _ in 0..MAX_PENDING / chunk + 8 {
            ui.send(data(chunk)).await.unwrap();
        }
        // Let the demux take everything in before the session reads any.
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Whatever got through before the cap, then the close.
        loop {
            match tokio::time::timeout(Duration::from_secs(5), input.recv()).await {
                Ok(Some(Message::Data { .. })) => {}
                Ok(Some(Message::Close { .. })) => break,
                other => panic!("{:?}", other.map(|m| m.map(|m| std::mem::discriminant(&m)))),
            }
        }
    }
}
