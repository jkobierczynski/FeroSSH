//! SSHFP lookups (RFC 4255) for `VerifyHostKeyDNS`: a DNS query for a
//! host's SSHFP records, sent to the DNS servers the UI names, and the
//! parser of the answer.
//!
//! The worker asks, not the UI: the answer comes from the network, so it
//! is parsed in the sandbox like everything else the network sends. Only
//! the records (algorithm, fingerprint type, digest) and whether the
//! server set the AD bit (it validated the answer with DNSSEC) go to the
//! UI, which compares them with the host key and decides.
//!
//! The query asks for DNSSEC (EDNS0 with the DO bit, and AD set to ask the
//! server to say whether it validated). The answer must carry our random
//! id and repeat our question, come from the server asked (a connected
//! UDP socket), and every name in it is read with a bound on its length
//! and on compression pointers. A truncated answer is asked again over
//! TCP.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// The DNS type of SSHFP records.
pub const TYPE_SSHFP: u16 = 44;
const TYPE_CNAME: u16 = 5;
/// The most CNAME records read and followed in one answer.
const MAX_CNAMES: usize = 8;
const TYPE_OPT: u16 = 41;
const CLASS_IN: u16 = 1;

/// More records than any host has; the rest are ignored.
pub const MAX_RECORDS: usize = 32;
/// Longest digest kept (SHA-256 is 32 bytes; room for longer ones).
pub const MAX_DIGEST: usize = 64;
/// How long one server gets to answer.
const SERVER_TIMEOUT: Duration = Duration::from_secs(2);
/// Servers tried at most.
const MAX_SERVERS: usize = 3;
/// The EDNS0 buffer size offered: fits any path without fragments.
const UDP_SIZE: u16 = 1232;

/// One SSHFP record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sshfp {
    /// 1 RSA, 2 DSA, 3 ECDSA, 4 Ed25519, 6 Ed448.
    pub algorithm: u8,
    /// 1 SHA-1, 2 SHA-256.
    pub fp_type: u8,
    pub digest: Vec<u8>,
}

/// What a DNS server answered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    /// The host's SSHFP records (none for a name without any, or one that
    /// does not exist).
    pub records: Vec<Sshfp>,
    /// The AD bit: the server says it validated the answer with DNSSEC.
    pub authenticated: bool,
    /// TC: the answer did not fit; ask over TCP.
    pub truncated: bool,
}

/// Why an answer is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bad {
    /// Too short, a name that runs off the end or loops, a bad length.
    Malformed,
    /// Not the answer to our query: another id, not a response, another
    /// question.
    NotOurs,
    /// The server failed (SERVFAIL, REFUSED, ...): the code.
    Failed(u8),
}

impl std::fmt::Display for Bad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bad::Malformed => f.write_str("a malformed answer"),
            Bad::NotOurs => f.write_str("an answer to another question"),
            Bad::Failed(2) => f.write_str("server failure"),
            Bad::Failed(5) => f.write_str("query refused"),
            Bad::Failed(code) => write!(f, "error code {code}"),
        }
    }
}

/// The name in wire format (length-prefixed labels, a zero at the end),
/// or `None` when it cannot be a DNS name: an IP address (OpenSSH does
/// not look those up either), an empty label, a label over 63 bytes, a
/// name over 255, or bytes other than letters, digits, `-` and `_`.
pub fn wire_name(host: &str) -> Option<Vec<u8>> {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let mut out = Vec::with_capacity(host.len() + 2);
    for label in host.split('.') {
        let ok = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        if label.is_empty() || label.len() > 63 || !label.bytes().all(ok) {
            return None;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    (out.len() <= 255).then_some(out)
}

/// The query for `name`'s SSHFP records, with id `id`.
pub fn query(id: u16, name: &[u8]) -> Vec<u8> {
    let mut q = Vec::with_capacity(12 + name.len() + 4 + 11);
    q.extend_from_slice(&id.to_be_bytes());
    // RD (recursion desired), AD (tell us whether you validated).
    q.extend_from_slice(&0x0120u16.to_be_bytes());
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 1]);
    q.extend_from_slice(name);
    q.extend_from_slice(&TYPE_SSHFP.to_be_bytes());
    q.extend_from_slice(&CLASS_IN.to_be_bytes());
    // EDNS0 OPT: root name, our buffer size, DO (DNSSEC OK).
    q.push(0);
    q.extend_from_slice(&TYPE_OPT.to_be_bytes());
    q.extend_from_slice(&UDP_SIZE.to_be_bytes());
    q.extend_from_slice(&[0, 0, 0x80, 0, 0, 0]);
    q
}

struct Reader<'a> {
    msg: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], Bad> {
        let end = self.at.checked_add(n).ok_or(Bad::Malformed)?;
        let s = self.msg.get(self.at..end).ok_or(Bad::Malformed)?;
        self.at = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16, Bad> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([
            *b.first().ok_or(Bad::Malformed)?,
            *b.get(1).ok_or(Bad::Malformed)?,
        ]))
    }

    /// A name at the reader's place, followed through compression
    /// pointers; the reader moves past it. Returned lower-cased, in wire
    /// format.
    fn name(&mut self) -> Result<Vec<u8>, Bad> {
        let (name, end) = read_name(self.msg, self.at)?;
        self.at = end;
        Ok(name)
    }
}

/// The name starting at `at` in `msg`, lower-cased, and where the bytes
/// after it start. Pointers may only point backwards, at most 16 are
/// followed, and the name may not exceed 255 bytes.
fn read_name(msg: &[u8], mut at: usize) -> Result<(Vec<u8>, usize), Bad> {
    let mut out = Vec::new();
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(at).ok_or(Bad::Malformed)?;
        match len & 0xC0 {
            0x00 => {
                out.push(len);
                if len == 0 {
                    return Ok((out, end.unwrap_or(at + 1)));
                }
                let label = msg
                    .get(at + 1..at + 1 + usize::from(len))
                    .ok_or(Bad::Malformed)?;
                out.extend(label.iter().map(u8::to_ascii_lowercase));
                if out.len() > 255 {
                    return Err(Bad::Malformed);
                }
                at += 1 + usize::from(len);
            }
            0xC0 => {
                let low = *msg.get(at + 1).ok_or(Bad::Malformed)?;
                let target = usize::from(len & 0x3F) << 8 | usize::from(low);
                jumps += 1;
                if target >= at || jumps > 16 {
                    return Err(Bad::Malformed);
                }
                end.get_or_insert(at + 2);
                at = target;
            }
            // 0x40 and 0x80 are not used.
            _ => return Err(Bad::Malformed),
        }
    }
}

/// Reads the answer to [`query`] `id` for `name` (wire format, as
/// [`wire_name`] gives it). Records are taken for the name asked, or for
/// the name a CNAME chain in the answer leads to. A name that does not
/// exist is an answer with no records.
pub fn parse(msg: &[u8], id: u16, name: &[u8]) -> Result<Answer, Bad> {
    let mut r = Reader { msg, at: 0 };
    if r.u16()? != id {
        return Err(Bad::NotOurs);
    }
    let flags = r.u16()?;
    let (qd, an) = (r.u16()?, r.u16()?);
    let _ns = r.u16()?;
    let _ar = r.u16()?;
    // QR set, opcode 0 (a standard query).
    if flags & 0x8000 == 0 || flags & 0x7800 != 0 {
        return Err(Bad::NotOurs);
    }
    let truncated = flags & 0x0200 != 0;
    let authenticated = flags & 0x0020 != 0;
    let rcode = (flags & 0x000F) as u8;
    if qd != 1 {
        // A truncated answer may leave out even the question.
        return if truncated && qd == 0 {
            Ok(Answer {
                truncated,
                ..Answer::default()
            })
        } else {
            Err(Bad::NotOurs)
        };
    }
    let lower: Vec<u8> = name.iter().map(u8::to_ascii_lowercase).collect();
    if r.name()? != lower || r.u16()? != TYPE_SSHFP || r.u16()? != CLASS_IN {
        return Err(Bad::NotOurs);
    }
    match rcode {
        0 => {}
        // NXDOMAIN: no such name, so no records.
        3 => {
            return Ok(Answer {
                authenticated,
                truncated,
                ..Answer::default()
            });
        }
        code => return Err(Bad::Failed(code)),
    }
    // A truncated answer is asked again over TCP: its records are not
    // read, as the last one may be cut off.
    if truncated {
        return Ok(Answer {
            authenticated,
            truncated,
            ..Answer::default()
        });
    }
    // Every record first, then the CNAME chain from the name asked, so
    // the order of the answer section does not matter.
    let mut cnames: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut found: Vec<(Vec<u8>, Sshfp)> = Vec::new();
    for _ in 0..an {
        let rr_name = r.name()?;
        let rtype = r.u16()?;
        let class = r.u16()?;
        let _ttl = r.take(4)?;
        let len = usize::from(r.u16()?);
        let start = r.at;
        let data = r.take(len)?;
        if class != CLASS_IN {
            continue;
        }
        match rtype {
            TYPE_CNAME if cnames.len() < MAX_CNAMES => {
                // The target must lie within this record's data.
                let (target, end) = read_name(msg, start)?;
                if end != start + len {
                    return Err(Bad::Malformed);
                }
                cnames.push((rr_name, target));
            }
            TYPE_SSHFP if found.len() < MAX_RECORDS * 4 => {
                let [algorithm, fp_type, digest @ ..] = data else {
                    return Err(Bad::Malformed);
                };
                if digest.is_empty() || digest.len() > MAX_DIGEST {
                    continue;
                }
                found.push((
                    rr_name,
                    Sshfp {
                        algorithm: *algorithm,
                        fp_type: *fp_type,
                        digest: digest.to_vec(),
                    },
                ));
            }
            _ => {}
        }
    }
    let mut owner = lower;
    for _ in 0..MAX_CNAMES {
        match cnames.iter().find(|(from, _)| *from == owner) {
            Some((_, to)) => owner = to.clone(),
            None => break,
        }
    }
    let records: Vec<Sshfp> = found
        .into_iter()
        .filter(|(n, _)| *n == owner)
        .map(|(_, rec)| rec)
        .take(MAX_RECORDS)
        .collect();
    Ok(Answer {
        records,
        authenticated,
        truncated,
    })
}

fn random_id() -> u16 {
    let mut b = [0u8; 2];
    // A fixed id only makes a forged answer easier to guess; the answer
    // still has to come from the server asked.
    let _ = getrandom::fill(&mut b);
    u16::from_be_bytes(b)
}

async fn over_udp(server: SocketAddr, q: &[u8], id: u16, name: &[u8]) -> Result<Answer, String> {
    let bind: SocketAddr = if server.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let sock = UdpSocket::bind(bind).await.map_err(|e| e.to_string())?;
    // Connected: datagrams from anywhere else are dropped.
    sock.connect(server).await.map_err(|e| e.to_string())?;
    sock.send(q).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 4096];
    loop {
        let n = sock.recv(&mut buf).await.map_err(|e| e.to_string())?;
        match parse(buf.get(..n).unwrap_or_default(), id, name) {
            // A stray datagram: keep waiting for ours.
            Err(Bad::NotOurs) => continue,
            other => return other.map_err(|e| e.to_string()),
        }
    }
}

async fn over_tcp(server: SocketAddr, q: &[u8], id: u16, name: &[u8]) -> Result<Answer, String> {
    let mut s = TcpStream::connect(server)
        .await
        .map_err(|e| e.to_string())?;
    let len = u16::try_from(q.len()).map_err(|e| e.to_string())?;
    let mut framed = len.to_be_bytes().to_vec();
    framed.extend_from_slice(q);
    s.write_all(&framed).await.map_err(|e| e.to_string())?;
    let n = usize::from(s.read_u16().await.map_err(|e| e.to_string())?);
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
    parse(&buf, id, name).map_err(|e| e.to_string())
}

async fn ask(server: SocketAddr, name: &[u8]) -> Result<Answer, String> {
    let id = random_id();
    let q = query(id, name);
    let answer = over_udp(server, &q, id, name).await?;
    if !answer.truncated {
        return Ok(answer);
    }
    let id = random_id();
    let q = query(id, name);
    over_tcp(server, &q, id, name).await
}

/// Asks the servers in turn (each for a couple of seconds, at most
/// three) for `host`'s SSHFP records. Returns the first answer and the
/// server that gave it, or why none did.
pub async fn lookup(host: &str, servers: &[SocketAddr]) -> Result<(Answer, SocketAddr), String> {
    let Some(name) = wire_name(host) else {
        return Err(format!("{host} is not a DNS name"));
    };
    if servers.is_empty() {
        return Err("no DNS server is set up on this PC".to_owned());
    }
    let mut why = Vec::new();
    for &server in servers.iter().take(MAX_SERVERS) {
        match tokio::time::timeout(SERVER_TIMEOUT, ask(server, &name)).await {
            Ok(Ok(answer)) => return Ok((answer, server)),
            Ok(Err(e)) => why.push(format!("{server}: {e}")),
            Err(_) => why.push(format!("{server}: no answer")),
        }
    }
    Err(why.join("; "))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// An answer to `query(id, name)` with these answer records (name,
    /// type, data) appended after the question, names written out.
    fn answer(id: u16, flags: u16, name: &[u8], rrs: &[(&[u8], u16, Vec<u8>)]) -> Vec<u8> {
        let mut m = id.to_be_bytes().to_vec();
        m.extend_from_slice(&flags.to_be_bytes());
        m.extend_from_slice(&[0, 1]);
        m.extend_from_slice(&(rrs.len() as u16).to_be_bytes());
        m.extend_from_slice(&[0, 0, 0, 0]);
        m.extend_from_slice(name);
        m.extend_from_slice(&TYPE_SSHFP.to_be_bytes());
        m.extend_from_slice(&CLASS_IN.to_be_bytes());
        for (owner, t, data) in rrs {
            m.extend_from_slice(owner);
            m.extend_from_slice(&t.to_be_bytes());
            m.extend_from_slice(&CLASS_IN.to_be_bytes());
            m.extend_from_slice(&[0, 0, 1, 0]);
            m.extend_from_slice(&(data.len() as u16).to_be_bytes());
            m.extend_from_slice(data);
        }
        m
    }

    fn sshfp(alg: u8, t: u8, digest: &[u8]) -> Vec<u8> {
        let mut d = vec![alg, t];
        d.extend_from_slice(digest);
        d
    }

    #[test]
    fn names() {
        assert_eq!(wire_name("a.Bc"), Some(b"\x01a\x02Bc\x00".to_vec()));
        assert_eq!(wire_name("a.bc."), Some(b"\x01a\x02bc\x00".to_vec()));
        assert_eq!(wire_name("10.0.0.1"), None);
        assert_eq!(wire_name("::1"), None);
        assert_eq!(wire_name("a..b"), None);
        assert_eq!(wire_name("a b"), None);
        assert_eq!(wire_name(""), None);
        assert_eq!(wire_name(&"a".repeat(64)), None);
        let long = vec!["a".repeat(63); 4].join(".");
        assert_eq!(wire_name(&long), None);
    }

    #[test]
    fn query_asks_for_dnssec() {
        let name = wire_name("host.example").unwrap();
        let q = query(0x1234, &name);
        assert_eq!(&q[..4], &[0x12, 0x34, 0x01, 0x20]);
        assert_eq!(&q[12..12 + name.len()], &name[..]);
        // The OPT record's DO bit.
        assert_eq!(q[q.len() - 4], 0x80);
    }

    #[test]
    fn reads_records_and_the_ad_bit() {
        let name = wire_name("Host.example").unwrap();
        let msg = answer(
            7,
            0x81A0,
            b"\x04host\x07example\x00",
            &[
                (b"\xC0\x0C", TYPE_SSHFP, sshfp(4, 2, &[9; 32])),
                // Another name: ignored.
                (b"\x01x\x00", TYPE_SSHFP, sshfp(4, 2, &[1; 32])),
                (b"\xC0\x0C", TYPE_SSHFP, sshfp(1, 1, &[8; 20])),
            ],
        );
        let a = parse(&msg, 7, &name).unwrap();
        assert!(a.authenticated && !a.truncated);
        assert_eq!(
            a.records,
            vec![
                Sshfp {
                    algorithm: 4,
                    fp_type: 2,
                    digest: vec![9; 32]
                },
                Sshfp {
                    algorithm: 1,
                    fp_type: 1,
                    digest: vec![8; 20]
                }
            ]
        );
        // Without AD.
        let msg = answer(7, 0x8180, &name, &[]);
        assert!(!parse(&msg, 7, &name).unwrap().authenticated);
    }

    #[test]
    fn follows_a_cname_chain() {
        let name = wire_name("a.example").unwrap();
        let msg = answer(
            1,
            0x8180,
            &name,
            &[
                (&name, TYPE_CNAME, b"\x01b\xC0\x0E".to_vec()),
                (b"\x01b\x07example\x00", TYPE_SSHFP, sshfp(4, 2, &[3; 32])),
            ],
        );
        let a = parse(&msg, 1, &name).unwrap();
        assert_eq!(a.records.len(), 1);
        // The same with the record before the CNAME that leads to it.
        let msg = answer(
            1,
            0x8180,
            &name,
            &[
                (b"\x01b\x07example\x00", TYPE_SSHFP, sshfp(4, 2, &[3; 32])),
                (&name, TYPE_CNAME, b"\x01b\x07example\x00".to_vec()),
                // A record for a name the chain does not reach is left out.
                (b"\x01c\x07example\x00", TYPE_SSHFP, sshfp(4, 2, &[4; 32])),
            ],
        );
        let a = parse(&msg, 1, &name).unwrap();
        assert_eq!(a.records.len(), 1);
        assert_eq!(a.records[0].digest, vec![3; 32]);
    }

    #[test]
    fn refuses_what_is_not_ours() {
        let name = wire_name("a.example").unwrap();
        let ok = answer(1, 0x8180, &name, &[]);
        assert_eq!(parse(&ok, 2, &name), Err(Bad::NotOurs));
        let query_back = answer(1, 0x0100, &name, &[]);
        assert_eq!(parse(&query_back, 1, &name), Err(Bad::NotOurs));
        let other = wire_name("b.example").unwrap();
        assert_eq!(parse(&ok, 1, &other), Err(Bad::NotOurs));
        let servfail = answer(1, 0x8182, &name, &[]);
        assert_eq!(parse(&servfail, 1, &name), Err(Bad::Failed(2)));
        let nx = answer(1, 0x8183, &name, &[]);
        assert_eq!(parse(&nx, 1, &name).unwrap().records, vec![]);
        let tc = answer(1, 0x8380, &name, &[]);
        assert!(parse(&tc, 1, &name).unwrap().truncated);
        // Truncated inside a record: still "ask over TCP", not an error.
        let mut cut = answer(
            1,
            0x8380,
            &name,
            &[(&name, TYPE_SSHFP, sshfp(4, 2, &[3; 32]))],
        );
        cut.truncate(cut.len() - 10);
        let a = parse(&cut, 1, &name).unwrap();
        assert!(a.truncated && a.records.is_empty());
    }

    #[test]
    fn malformed_answers_are_errors() {
        let name = wire_name("a.example").unwrap();
        let ok = answer(
            1,
            0x8180,
            &name,
            &[(b"\xC0\x0C", TYPE_SSHFP, sshfp(4, 2, &[3; 32]))],
        );
        for cut in 0..ok.len() {
            assert!(parse(&ok[..cut], 1, &name).is_err(), "cut at {cut}");
        }
        // A pointer to itself, and one forwards.
        let mut looped = answer(
            1,
            0x8180,
            &name,
            &[(b"\xC0\x1B", TYPE_SSHFP, vec![4, 2, 1])],
        );
        let at = 12 + name.len() + 4;
        looped[at + 1] = at as u8;
        assert_eq!(parse(&looped, 1, &name), Err(Bad::Malformed));
        // A record too short for an SSHFP one.
        let short = answer(1, 0x8180, &name, &[(b"\xC0\x0C", TYPE_SSHFP, vec![4])]);
        assert_eq!(parse(&short, 1, &name), Err(Bad::Malformed));
    }

    #[tokio::test]
    async fn looks_up_over_udp_and_falls_back_to_tcp() {
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tcp = tokio::net::TcpListener::bind(udp.local_addr().unwrap())
            .await
            .unwrap();
        let server = udp.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (n, from) = udp.recv_from(&mut buf).await.unwrap();
            let q = &buf[..n];
            let id = u16::from_be_bytes([q[0], q[1]]);
            let name = wire_name("host.example").unwrap();
            // A stray answer with another id first, then a truncated one.
            let stray = answer(id.wrapping_add(1), 0x8180, &name, &[]);
            udp.send_to(&stray, from).await.unwrap();
            udp.send_to(&answer(id, 0x8380, &name, &[]), from)
                .await
                .unwrap();
            let (mut s, _) = tcp.accept().await.unwrap();
            let n = usize::from(s.read_u16().await.unwrap());
            let mut q = vec![0u8; n];
            s.read_exact(&mut q).await.unwrap();
            let id = u16::from_be_bytes([q[0], q[1]]);
            let a = answer(
                id,
                0x81A0,
                &name,
                &[(b"\xC0\x0C", TYPE_SSHFP, sshfp(4, 2, &[5; 32]))],
            );
            s.write_all(&(a.len() as u16).to_be_bytes()).await.unwrap();
            s.write_all(&a).await.unwrap();
        });
        let (a, from) = lookup("host.example", &[server]).await.unwrap();
        assert_eq!(from, server);
        assert!(a.authenticated);
        assert_eq!(a.records.len(), 1);
        assert!(lookup("10.0.0.1", &[server]).await.is_err());
        assert!(lookup("host.example", &[]).await.is_err());
    }
}
