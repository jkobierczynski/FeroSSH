//! X11 forwarding pieces: the `DISPLAY` setting, the local `.Xauthority`
//! file, and the cookie swap in a client's connection setup.
//!
//! The server gets a fake cookie. When an X program on the server
//! connects, the worker reads the connection setup, checks that it carries
//! that fake cookie, and puts the real one (or none, for an X server that
//! needs none) in its place before anything reaches the X server on this
//! PC, as OpenSSH does. A connection with any other cookie is closed.
//!
//! FeroSSH ships no X server; this is for one running on Windows, such as
//! VcXsrv or X410, listening on TCP.

/// The first TCP port of X displays: display N listens on 6000 + N.
pub const X11_BASE_PORT: u16 = 6000;

/// The authorisation protocol FeroSSH uses towards the server.
pub const MIT_COOKIE: &str = "MIT-MAGIC-COOKIE-1";

/// The longest authorisation name or data accepted in a setup.
const MAX_AUTH_FIELD: usize = 256;

/// A display as `DISPLAY` writes it: `[host]:number[.screen]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    /// `localhost` when the setting names no host.
    pub host: String,
    pub number: u16,
    pub screen: u32,
}

impl Display {
    /// Parses `localhost:0`, `:0.0`, `192.168.1.5:1` or `[::1]:0`.
    /// Unix socket forms (`unix:0`, paths) have no meaning on Windows.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (host, rest) = if let Some(v6) = text.strip_prefix('[') {
            let (h, rest) = v6.split_once(']')?;
            (h, rest.strip_prefix(':')?)
        } else {
            text.rsplit_once(':')?
        };
        let (number, screen) = match rest.split_once('.') {
            Some((n, s)) => (n, s.parse().ok()?),
            None => (rest, 0),
        };
        let number: u16 = number.parse().ok()?;
        if number > u16::MAX - X11_BASE_PORT || host == "unix" || host.contains('/') {
            return None;
        }
        let host = if host.is_empty() { "localhost" } else { host };
        Some(Self {
            host: host.to_owned(),
            number,
            screen,
        })
    }

    pub fn port(&self) -> u16 {
        X11_BASE_PORT + self.number
    }

    fn is_local(&self) -> bool {
        matches!(self.host.as_str(), "localhost" | "127.0.0.1" | "::1")
    }
}

impl std::fmt::Display for Display {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}.{}", self.host, self.number, self.screen)
        } else {
            write!(f, "{}:{}.{}", self.host, self.number, self.screen)
        }
    }
}

/// `.Xauthority` address families.
mod family {
    pub const INTERNET: u16 = 0;
    pub const LOCAL: u16 = 256;
    pub const WILD: u16 = 65535;
}

/// One entry of an `.Xauthority` file.
#[derive(Clone, PartialEq, Eq)]
pub struct XauthEntry {
    family: u16,
    address: Vec<u8>,
    number: Vec<u8>,
    pub name: String,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for XauthEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XauthEntry")
            .field("family", &self.family)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Reads the entries of an `.Xauthority` file; stops at the first entry
/// that does not parse.
pub fn xauth_entries(file: &[u8]) -> Vec<XauthEntry> {
    fn field<'a>(b: &mut &'a [u8]) -> Option<&'a [u8]> {
        let (len, rest) = b.split_first_chunk::<2>()?;
        let len = usize::from(u16::from_be_bytes(*len));
        let (v, rest) = (rest.get(..len)?, rest.get(len..)?);
        *b = rest;
        Some(v)
    }
    let mut b = file;
    let mut out = Vec::new();
    while let Some((fam, rest)) = b.split_first_chunk::<2>() {
        b = rest;
        let entry = (|| {
            Some(XauthEntry {
                family: u16::from_be_bytes(*fam),
                address: field(&mut b)?.to_vec(),
                number: field(&mut b)?.to_vec(),
                name: String::from_utf8(field(&mut b)?.to_vec()).ok()?,
                data: field(&mut b)?.to_vec(),
            })
        })();
        match entry {
            Some(e) => out.push(e),
            None => break,
        }
    }
    out
}

/// The cookie for `display` in an `.Xauthority` file: an entry for the
/// display's number whose address fits (any address for a wildcard or a
/// local entry, the loopback address for a local display, or the
/// display's own address), preferring MIT-MAGIC-COOKIE-1.
pub fn find_cookie(entries: &[XauthEntry], display: &Display) -> Option<(String, Vec<u8>)> {
    let number = display.number.to_string();
    let host_v4: Option<[u8; 4]> = display
        .host
        .parse::<std::net::Ipv4Addr>()
        .ok()
        .map(|a| a.octets());
    let fits = |e: &&XauthEntry| {
        e.number == number.as_bytes()
            && match e.family {
                family::WILD | family::LOCAL => true,
                family::INTERNET => {
                    (display.is_local() && e.address == [127, 0, 0, 1])
                        || host_v4.is_some_and(|a| e.address == a)
                }
                _ => false,
            }
    };
    let mut found = entries.iter().filter(fits);
    let first = found.clone().next()?;
    let best = found.find(|e| e.name == MIT_COOKIE).unwrap_or(first);
    Some((best.name.clone(), best.data.clone()))
}

/// What to do with the bytes an X client on the server sent so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setup {
    /// The setup is not complete yet.
    More,
    /// The setup carried the fake cookie: send these bytes (the setup with
    /// the real authorisation, and whatever followed it) to the X server.
    Ready(Vec<u8>),
    /// Not an X11 setup, or the wrong cookie: close the channel.
    Refuse,
}

/// Compares without stopping at the first difference, so the time taken
/// says nothing about how much of a guessed cookie was right.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn pad4(n: usize) -> usize {
    (4 - n % 4) % 4
}

/// Checks a client's connection setup against the fake cookie and swaps in
/// the real authorisation (`real_name` and `real_data`, both empty for an
/// X server without access control).
pub fn swap_cookie(buf: &[u8], fake: &[u8], real_name: &str, real_data: &[u8]) -> Setup {
    let Some(&order) = buf.first() else {
        return Setup::More;
    };
    let read16: fn([u8; 2]) -> u16 = match order {
        b'B' => u16::from_be_bytes,
        b'l' => u16::from_le_bytes,
        _ => return Setup::Refuse,
    };
    let write16: fn(u16) -> [u8; 2] = if order == b'B' {
        u16::to_be_bytes
    } else {
        u16::to_le_bytes
    };
    let Some(head) = buf.get(..12) else {
        return Setup::More;
    };
    let field = |at: usize| {
        head.get(at..at + 2)
            .and_then(|b| b.try_into().ok())
            .map(read16)
            .map(usize::from)
            .unwrap_or(0)
    };
    let (name_len, data_len) = (field(6), field(8));
    if name_len > MAX_AUTH_FIELD || data_len > MAX_AUTH_FIELD {
        return Setup::Refuse;
    }
    let name_at = 12;
    let data_at = name_at + name_len + pad4(name_len);
    let end = data_at + data_len + pad4(data_len);
    let (Some(name), Some(data), Some(rest)) = (
        buf.get(name_at..name_at + name_len),
        buf.get(data_at..data_at + data_len),
        buf.get(end..),
    ) else {
        return Setup::More;
    };
    if name != MIT_COOKIE.as_bytes() || !same(data, fake) {
        return Setup::Refuse;
    }
    let (Ok(n), Ok(d)) = (
        u16::try_from(real_name.len()),
        u16::try_from(real_data.len()),
    ) else {
        return Setup::Refuse;
    };
    let mut out = Vec::with_capacity(12 + real_name.len() + real_data.len() + 6 + rest.len());
    out.extend_from_slice(head.get(..6).unwrap_or_default());
    out.extend_from_slice(&write16(n));
    out.extend_from_slice(&write16(d));
    out.extend_from_slice(head.get(10..12).unwrap_or_default());
    out.extend_from_slice(real_name.as_bytes());
    out.resize(out.len() + pad4(real_name.len()), 0);
    out.extend_from_slice(real_data);
    out.resize(out.len() + pad4(real_data.len()), 0);
    out.extend_from_slice(rest);
    Setup::Ready(out)
}

/// The longest setup worth waiting for: the header and two padded fields.
pub const MAX_SETUP: usize = 12 + 2 * (MAX_AUTH_FIELD + 3);

/// Lower-case hex, as the `x11-req` request carries the cookie.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn parses_displays() {
        let d = Display::parse("localhost:0.0").unwrap();
        assert_eq!(
            (d.host.as_str(), d.number, d.screen, d.port()),
            ("localhost", 0, 0, 6000)
        );
        let d = Display::parse(":1").unwrap();
        assert_eq!((d.host.as_str(), d.port()), ("localhost", 6001));
        let d = Display::parse("192.168.1.5:2.1").unwrap();
        assert_eq!((d.number, d.screen), (2, 1));
        let d = Display::parse("[::1]:0").unwrap();
        assert_eq!(d.host, "::1");
        assert_eq!(d.to_string(), "[::1]:0.0");
        for bad in [
            "",
            "localhost",
            "unix:0",
            "/tmp/.X11-unix/X0:0",
            "host:x",
            "host:60000",
        ] {
            assert_eq!(Display::parse(bad), None, "{bad}");
        }
    }

    fn entry(family: u16, address: &[u8], number: &str, name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = family.to_be_bytes().to_vec();
        for f in [address, number.as_bytes(), name.as_bytes(), data] {
            out.extend_from_slice(&(f.len() as u16).to_be_bytes());
            out.extend_from_slice(f);
        }
        out
    }

    #[test]
    fn finds_cookies_in_xauthority() {
        let mut file = entry(family::INTERNET, &[10, 0, 0, 1], "0", MIT_COOKIE, &[1; 16]);
        file.extend(entry(family::LOCAL, b"MYPC", "1", MIT_COOKIE, &[2; 16]));
        file.extend(entry(
            family::INTERNET,
            &[127, 0, 0, 1],
            "0",
            MIT_COOKIE,
            &[3; 16],
        ));
        file.extend([0, 0, 0, 9]);
        let entries = xauth_entries(&file);
        assert_eq!(entries.len(), 3);
        let local0 = Display::parse("localhost:0").unwrap();
        assert_eq!(find_cookie(&entries, &local0).unwrap().1, vec![3; 16]);
        let local1 = Display::parse(":1").unwrap();
        assert_eq!(find_cookie(&entries, &local1).unwrap().1, vec![2; 16]);
        let remote = Display::parse("10.0.0.1:0").unwrap();
        assert_eq!(find_cookie(&entries, &remote).unwrap().1, vec![1; 16]);
        let none = Display::parse(":7").unwrap();
        assert_eq!(find_cookie(&entries, &none), None);
        assert!(!format!("{:?}", entries[0]).contains("\\u{1}"));
    }

    fn setup(order: u8, name: &[u8], data: &[u8]) -> Vec<u8> {
        let w = |v: u16| {
            if order == b'B' {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut out = vec![order, 0];
        out.extend(w(11));
        out.extend(w(0));
        out.extend(w(name.len() as u16));
        out.extend(w(data.len() as u16));
        out.extend([0, 0]);
        out.extend_from_slice(name);
        out.resize(out.len() + pad4(name.len()), 0);
        out.extend_from_slice(data);
        out.resize(out.len() + pad4(data.len()), 0);
        out
    }

    #[test]
    fn swaps_the_fake_cookie_for_the_real_one() {
        let fake = [7u8; 16];
        let real = [9u8; 16];
        for order in *b"Bl" {
            let mut sent = setup(order, MIT_COOKIE.as_bytes(), &fake);
            sent.extend_from_slice(b"more");
            for cut in 0..sent.len() - 4 {
                assert_eq!(
                    swap_cookie(&sent[..cut], &fake, MIT_COOKIE, &real),
                    Setup::More
                );
            }
            let mut want = setup(order, MIT_COOKIE.as_bytes(), &real);
            want.extend_from_slice(b"more");
            assert_eq!(
                swap_cookie(&sent, &fake, MIT_COOKIE, &real),
                Setup::Ready(want)
            );
            // An X server without access control gets no authorisation.
            let open = setup(order, b"", b"");
            assert_eq!(
                swap_cookie(&sent[..sent.len() - 4], &fake, "", &[]),
                Setup::Ready(open)
            );
        }
    }

    #[test]
    fn refuses_wrong_cookies_and_junk() {
        let fake = [7u8; 16];
        let wrong = setup(b'l', MIT_COOKIE.as_bytes(), &[8; 16]);
        assert_eq!(swap_cookie(&wrong, &fake, "", &[]), Setup::Refuse);
        let other = setup(b'l', b"XDM-AUTHORIZATION-1", &fake);
        assert_eq!(swap_cookie(&other, &fake, "", &[]), Setup::Refuse);
        let none = setup(b'l', b"", b"");
        assert_eq!(swap_cookie(&none, &fake, "", &[]), Setup::Refuse);
        assert_eq!(swap_cookie(b"GET /", &fake, "", &[]), Setup::Refuse);
        let mut huge = setup(b'l', b"", b"");
        huge[6] = 0xff;
        huge[7] = 0xff;
        assert_eq!(swap_cookie(&huge, &fake, "", &[]), Setup::Refuse);
    }

    #[test]
    fn hex_cookies() {
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
    }
}
