//! WinRM's message encryption over HTTP (MS-WSMV 2.2.9.1): each body is
//! sealed with the login's security context and sent as a one-part
//! `multipart/encrypted` body. Sealing itself happens in the UI, which
//! holds the context; this is the framing around it, parsed from
//! untrusted bytes here.

/// The request's `Content-Type`.
pub const CONTENT_TYPE: &str = "multipart/encrypted;protocol=\"application/HTTP-SPNEGO-session-encrypted\";boundary=\"Encrypted Boundary\"";
const BOUNDARY: &[u8] = b"--Encrypted Boundary\r\n";
const END: &[u8] = b"--Encrypted Boundary--";
const PROTOCOL: &str = "application/HTTP-SPNEGO-session-encrypted";
const STREAM: &[u8] = b"Content-Type: application/octet-stream\r\n";

/// Whether a response's `Content-Type` says it is sealed.
pub fn is_sealed(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|t| {
        t.trim_start()
            .get(..19)
            .is_some_and(|p| p.eq_ignore_ascii_case("multipart/encrypted"))
            && t.contains(PROTOCOL)
    })
}

/// The body for a sealed message: the plain text was `plain_len` bytes;
/// `header` is the seal's signature and `data` the encrypted text.
pub fn frame(plain_len: usize, header: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(header.len() + data.len() + 256);
    out.extend_from_slice(BOUNDARY);
    out.extend_from_slice(format!("\tContent-Type: {PROTOCOL}\r\n").as_bytes());
    out.extend_from_slice(
        format!(
            "\tOriginalContent: type=application/soap+xml;charset=UTF-8;Length={plain_len}\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(BOUNDARY);
    out.push(b'\t');
    out.extend_from_slice(STREAM);
    let len = u32::try_from(header.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(data);
    out.extend_from_slice(END);
    out.extend_from_slice(b"\r\n");
    out
}

/// A sealed body taken apart: the plain text's length, the signature and
/// the encrypted text.
#[derive(Debug, PartialEq, Eq)]
pub struct Sealed<'a> {
    pub plain_len: usize,
    pub header: &'a [u8],
    pub data: &'a [u8],
}

/// Takes a sealed response body apart.
pub fn unframe(body: &[u8]) -> Result<Sealed<'_>, &'static str> {
    let rest = body.strip_prefix(BOUNDARY).ok_or("no boundary")?;
    let part_end = find(rest, BOUNDARY).ok_or("no second part")?;
    let part = std::str::from_utf8(rest.get(..part_end).unwrap_or_default())
        .map_err(|_| "bad part header")?;
    if !part.contains(PROTOCOL) {
        return Err("not SPNEGO-sealed");
    }
    let plain_len = part
        .split(['\r', '\n', ';'])
        .find_map(|f| f.trim().strip_prefix("Length="))
        .and_then(|n| n.trim().parse::<usize>().ok())
        .ok_or("no length")?;
    let rest = rest.get(part_end + BOUNDARY.len()..).unwrap_or_default();
    let rest = rest.strip_prefix(b"\t").unwrap_or(rest);
    let rest = rest.strip_prefix(STREAM).ok_or("no octet stream")?;
    let rest = rest
        .strip_suffix(b"\r\n")
        .unwrap_or(rest)
        .strip_suffix(END)
        .ok_or("no closing boundary")?;
    let (len, rest) = rest.split_first_chunk::<4>().ok_or("too short")?;
    let len = u32::from_le_bytes(*len) as usize;
    if len > rest.len() {
        return Err("signature longer than the body");
    }
    let (header, data) = rest.split_at(len);
    Ok(Sealed {
        plain_len,
        header,
        data,
    })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        // Encrypted bytes may hold the boundary and CR LF themselves.
        let data = b"\r\n--Encrypted Boundary\r\nxyz--Encrypted Boundary--";
        let body = frame(40, &[9; 16], data);
        let s = unframe(&body).unwrap();
        assert_eq!(s.plain_len, 40);
        assert_eq!(s.header, &[9; 16]);
        assert_eq!(s.data, data);
    }

    #[test]
    fn reads_what_windows_sends() {
        let mut body = b"--Encrypted Boundary\r\n\tContent-Type: application/HTTP-SPNEGO-session-encrypted\r\n\tOriginalContent: type=application/soap+xml;charset=UTF-8;Length=5\r\n--Encrypted Boundary\r\n\tContent-Type: application/octet-stream\r\n".to_vec();
        body.extend_from_slice(&2u32.to_le_bytes());
        body.extend_from_slice(b"SGabcde--Encrypted Boundary--\r\n");
        let s = unframe(&body).unwrap();
        assert_eq!(
            (s.plain_len, s.header, s.data),
            (5, &b"SG"[..], &b"abcde"[..])
        );
        assert!(is_sealed(Some(
            "multipart/encrypted;protocol=\"application/HTTP-SPNEGO-session-encrypted\";boundary=\"Encrypted Boundary\""
        )));
        assert!(!is_sealed(Some("application/soap+xml;charset=UTF-8")));
        assert!(!is_sealed(None));
    }

    #[test]
    fn refuses_broken_frames() {
        let good = frame(3, &[1; 4], b"abc");
        for cut in 0..good.len() - 2 {
            assert!(unframe(good.get(..cut).unwrap()).is_err(), "{cut}");
        }
        let mut long_sig = frame(3, &[], b"abc");
        let at = long_sig.len() - 3 - END.len() - 2 - 4;
        long_sig.splice(at..at + 4, 99u32.to_le_bytes());
        assert!(unframe(&long_sig).is_err());
    }
}
