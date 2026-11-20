//! The DNS servers this PC uses, for the SSHFP lookups of
//! `VerifyHostKeyDNS` (the worker sends the queries; see
//! `ferossh_worker::dns`).
//!
//! On Windows, the servers of the network adapters that are up, in the
//! order Windows lists them; elsewhere (tests), `/etc/resolv.conf`.

use std::net::SocketAddr;

/// Most servers handed to the worker; it tries three at most.
const MAX: usize = 4;

/// The servers, without duplicates.
pub fn system() -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    for a in os_servers() {
        if !out.contains(&a) && out.len() < MAX {
            out.push(a);
        }
    }
    out
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn os_servers() -> Vec<SocketAddr> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_FRIENDLY_NAME, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6,
    };

    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_FRIENDLY_NAME;
    let mut size: u32 = 16 * 1024;
    let mut buf: Vec<u64> = Vec::new();
    // The list can grow between the calls; try a few times.
    for _ in 0..3 {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: the buffer is `size` bytes, 8-aligned, and lives through
        // the call.
        let r = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC.0),
                flags,
                None,
                Some(buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
                &mut size,
            )
        };
        match r {
            0 => break,
            // ERROR_BUFFER_OVERFLOW: `size` now says how much is needed.
            111 => {
                buf.clear();
                continue;
            }
            _ => return Vec::new(),
        }
    }
    if buf.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // SAFETY: Windows filled the buffer with a linked list of adapters
    // whose pointers point inside it; it is read while `buf` lives.
    unsafe {
        let mut adapter = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while let Some(a) = adapter.as_ref() {
            if a.OperStatus == IfOperStatusUp {
                let mut dns = a.FirstDnsServerAddress;
                while let Some(d) = dns.as_ref() {
                    let sa = d.Address.lpSockaddr;
                    if let Some(raw) = sa.as_ref() {
                        if raw.sa_family == AF_INET {
                            let v4 = &*sa.cast::<SOCKADDR_IN>();
                            let ip =
                                std::net::Ipv4Addr::from(u32::from_be(v4.sin_addr.S_un.S_addr));
                            out.push(SocketAddr::from((ip, 53)));
                        } else if raw.sa_family == AF_INET6 {
                            let v6 = &*sa.cast::<SOCKADDR_IN6>();
                            let ip = std::net::Ipv6Addr::from(v6.sin6_addr.u.Byte);
                            // Windows' placeholder servers (fec0:0:0:ffff::1-3)
                            // answer nothing.
                            if ip.segments()[..4] != [0xfec0, 0, 0, 0xffff] {
                                out.push(SocketAddr::V6(std::net::SocketAddrV6::new(
                                    ip,
                                    53,
                                    0,
                                    v6.Anonymous.sin6_scope_id,
                                )));
                            }
                        }
                    }
                    dns = d.Next;
                }
            }
            adapter = a.Next;
        }
    }
    out
}

#[cfg(not(windows))]
fn os_servers() -> Vec<SocketAddr> {
    std::fs::read_to_string("/etc/resolv.conf")
        .map(|t| parse_resolv_conf(&t))
        .unwrap_or_default()
}

/// The `nameserver` lines of a resolv.conf.
#[cfg_attr(windows, allow(dead_code))]
fn parse_resolv_conf(text: &str) -> Vec<SocketAddr> {
    text.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            (w.next()? == "nameserver").then_some(())?;
            let ip: std::net::IpAddr = w.next()?.split('%').next()?.parse().ok()?;
            Some(SocketAddr::from((ip, 53)))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_resolv_conf() {
        let t = "# x\nnameserver 192.168.1.1\nsearch lan\nnameserver ::1\nnameserver bad\n";
        assert_eq!(
            parse_resolv_conf(t),
            vec![
                "192.168.1.1:53".parse::<SocketAddr>().unwrap(),
                "[::1]:53".parse().unwrap()
            ]
        );
    }
}
