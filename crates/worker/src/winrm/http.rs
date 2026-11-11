//! The HTTP/1.1 a WinRM client needs: POST requests on one kept-alive
//! connection, and responses parsed from untrusted bytes with their
//! header and body sizes capped.

/// The longest response head (status line and headers).
pub const MAX_HEAD: usize = 32 * 1024;
/// The largest response body. WinRM keeps envelopes under the
/// `MaxEnvelopeSize` FeroSSH asks for (150 KiB); encryption adds little.
pub const MAX_BODY: usize = 512 * 1024;
/// Headers in one response.
const MAX_HEADERS: usize = 64;

/// Why a response was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad HTTP response ({})", self.0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header called `name` (in any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers_named(name).into_iter().next()
    }

    pub fn headers_named(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// The token in a `WWW-Authenticate: Negotiate <base64>` header:
    /// `Some(empty)` for a bare `Negotiate`, `None` without one.
    pub fn negotiate_token(&self) -> Result<Option<Vec<u8>>, Error> {
        for value in self.headers_named("WWW-Authenticate") {
            // Several challenges may share one header, comma-separated;
            // a Negotiate token is base64 and has no commas.
            for challenge in value.split(',') {
                let challenge = challenge.trim();
                let Some(scheme) = challenge.get(..9) else {
                    continue;
                };
                if !scheme.eq_ignore_ascii_case("Negotiate") {
                    continue;
                }
                let rest = challenge.get(9..).unwrap_or_default();
                if !rest.is_empty() && !rest.starts_with(' ') {
                    continue;
                }
                let b64 = rest.trim();
                if b64.is_empty() {
                    return Ok(Some(Vec::new()));
                }
                return super::base64_decode(b64)
                    .map(Some)
                    .ok_or(Error("bad Negotiate token"));
            }
        }
        Ok(None)
    }
}

/// A POST request for `path` on `host` (the `Host` header's value).
pub fn post(host: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("POST {path} HTTP/1.1\r\nHost: {host}\r\n");
    for (name, value) in headers {
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    out.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut out = out.into_bytes();
    out.extend_from_slice(body);
    out
}

/// Takes one whole response off the front of `buf`, or `None` while more
/// bytes are needed.
pub fn take_response(buf: &mut Vec<u8>) -> Result<Option<Response>, Error> {
    let Some(head_end) = find(buf, b"\r\n\r\n") else {
        if buf.len() > MAX_HEAD {
            return Err(Error("header too long"));
        }
        return Ok(None);
    };
    if head_end > MAX_HEAD {
        return Err(Error("header too long"));
    }
    let head = std::str::from_utf8(buf.get(..head_end).unwrap_or_default())
        .map_err(|_| Error("header is not text"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(Error("not HTTP/1.x"));
    }
    let status = parts
        .next()
        .filter(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or(Error("bad status"))?;
    let mut headers = Vec::new();
    for line in lines {
        if headers.len() >= MAX_HEADERS {
            return Err(Error("too many headers"));
        }
        let (name, value) = line.split_once(':').ok_or(Error("bad header"))?;
        if name.is_empty() || name.bytes().any(|b| b <= b' ' || b >= 0x7f) {
            return Err(Error("bad header name"));
        }
        headers.push((name.to_owned(), value.trim().to_owned()));
    }
    let mut response = Response {
        status,
        headers,
        body: Vec::new(),
    };
    let body_start = head_end + 4;
    let chunked = response
        .header("Transfer-Encoding")
        .is_some_and(|t| t.eq_ignore_ascii_case("chunked"));
    let used = if chunked {
        let Some((body, used)) = dechunk(buf.get(body_start..).unwrap_or_default())? else {
            return Ok(None);
        };
        response.body = body;
        used
    } else if (100..200).contains(&status) || status == 204 || status == 304 {
        0
    } else {
        let len = response
            .header("Content-Length")
            .ok_or(Error("no Content-Length"))?
            .parse::<usize>()
            .map_err(|_| Error("bad Content-Length"))?;
        if len > MAX_BODY {
            return Err(Error("body too large"));
        }
        let Some(body) = buf.get(body_start..body_start + len) else {
            return Ok(None);
        };
        response.body = body.to_vec();
        len
    };
    buf.drain(..body_start + used);
    Ok(Some(response))
}

/// A chunked body at the front of `data`, and the bytes it took, or
/// `None` while incomplete.
fn dechunk(data: &[u8]) -> Result<Option<(Vec<u8>, usize)>, Error> {
    let mut body = Vec::new();
    let mut at = 0usize;
    loop {
        let rest = data.get(at..).unwrap_or_default();
        let Some(eol) = find(rest, b"\r\n") else {
            if rest.len() > 1024 {
                return Err(Error("bad chunk size"));
            }
            return Ok(None);
        };
        let line = std::str::from_utf8(rest.get(..eol).unwrap_or_default())
            .map_err(|_| Error("bad chunk size"))?;
        let size_text = line.split(';').next().unwrap_or_default().trim();
        if size_text.is_empty() || size_text.len() > 8 {
            return Err(Error("bad chunk size"));
        }
        let size = usize::from_str_radix(size_text, 16).map_err(|_| Error("bad chunk size"))?;
        at += eol + 2;
        if size == 0 {
            // Trailers, then the empty line.
            loop {
                let rest = data.get(at..).unwrap_or_default();
                let Some(eol) = find(rest, b"\r\n") else {
                    if rest.len() > MAX_HEAD {
                        return Err(Error("trailer too long"));
                    }
                    return Ok(None);
                };
                at += eol + 2;
                if eol == 0 {
                    return Ok(Some((body, at)));
                }
            }
        }
        if body.len() + size > MAX_BODY {
            return Err(Error("body too large"));
        }
        let Some(chunk) = data.get(at..at + size) else {
            return Ok(None);
        };
        body.extend_from_slice(chunk);
        at += size;
        match data.get(at..at + 2) {
            Some(b"\r\n") => at += 2,
            Some(_) => return Err(Error("chunk without CRLF")),
            None => return Ok(None),
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn takes_a_response_with_a_length_once_complete() {
        let wire = b"HTTP/1.1 401 \r\nWWW-Authenticate: Negotiate\r\nWWW-Authenticate: Kerberos\r\nContent-Length: 3\r\n\r\nabcHTTP/1.1";
        for cut in 0..wire.len() - 11 {
            let mut buf = wire.get(..cut).unwrap().to_vec();
            assert_eq!(take_response(&mut buf).unwrap(), None, "{cut}");
        }
        let mut buf = wire.to_vec();
        let r = take_response(&mut buf).unwrap().unwrap();
        assert_eq!(r.status, 401);
        assert_eq!(r.body, b"abc");
        assert_eq!(r.negotiate_token().unwrap(), Some(Vec::new()));
        assert_eq!(buf, b"HTTP/1.1");
    }

    #[test]
    fn reads_chunked_bodies_and_tokens() {
        let mut buf = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nwww-authenticate: Negotiate oRQwEqADCgEA\r\n\r\n3;x=y\r\nabc\r\n2\r\nde\r\n0\r\n\r\n".to_vec();
        let r = take_response(&mut buf).unwrap().unwrap();
        assert_eq!(r.body, b"abcde");
        assert_eq!(
            r.negotiate_token().unwrap(),
            Some(vec![0xa1, 0x14, 0x30, 0x12, 0xa0, 0x03, 0x0a, 0x01, 0x00])
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn refuses_bad_or_huge_responses() {
        for bad in [
            &b"SSH-2.0-x\r\n\r\n"[..],
            b"HTTP/1.1 2000 x\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n",
            b"HTTP/1.1 200 OK\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nabc",
        ] {
            let mut buf = bad.to_vec();
            assert!(
                take_response(&mut buf).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        let mut long = vec![b'a'; MAX_HEAD + 1];
        assert!(take_response(&mut long).is_err());
    }

    #[test]
    fn writes_a_post() {
        let req = post(
            "h:5985",
            "/wsman",
            &[("Authorization", "Negotiate x")],
            b"hi",
        );
        assert_eq!(
            req,
            b"POST /wsman HTTP/1.1\r\nHost: h:5985\r\nAuthorization: Negotiate x\r\nContent-Length: 2\r\n\r\nhi"
        );
    }
}
