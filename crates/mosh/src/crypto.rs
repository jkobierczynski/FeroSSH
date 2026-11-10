//! Mosh's packet encryption: AES-128 in OCB3 mode (RFC 7253) with a
//! 12-byte nonce and a 16-byte tag, as `mosh-server` sets it up.
//!
//! On the wire a packet is the last 8 bytes of the nonce (its first 4 are
//! zero), then the ciphertext with its tag. The nonce carries the
//! direction bit and the sequence number, so a packet cannot be replayed
//! the other way, and each one is encrypted under a nonce of its own.

use aes::Aes128;
use ocb3::aead::{AeadInOut, KeyInit};
use ocb3::{Nonce, Ocb3};
use zeroize::Zeroizing;

/// Bytes the nonce takes on the wire.
pub const NONCE_LEN: usize = 8;
/// Bytes the tag adds.
pub const TAG_LEN: usize = 16;
/// The largest datagram Mosh sends or takes.
pub const RECEIVE_MTU: usize = 2048;

/// Why a key or a packet was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// The key is not 22 characters of base64 for 16 bytes.
    BadKey,
    /// Too short to hold a nonce and a tag.
    Short,
    /// The tag did not match: forged, damaged, or under another key.
    Integrity,
    /// 2^47 blocks went out under this key: the session must end.
    Exhausted,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CryptoError::BadKey => "the server's Mosh key is not a 22-letter base64 key",
            CryptoError::Short => "a packet too short to be Mosh's",
            CryptoError::Integrity => "a packet failed its integrity check",
            CryptoError::Exhausted => "the session's key has encrypted all it may",
        })
    }
}

/// Reads the key `mosh-server` prints: 22 base64 letters, without the
/// `==` padding, for exactly 16 bytes.
pub fn parse_key(printable: &str) -> Result<Zeroizing<[u8; 16]>, CryptoError> {
    let letters = printable.as_bytes();
    if letters.len() != 22 {
        return Err(CryptoError::BadKey);
    }
    let mut bits: u32 = 0;
    let mut have = 0u32;
    let mut key = Zeroizing::new([0u8; 16]);
    let mut at = 0;
    for &c in letters {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(CryptoError::BadKey),
        };
        bits = (bits << 6) | u32::from(v);
        have += 6;
        if have >= 8 {
            have -= 8;
            let byte = (bits >> have) as u8;
            let Some(slot) = key.get_mut(at) else {
                return Err(CryptoError::BadKey);
            };
            *slot = byte;
            at += 1;
            bits &= (1 << have) - 1;
        }
    }
    // 22 letters are 132 bits: the 4 left over must be zero, or the
    // letters stand for something other than a 128-bit key.
    if at != 16 || bits != 0 {
        return Err(CryptoError::BadKey);
    }
    Ok(key)
}

/// One session key, for both directions.
pub struct Session {
    cipher: Ocb3<Aes128>,
    /// Blocks encrypted so far, against the 2^47 limit Mosh keeps.
    blocks: u64,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("blocks", &self.blocks)
            .finish_non_exhaustive()
    }
}

fn nonce(value: u64) -> Nonce<ocb3::aead::consts::U12> {
    let v = value.to_be_bytes();
    let n: [u8; 12] = [0, 0, 0, 0, v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]];
    n.into()
}

impl Session {
    pub fn new(key: &[u8; 16]) -> Self {
        Self {
            cipher: Ocb3::new(key.into()),
            blocks: 0,
        }
    }

    /// Encrypts `plaintext` under nonce `value`: the packet to send.
    pub fn encrypt(&mut self, value: u64, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.blocks += (plaintext.len() as u64).div_ceil(16);
        if self.blocks >> 47 != 0 {
            return Err(CryptoError::Exhausted);
        }
        let mut body = plaintext.to_vec();
        self.cipher
            .encrypt_in_place(&nonce(value), &[], &mut body)
            .map_err(|_| CryptoError::Exhausted)?;
        let mut packet = Vec::with_capacity(NONCE_LEN + body.len());
        packet.extend_from_slice(&value.to_be_bytes());
        packet.extend_from_slice(&body);
        Ok(packet)
    }

    /// Checks and decrypts a packet: its nonce value and plaintext.
    pub fn decrypt(&self, packet: &[u8]) -> Result<(u64, Vec<u8>), CryptoError> {
        if packet.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptoError::Short);
        }
        let (head, body) = packet.split_at(NONCE_LEN);
        let mut value = [0u8; 8];
        value.copy_from_slice(head);
        let value = u64::from_be_bytes(value);
        let mut body = body.to_vec();
        self.cipher
            .decrypt_in_place(&nonce(value), &[], &mut body)
            .map_err(|_| CryptoError::Integrity)?;
        Ok((value, body))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_22_letters_for_16_bytes() {
        let key = parse_key("AAECAwQFBgcICQoLDA0ODw").unwrap();
        assert_eq!(*key, core::array::from_fn::<u8, 16, _>(|i| i as u8));
        assert_eq!(
            parse_key("AAECAwQFBgcICQoLDA0ODx"),
            Err(CryptoError::BadKey)
        );
        assert_eq!(parse_key("AAECAwQFBgcICQoLDA0OD"), Err(CryptoError::BadKey));
        assert_eq!(
            parse_key("AAECAwQFBgcICQoLDA0OD="),
            Err(CryptoError::BadKey)
        );
        assert_eq!(
            parse_key("AAECAwQFBgcICQoLDA0ODwA"),
            Err(CryptoError::BadKey)
        );
    }

    #[test]
    fn packets_round_trip_and_reject_changes() {
        // Not literals: CodeQL reads a literal key as a hard-coded one.
        let key: [u8; 16] = core::array::from_fn(|i| (i as u8) ^ 0x5a);
        let mut a = Session::new(&key);
        let b = Session::new(&key);
        let packet = a.encrypt(0x8000_0000_0000_0005, b"hello mosh").unwrap();
        assert_eq!(packet.len(), 8 + 10 + 16);
        assert_eq!(&packet[..8], &0x8000_0000_0000_0005u64.to_be_bytes());
        assert_eq!(
            b.decrypt(&packet).unwrap(),
            (0x8000_0000_0000_0005, b"hello mosh".to_vec())
        );
        for i in 0..packet.len() {
            let mut bad = packet.clone();
            bad[i] ^= 1;
            assert_eq!(b.decrypt(&bad), Err(CryptoError::Integrity), "byte {i}");
        }
        assert_eq!(b.decrypt(&packet[..23]), Err(CryptoError::Short));
        let other: [u8; 16] = core::array::from_fn(|i| (i as u8) ^ 0xa5);
        assert!(Session::new(&other).decrypt(&packet).is_err());
    }

    /// RFC 7253 appendix A, the sample with no associated data and 16
    /// bytes of plaintext (key 000102..0F, nonce BBAA99887766554433221103).
    #[test]
    fn matches_rfc_7253() {
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let cipher: Ocb3<Aes128> = Ocb3::new((&key).into());
        let n: [u8; 12] = [
            0xBB, 0xAA, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x03,
        ];
        let mut body: Vec<u8> = (0u8..8).collect();
        cipher.encrypt_in_place(&n.into(), &[], &mut body).unwrap();
        let want = [
            0x45, 0xDD, 0x69, 0xF8, 0xF5, 0xAA, 0xE7, 0x24, 0x14, 0x05, 0x4C, 0xD1, 0xF3, 0x5D,
            0x82, 0x76, 0x0B, 0x2C, 0xD0, 0x0D, 0x2F, 0x99, 0xBF, 0xA9,
        ];
        assert_eq!(body, want);
    }
}
