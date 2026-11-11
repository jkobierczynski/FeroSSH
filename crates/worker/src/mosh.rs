//! A Mosh session's network side: UDP to `mosh-server` after the SSH
//! login started it (see `session.rs`, which reads its `MOSH CONNECT`
//! line).
//!
//! Everything the server sends (packets, fragments, the compressed
//! instructions and the screen diffs inside them) is parsed here, in the
//! sandboxed worker, by [`ferossh_mosh`]. The UI gets plain terminal
//! output, as from an SSH shell: the worker keeps the server's screen
//! states and writes the output that brings the pane up to the newest.
//!
//! Mosh keeps no connection: when the PC changes network the next packet
//! simply comes from the new address, and the server answers there. When
//! the server stays silent for a while, the worker sends from a new
//! source port, in case a NAT forgot the old one; a bar on the top line
//! says how long ago the server was last heard, as `mosh` does.

use std::future::poll_fn;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::Poll;
use std::time::Duration;

use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message};
use ferossh_mosh::client::Client;
use ferossh_mosh::crypto::RECEIVE_MTU;
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Old sockets are kept this long after a port change, for late replies.
const OLD_SOCKET_AGE: u64 = 60_000;
/// At most this many sockets at once.
const MAX_SOCKETS: usize = 10;
/// The longest the loop sleeps: the "last contact" bar counts seconds.
const MAX_SLEEP: u64 = 250;
/// How long a closing tab waits for the server to acknowledge.
const CLOSE_WAIT: Duration = Duration::from_millis(1500);

/// Puts the pane's terminal in application cursor-key mode for the
/// session (arrow keys then come as `ESC O A`, which the server turns
/// back when its programs do not want them), as `mosh-client` does.
const OPEN: &[u8] = b"\x1b[?1h";
/// Undoes what a session may have left on: cursor keys, renditions,
/// cursor visibility, mouse reporting, bracketed paste.
const CLOSE: &[u8] = b"\x1b[?1l\x1b[0m\x1b[?25h\x1b[?1003l\x1b[?1002l\x1b[?1001l\
\x1b[?1000l\x1b[?1015l\x1b[?1006l\x1b[?1005l\x1b[?1004l\x1b[?2004l\x1b[?5l\r\n";

/// How a Mosh session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The server's shell exited, and the server said so.
    Server,
    /// The user closed the tab.
    Closed,
}

/// Where the server listens and the session's key.
#[derive(Debug)]
pub struct Target {
    pub addr: SocketAddr,
    pub key: zeroize::Zeroizing<[u8; 16]>,
    /// When typed text shows before the server echoes it.
    pub predict: ferossh_mosh::predict::Mode,
}

async fn bind(addr: &SocketAddr) -> std::io::Result<UdpSocket> {
    let any = if addr.is_ipv6() {
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    };
    let s = UdpSocket::bind((any, 0)).await?;
    s.connect(addr).await?;
    Ok(s)
}

/// Runs the session on pane `channel` of `size` until the server ends it
/// or the UI closes the pane. An `Err` says why it could not go on.
pub async fn run(
    channel: ChannelId,
    target: Target,
    size: (u16, u16),
    inbox: &mut mpsc::Receiver<Message>,
    out: &mpsc::Sender<Message>,
) -> Result<Ended, String> {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut seed = [0u8; 8];
    let _ = getrandom::fill(&mut seed);
    let mut client = Client::new(
        &target.key,
        target.addr.is_ipv6(),
        size.0.max(1),
        size.1.max(1),
        u64::from_le_bytes(seed),
        now(),
    );
    client.set_prediction(target.predict);
    let first = bind(&target.addr)
        .await
        .map_err(|e| format!("Could not open a UDP socket for Mosh: {e}."))?;
    let mut socks: Vec<UdpSocket> = vec![first];
    let send = |bytes: &[u8]| Message::Data {
        channel,
        bytes: bytes.to_vec(),
    };
    out.send(send(OPEN)).await.map_err(|_| String::new())?;
    let mut closing: Option<Instant> = None;
    let mut last_hop = 0;
    let mut buf = vec![0u8; RECEIVE_MTU];
    let result = loop {
        let t = now();
        match client.tick(t) {
            Ok(datagrams) => {
                if let Some(sock) = socks.last() {
                    for d in datagrams {
                        // A send that fails (no route while the network
                        // changes) is retried by the protocol.
                        let _ = sock.send(&d).await;
                    }
                }
            }
            Err(e) => break Err(format!("Mosh: {e}.")),
        }
        if closing.is_none()
            && let Some(frame) = client.frame(t)
        {
            for chunk in frame.as_bytes().chunks(MAX_DATA_CHUNK) {
                if out.send(send(chunk)).await.is_err() {
                    closing = Some(Instant::now());
                    client.start_shutdown(t);
                    break;
                }
            }
        }
        if client.server_ended() {
            break Ok(Ended::Server);
        }
        if client.shutting_down()
            && (client.shutdown_acknowledged()
                || client.shutdown_timed_out(t)
                || closing.is_some_and(|c| c.elapsed() > CLOSE_WAIT))
        {
            break Ok(Ended::Closed);
        }
        if client.connect_timed_out(t) {
            break Err(format!(
                "Mosh: no answer from {} over UDP. A firewall may block the port, \
                 or the server may be behind NAT without a forwarded port.",
                target.addr
            ));
        }
        if client.wants_new_port(t) {
            if let Ok(s) = bind(&target.addr).await {
                socks.push(s);
                if socks.len() > MAX_SOCKETS {
                    socks.remove(0);
                }
            }
            client.port_changed(t);
            last_hop = t;
        }
        let wait = client.wait(t).min(MAX_SLEEP);
        tokio::select! {
            got = recv_any(&socks, &mut buf) => {
                if let Some((i, n)) = got {
                    if let Some(d) = buf.get(..n)
                        && let Err(e) = client.receive(d, now())
                    {
                        break Err(format!("Mosh: {e}."));
                    }
                    // Once the newest socket hears back, the old ones
                    // may go after a while.
                    if i + 1 == socks.len()
                        && socks.len() > 1
                        && now().saturating_sub(last_hop) > OLD_SOCKET_AGE
                    {
                        socks.drain(..socks.len() - 1);
                    }
                }
            }
            msg = inbox.recv(), if closing.is_none() => match msg {
                Some(Message::Data { bytes, .. }) => client.keys(&bytes),
                Some(Message::Resize { cols, rows, .. }) => {
                    client.resize(cols.max(1), rows.max(1));
                }
                None | Some(Message::Close { .. }) => {
                    closing = Some(Instant::now());
                    client.start_shutdown(now());
                }
                Some(_) => {}
            },
            () = tokio::time::sleep(Duration::from_millis(wait)) => {}
        }
    };
    if closing.is_none() {
        let _ = out.send(send(CLOSE)).await;
    }
    result
}

/// The next datagram on any of `socks`: which socket, and its length.
/// Errors (an ICMP "port unreachable" Windows reports on the next read,
/// a network going away) are skipped: the protocol retries.
async fn recv_any(socks: &[UdpSocket], buf: &mut [u8]) -> Option<(usize, usize)> {
    poll_fn(|cx| {
        for (i, s) in socks.iter().enumerate().rev() {
            for _ in 0..8 {
                let mut rb = ReadBuf::new(buf);
                match s.poll_recv(cx, &mut rb) {
                    Poll::Ready(Ok(())) => return Poll::Ready(Some((i, rb.filled().len()))),
                    Poll::Ready(Err(_)) => {}
                    Poll::Pending => break,
                }
            }
        }
        Poll::Pending
    })
    .await
}
