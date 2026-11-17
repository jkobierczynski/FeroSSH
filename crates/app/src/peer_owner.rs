//! Who is at the other end of a connection to a forward's local port.
//!
//! A local (`-L`) or dynamic (`-D`, SOCKS) forward listens on loopback,
//! where every program on the PC can connect, also those of other people
//! signed in to the same PC (a terminal server) and services. SOCKS asks
//! for no password, as in OpenSSH. So a connection from loopback is
//! taken only from a program in this Windows session: Windows' TCP table
//! names the process that holds the other end, and that process's
//! session must be ours. A connection from another machine (to a forward
//! bound to other addresses, which takes them on purpose) is not checked.
//!
//! The lookup reads the whole TCP table, so callers on an async runtime
//! run it with `spawn_blocking`.

use std::net::SocketAddr;

/// What became of the check of one connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Ours, or from another machine.
    Allowed,
    /// From a program in another Windows session.
    OtherSession,
    /// From loopback, but its program could not be found (it may have
    /// closed already). Refused too.
    Unknown,
}

/// Whether the connection from `peer` to our `local` end may use the
/// forward. A loopback connection whose owner cannot be found is refused.
pub fn check(local: SocketAddr, peer: SocketAddr) -> Verdict {
    if !peer.ip().to_canonical().is_loopback() {
        return Verdict::Allowed;
    }
    #[cfg(windows)]
    {
        win::in_this_session(local, peer)
    }
    #[cfg(not(windows))]
    {
        let _ = local;
        Verdict::Allowed
    }
}

/// The owning process of the TCP connection whose own end is `from` and
/// whose other end is `to`, read from a table as `GetExtendedTcpTable`
/// writes it: a row count, then rows of `row_len` bytes. `ends` gives a
/// row's two ends and owner.
fn owner_in(
    table: &[u8],
    row_len: usize,
    ends: impl Fn(&[u8]) -> Option<(SocketAddr, SocketAddr, u32)>,
    from: SocketAddr,
    to: SocketAddr,
) -> Option<u32> {
    let count = u32::from_ne_bytes(table.get(..4)?.try_into().ok()?) as usize;
    let rows = table.get(4..)?;
    rows.chunks_exact(row_len)
        .take(count)
        .filter_map(&ends)
        .find(|&(a, b, _)| same(a, from) && same(b, to))
        .map(|(_, _, pid)| pid)
}

/// The same address and port, an IPv4-mapped IPv6 address being its IPv4
/// one.
fn same(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() == b.port() && a.ip().to_canonical() == b.ip().to_canonical()
}

/// A port as the TCP table keeps it: network order in the low 16 bits.
fn port(field: u32) -> u16 {
    u16::from_be((field & 0xFFFF) as u16)
}

fn u32_at(row: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(row.get(at..at + 4)?.try_into().ok()?))
}

/// An IPv4 row (`MIB_TCPROW_OWNER_PID`, 24 bytes): state, local address
/// and port, remote address and port, owner.
fn row_v4(row: &[u8]) -> Option<(SocketAddr, SocketAddr, u32)> {
    let ip = |at| -> Option<std::net::IpAddr> {
        let b: [u8; 4] = row.get(at..at + 4)?.try_into().ok()?;
        Some(std::net::Ipv4Addr::from(b).into())
    };
    let local = SocketAddr::new(ip(4)?, port(u32_at(row, 8)?));
    let remote = SocketAddr::new(ip(12)?, port(u32_at(row, 16)?));
    Some((local, remote, u32_at(row, 20)?))
}

/// An IPv6 row (`MIB_TCP6ROW_OWNER_PID`, 56 bytes): local address, scope
/// and port, remote address, scope and port, state, owner.
fn row_v6(row: &[u8]) -> Option<(SocketAddr, SocketAddr, u32)> {
    let ip = |at| -> Option<std::net::IpAddr> {
        let b: [u8; 16] = row.get(at..at + 16)?.try_into().ok()?;
        Some(std::net::Ipv6Addr::from(b).into())
    };
    let local = SocketAddr::new(ip(0)?, port(u32_at(row, 20)?));
    let remote = SocketAddr::new(ip(24)?, port(u32_at(row, 44)?));
    Some((local, remote, u32_at(row, 52)?))
}

/// Windows' TCP table and process sessions.
#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use std::net::SocketAddr;

    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, TCP_TABLE_OWNER_PID_CONNECTIONS,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;

    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

    /// The TCP connections of one address family with their owners.
    fn table(family: u32) -> Option<Vec<u8>> {
        // Connections come and go between the two calls: try a few times.
        for _ in 0..4 {
            let mut size = 0u32;
            // SAFETY: asks only for the size.
            let r = unsafe {
                GetExtendedTcpTable(
                    None,
                    &mut size,
                    false,
                    family,
                    TCP_TABLE_OWNER_PID_CONNECTIONS,
                    0,
                )
            };
            if r != ERROR_INSUFFICIENT_BUFFER && r != 0 {
                return None;
            }
            // Room for a few more rows that may appear meanwhile.
            let mut buf = vec![0u8; size as usize + 4096];
            let mut size = u32::try_from(buf.len()).ok()?;
            // SAFETY: `buf` is `size` bytes long.
            let r = unsafe {
                GetExtendedTcpTable(
                    Some(buf.as_mut_ptr().cast()),
                    &mut size,
                    false,
                    family,
                    TCP_TABLE_OWNER_PID_CONNECTIONS,
                    0,
                )
            };
            match r {
                0 => {
                    buf.truncate(size as usize);
                    return Some(buf);
                }
                ERROR_INSUFFICIENT_BUFFER => continue,
                _ => return None,
            }
        }
        None
    }

    fn session(pid: u32) -> Option<u32> {
        let mut id = 0u32;
        // SAFETY: writes one u32.
        unsafe { ProcessIdToSessionId(pid, &mut id) }.ok()?;
        Some(id)
    }

    pub fn in_this_session(local: SocketAddr, peer: SocketAddr) -> super::Verdict {
        use super::Verdict;
        let owner = if peer.is_ipv4() || peer.ip().to_canonical().is_ipv4() {
            table(AF_INET).and_then(|t| super::owner_in(&t, 24, super::row_v4, peer, local))
        } else {
            None
        }
        .or_else(|| {
            table(AF_INET6).and_then(|t| super::owner_in(&t, 56, super::row_v6, peer, local))
        });
        // Pid 0 is the idle process: a connection that is closing.
        let Some(pid) = owner.filter(|&pid| pid != 0) else {
            return Verdict::Unknown;
        };
        // SAFETY: no arguments.
        let me = unsafe { GetCurrentProcessId() };
        if pid == me {
            return Verdict::Allowed;
        }
        match (session(pid), session(me)) {
            (Some(theirs), Some(ours)) if theirs == ours => Verdict::Allowed,
            (Some(_), Some(_)) => Verdict::OtherSession,
            _ => Verdict::Unknown,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v4_row(local: SocketAddr, remote: SocketAddr, pid: u32) -> Vec<u8> {
        let (SocketAddr::V4(l), SocketAddr::V4(r)) = (local, remote) else {
            unreachable!()
        };
        let mut row = 5u32.to_ne_bytes().to_vec(); // ESTABLISHED
        row.extend(l.ip().octets());
        row.extend(u32::from(l.port().to_be()).to_ne_bytes());
        row.extend(r.ip().octets());
        row.extend(u32::from(r.port().to_be()).to_ne_bytes());
        row.extend(pid.to_ne_bytes());
        row
    }

    fn v6_row(local: SocketAddr, remote: SocketAddr, pid: u32) -> Vec<u8> {
        let (SocketAddr::V6(l), SocketAddr::V6(r)) = (local, remote) else {
            unreachable!()
        };
        let mut row = l.ip().octets().to_vec();
        row.extend(0u32.to_ne_bytes());
        row.extend(u32::from(l.port().to_be()).to_ne_bytes());
        row.extend(r.ip().octets());
        row.extend(0u32.to_ne_bytes());
        row.extend(u32::from(r.port().to_be()).to_ne_bytes());
        row.extend(5u32.to_ne_bytes());
        row.extend(pid.to_ne_bytes());
        row
    }

    fn table(rows: &[Vec<u8>]) -> Vec<u8> {
        let mut t = u32::try_from(rows.len()).unwrap().to_ne_bytes().to_vec();
        for r in rows {
            t.extend(r);
        }
        t
    }

    #[test]
    fn the_client_end_of_a_connection_names_its_owner() {
        let listen: SocketAddr = "127.0.0.1:1080".parse().unwrap();
        let client: SocketAddr = "127.0.0.1:50001".parse().unwrap();
        let other: SocketAddr = "127.0.0.1:50002".parse().unwrap();
        let t = table(&[
            // Our accepted end, then another program's connection, then
            // the client's end.
            v4_row(listen, client, 10),
            v4_row(other, "127.0.0.1:9".parse().unwrap(), 20),
            v4_row(client, listen, 30),
        ]);
        assert_eq!(owner_in(&t, 24, row_v4, client, listen), Some(30));
        assert_eq!(owner_in(&t, 24, row_v4, other, listen), None);
        // A count past the rows there are, and a cut row, find nothing.
        let mut short = t.clone();
        short.truncate(t.len() - 1);
        assert_eq!(owner_in(&short, 24, row_v4, client, listen), None);
        assert_eq!(owner_in(&[], 24, row_v4, client, listen), None);
    }

    #[test]
    fn ipv6_rows_and_mapped_addresses() {
        let listen: SocketAddr = "[::1]:1080".parse().unwrap();
        let client: SocketAddr = "[::1]:50001".parse().unwrap();
        let t = table(&[v6_row(client, listen, 7)]);
        assert_eq!(owner_in(&t, 56, row_v6, client, listen), Some(7));
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:50001".parse().unwrap();
        let v4: SocketAddr = "127.0.0.1:1080".parse().unwrap();
        let t = table(&[v4_row("127.0.0.1:50001".parse().unwrap(), v4, 8)]);
        assert_eq!(owner_in(&t, 24, row_v4, mapped, v4), Some(8));
    }

    #[test]
    fn other_machines_are_not_checked() {
        let local: SocketAddr = "192.0.2.1:1080".parse().unwrap();
        assert_eq!(
            check(local, "192.0.2.7:50000".parse().unwrap()),
            Verdict::Allowed
        );
    }
}
