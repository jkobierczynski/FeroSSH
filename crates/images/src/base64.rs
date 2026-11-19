//! A forgiving base64 decoder: whitespace is skipped and padding is
//! optional, as hosts send it (iTerm2's `imgcat` wraps lines, kitty
//! chunks pad each part).

use crate::Error;

/// Decodes `text`, refusing more than `max` bytes out.
pub fn decode(text: &[u8], max: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity((text.len() / 4 * 3).min(max));
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut ended = false;
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => {
                ended = true;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return Err(Error::Invalid),
        };
        if ended {
            // Data after padding: kitty chunks each padded, joined.
            acc = 0;
            bits = 0;
            ended = false;
        }
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if out.len() >= max {
                return Err(Error::TooLarge);
            }
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// Encodes `data` with padding (for tests and tools that build
/// sequences).
pub fn encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |a, (i, &b)| a | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= c.len() {
                let at = (n >> (18 - 6 * i) & 63) as usize;
                s.push(char::from(T.get(at).copied().unwrap_or(b'A')));
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes() {
        assert_eq!(decode(b"aGVsbG8=", 100).ok(), Some(b"hello".to_vec()));
        assert_eq!(decode(b"aGVs\nbG8", 100).ok(), Some(b"hello".to_vec()));
        assert_eq!(decode(b"aGk=aGk=", 100).ok(), Some(b"hihi".to_vec()));
        assert_eq!(decode(b"aGVsbG8=", 3), Err(Error::TooLarge));
        assert_eq!(decode(b"a*b", 3), Err(Error::Invalid));
        assert_eq!(encode(b"hello"), "aGVsbG8=");
        assert_eq!(
            decode(encode(b"hi!x").as_bytes(), 9).ok(),
            Some(b"hi!x".to_vec())
        );
    }
}
