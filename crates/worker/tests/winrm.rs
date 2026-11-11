//! A WinRM session end to end against a stand-in server: the worker logs
//! in on both its HTTP connections, opens a shell, starts the command,
//! relays output (across a Receive that times out on the server), input,
//! Ctrl+C and the exit code, with every message sealed. The test plays
//! the UI, with a stand-in security context: tokens are fixed strings
//! and sealing XORs the message, so the server can check both.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::Duration;

use base64ct::{Base64, Encoding};
use ferossh_ipc::{Message, PROTOCOL_VERSION, SspiOp, SspiResult};
use ferossh_worker::winrm::{sealed, xml};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify, mpsc};

const KEY: u8 = 0x5a;
const SIG: &[u8] = b"SIGNATURE-16-BYT";

fn xor(data: &[u8]) -> Vec<u8> {
    data.iter().map(|b| b ^ KEY).collect()
}

#[derive(Default)]
struct Shared {
    stdin: Vec<u8>,
    signals: Vec<String>,
    deleted: bool,
    receives: usize,
}

struct Request {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn read_request(s: &mut TcpStream, buf: &mut Vec<u8>) -> Option<Request> {
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8(buf[..end].to_vec()).unwrap();
            let mut lines = head.split("\r\n");
            assert!(lines.next().unwrap().starts_with("POST /wsman HTTP/1.1"));
            let headers: Vec<(String, String)> = lines
                .map(|l| {
                    let (n, v) = l.split_once(':').unwrap();
                    (n.to_owned(), v.trim().to_owned())
                })
                .collect();
            let len: usize = headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("Content-Length"))
                .map(|(_, v)| v.parse().unwrap())
                .unwrap();
            if buf.len() >= end + 4 + len {
                let body = buf[end + 4..end + 4 + len].to_vec();
                buf.drain(..end + 4 + len);
                return Some(Request { headers, body });
            }
        }
        let mut chunk = [0u8; 8192];
        let n = s.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn respond(s: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &[u8]) {
    let mut out = format!("HTTP/1.1 {status}\r\n");
    for (n, v) in headers {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut out = out.into_bytes();
    out.extend_from_slice(body);
    s.write_all(&out).await.unwrap();
}

async fn respond_sealed(s: &mut TcpStream, status: &str, envelope: &str) {
    let body = sealed::frame(envelope.len(), SIG, &xor(envelope.as_bytes()));
    respond(s, status, &[("Content-Type", sealed::CONTENT_TYPE)], &body).await;
}

fn envelope(body: &str) -> String {
    format!(
        "<s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" \
         xmlns:rsp=\"http://schemas.microsoft.com/wbem/wsman/1/windows/shell\">\
         <s:Header/><s:Body>{body}</s:Body></s:Envelope>"
    )
}

fn stream(name: &str, data: &[u8]) -> String {
    format!(
        "<rsp:Stream Name=\"{name}\" CommandId=\"C-1\">{}</rsp:Stream>",
        Base64::encode_string(data)
    )
}

/// One client connection: the Negotiate login, then sealed requests.
async fn serve_conn(mut s: TcpStream, shared: Arc<Mutex<Shared>>, input: Arc<Notify>) {
    let mut buf = Vec::new();
    // Login: T1 -> 401 S1 -> T2 -> 200.
    let r = read_request(&mut s, &mut buf).await.unwrap();
    assert_eq!(r.header("Authorization"), Some("Negotiate VDE="));
    assert!(r.body.is_empty());
    respond(
        &mut s,
        "401 Unauthorized",
        &[("WWW-Authenticate", "Negotiate UzE=")],
        b"",
    )
    .await;
    let r = read_request(&mut s, &mut buf).await.unwrap();
    assert_eq!(r.header("Authorization"), Some("Negotiate VDI="));
    respond(&mut s, "200 OK", &[], b"").await;
    while let Some(r) = read_request(&mut s, &mut buf).await {
        assert!(sealed::is_sealed(r.header("Content-Type")));
        assert!(r.header("Authorization").is_none());
        let parts = sealed::unframe(&r.body).unwrap();
        assert_eq!(parts.header, SIG);
        let plain = xor(parts.data);
        assert_eq!(plain.len(), parts.plain_len);
        let root = xml::parse(&plain).unwrap();
        let action = root.find("Action").unwrap().text();
        let verb = action.rsplit('/').next().unwrap().to_owned();
        assert!(root.find("MessageID").unwrap().text().len() > 40);
        match verb.as_str() {
            "Create" => {
                assert!(root.find("Selector").is_none());
                respond_sealed(
                    &mut s,
                    "200 OK",
                    &envelope("<rsp:Shell><rsp:ShellId>S-1</rsp:ShellId></rsp:Shell>"),
                )
                .await;
            }
            "Command" => {
                assert_eq!(root.find("Selector").unwrap().text(), "S-1");
                assert_eq!(root.find("Command").unwrap().text(), "powershell -NoLogo");
                respond_sealed(
                    &mut s,
                    "200 OK",
                    &envelope("<rsp:CommandResponse><rsp:CommandId>C-1</rsp:CommandId></rsp:CommandResponse>"),
                )
                .await;
            }
            "Receive" => {
                let n = {
                    let mut sh = shared.lock().await;
                    sh.receives += 1;
                    sh.receives
                };
                let reply = match n {
                    1 => envelope(&format!(
                        "<rsp:ReceiveResponse>{}{}</rsp:ReceiveResponse>",
                        stream("stdout", b"PS C:\\> "),
                        stream("stderr", b"warning\r\n")
                    )),
                    2 => {
                        // No output yet: the server's operation timeout.
                        let fault = envelope(
                            "<s:Fault><s:Code><s:Value>s:Receiver</s:Value><s:Subcode>\
                             <s:Value>w:TimedOut</s:Value></s:Subcode></s:Code>\
                             <s:Reason><s:Text>timed out</s:Text></s:Reason></s:Fault>",
                        );
                        respond_sealed(&mut s, "500 Internal Server Error", &fault).await;
                        continue;
                    }
                    _ => {
                        input.notified().await;
                        let sh = shared.lock().await;
                        if sh.stdin.ends_with(b"exit\r\n") {
                            envelope(&format!(
                                "<rsp:ReceiveResponse>{}<rsp:CommandState CommandId=\"C-1\" \
                                 State=\"http://schemas.microsoft.com/wbem/wsman/1/windows/shell/CommandState/Done\">\
                                 <rsp:ExitCode>7</rsp:ExitCode></rsp:CommandState></rsp:ReceiveResponse>",
                                stream("stdout", b"bye\r\n")
                            ))
                        } else {
                            let echo = format!("got {}", String::from_utf8_lossy(&sh.stdin));
                            envelope(&format!(
                                "<rsp:ReceiveResponse>{}</rsp:ReceiveResponse>",
                                stream("stdout", echo.as_bytes())
                            ))
                        }
                    }
                };
                respond_sealed(&mut s, "200 OK", &reply).await;
            }
            "Send" => {
                let st = root.find("Stream").unwrap();
                assert_eq!(st.attr("CommandId"), Some("C-1"));
                let data = Base64::decode_vec(&st.text()).unwrap();
                shared.lock().await.stdin.extend_from_slice(&data);
                input.notify_one();
                respond_sealed(&mut s, "200 OK", &envelope("<rsp:SendResponse/>")).await;
            }
            "Signal" => {
                let code = root.find("Code").unwrap().text();
                shared.lock().await.signals.push(code);
                respond_sealed(&mut s, "200 OK", &envelope("<rsp:SignalResponse/>")).await;
            }
            "Delete" => {
                shared.lock().await.deleted = true;
                respond_sealed(&mut s, "200 OK", &envelope("")).await;
            }
            other => panic!("unexpected action {other}"),
        }
    }
}

/// Plays the UI's side of SSPI: fixed tokens and XOR sealing.
fn answer(op: SspiOp) -> SspiResult {
    match op {
        SspiOp::Start => SspiResult::Continue(b"T1".to_vec()),
        SspiOp::Step(t) => {
            assert_eq!(t, b"S1");
            SspiResult::Done(b"T2".to_vec())
        }
        SspiOp::Seal(p) => SspiResult::Sealed {
            header: SIG.to_vec(),
            data: xor(&p),
        },
        SspiOp::Unseal { header, data } => {
            if header != SIG {
                return SspiResult::Failed("bad signature".into());
            }
            SspiResult::Unsealed(xor(&data))
        }
    }
}

#[tokio::test]
async fn runs_a_winrm_shell() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let input = Arc::new(Notify::new());
    let (sh, inp) = (shared.clone(), input.clone());
    tokio::spawn(async move {
        loop {
            let (s, _) = listener.accept().await.unwrap();
            tokio::spawn(serve_conn(s, sh.clone(), inp.clone()));
        }
    });

    let (to_worker, inbox) = mpsc::channel(64);
    let (out, mut from_worker) = mpsc::channel(64);
    tokio::spawn(ferossh_worker::serve(inbox, out));
    assert!(matches!(
        from_worker.recv().await,
        Some(Message::Hello { .. })
    ));
    to_worker
        .send(Message::Hello {
            version: PROTOCOL_VERSION,
        })
        .await
        .unwrap();
    to_worker
        .send(Message::WinrmConnect {
            channel: 3,
            host: "127.0.0.1".into(),
            port,
            connect_timeout: 5,
            command: "powershell -NoLogo".into(),
        })
        .await
        .unwrap();

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut conns = std::collections::HashSet::new();
    let mut sent_line = false;
    let mut sent_exit = false;
    let mut exit = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let reason = loop {
        let msg = tokio::time::timeout_at(deadline, from_worker.recv())
            .await
            .unwrap_or_else(|_| panic!("stuck; stdout {stdout:?}"))
            .unwrap();
        match msg {
            Message::SspiRequest {
                channel: 3,
                conn,
                op,
            } => {
                conns.insert(conn);
                let result = answer(op);
                to_worker
                    .send(Message::SspiReply {
                        channel: 3,
                        conn,
                        result,
                    })
                    .await
                    .unwrap();
            }
            Message::Connected { channel: 3, .. } => {}
            Message::Data { channel: 3, bytes } => {
                stdout.push_str(&String::from_utf8_lossy(&bytes));
                if stdout.contains("PS C:\\> ") && !sent_line {
                    sent_line = true;
                    // Ctrl+C is a signal, not input.
                    for bytes in [vec![3], b"Get-Date\r\n".to_vec()] {
                        to_worker
                            .send(Message::Data { channel: 3, bytes })
                            .await
                            .unwrap();
                    }
                }
                if stdout.contains("got Get-Date") && !sent_exit {
                    sent_exit = true;
                    to_worker
                        .send(Message::Data {
                            channel: 3,
                            bytes: b"exit\r\n".to_vec(),
                        })
                        .await
                        .unwrap();
                }
            }
            Message::ExecStderr { channel: 3, bytes } => {
                stderr.push_str(&String::from_utf8_lossy(&bytes));
            }
            Message::ExitStatus { channel: 3, code } => exit = Some(code),
            Message::Disconnected { channel: 3, reason } => break reason,
            other => panic!("unexpected {other:?}"),
        }
    };
    assert_eq!(conns.len(), 2, "output and input each log in");
    assert!(stdout.ends_with("bye\r\n"), "{stdout:?}");
    assert_eq!(stderr, "warning\r\n");
    assert_eq!(exit, Some(7));
    assert!(reason.contains("exit code 7"), "{reason}");
    let sh = shared.lock().await;
    assert_eq!(sh.stdin, b"Get-Date\r\nexit\r\n");
    assert_eq!(sh.signals.len(), 1);
    assert!(sh.signals[0].ends_with("/ctrl_c"));
    assert!(sh.deleted);
}

/// A reply that is not sealed is never believed, whatever it says.
#[tokio::test]
async fn refuses_unsealed_replies() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        read_request(&mut s, &mut buf).await.unwrap();
        respond(
            &mut s,
            "401 Unauthorized",
            &[("WWW-Authenticate", "Negotiate UzE=")],
            b"",
        )
        .await;
        read_request(&mut s, &mut buf).await.unwrap();
        respond(&mut s, "200 OK", &[], b"").await;
        read_request(&mut s, &mut buf).await.unwrap();
        let body = envelope("<rsp:Shell><rsp:ShellId>S-1</rsp:ShellId></rsp:Shell>");
        respond(
            &mut s,
            "200 OK",
            &[("Content-Type", "application/soap+xml;charset=UTF-8")],
            body.as_bytes(),
        )
        .await;
        let _ = read_request(&mut s, &mut buf).await;
    });
    let (to_worker, inbox) = mpsc::channel(64);
    let (out, mut from_worker) = mpsc::channel(64);
    tokio::spawn(ferossh_worker::serve(inbox, out));
    from_worker.recv().await.unwrap();
    to_worker
        .send(Message::Hello {
            version: PROTOCOL_VERSION,
        })
        .await
        .unwrap();
    to_worker
        .send(Message::WinrmConnect {
            channel: 3,
            host: "127.0.0.1".into(),
            port,
            connect_timeout: 5,
            command: "cmd".into(),
        })
        .await
        .unwrap();
    let reason = loop {
        match tokio::time::timeout(Duration::from_secs(10), from_worker.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Message::SspiRequest { conn, op, .. } => {
                let result = answer(op);
                to_worker
                    .send(Message::SspiReply {
                        channel: 3,
                        conn,
                        result,
                    })
                    .await
                    .unwrap();
            }
            Message::Disconnected { reason, .. } => break reason,
            other => panic!("unexpected {other:?}"),
        }
    };
    assert!(reason.contains("unencrypted"), "{reason}");
}
