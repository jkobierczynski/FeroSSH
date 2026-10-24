//! The only algorithms FeroSSH will negotiate.
//!
//! russh compiles legacy algorithms in unconditionally, so this list is the
//! enforcement point: every client config is built from [`preferred`], and no
//! per-host setting may add to it. The tests below fail if a banned
//! algorithm ever appears.

use std::borrow::Cow;

use russh::keys::{Algorithm, EcdsaCurve, HashAlg};
use russh::{Preferred, cipher, compression, kex, mac};

const KEX: &[kex::Name] = &[
    kex::MLKEM768X25519_SHA256,
    kex::CURVE25519,
    kex::CURVE25519_PRE_RFC_8731,
    // Pseudo-algorithms: extension negotiation and strict KEX (Terrapin).
    kex::EXTENSION_SUPPORT_AS_CLIENT,
    kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
];

const CIPHERS: &[cipher::Name] = &[
    cipher::CHACHA20_POLY1305,
    cipher::AES_256_GCM,
    cipher::AES_128_GCM,
];

// Only used if a future cipher needs a separate MAC; all ciphers above are AEAD.
const MACS: &[mac::Name] = &[mac::HMAC_SHA512_ETM, mac::HMAC_SHA256_ETM];

const HOST_KEYS: &[Algorithm] = &[
    Algorithm::Ed25519,
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP256,
    },
    Algorithm::Rsa {
        hash: Some(HashAlg::Sha512),
    },
    Algorithm::Rsa {
        hash: Some(HashAlg::Sha256),
    },
];

const COMPRESSION: &[compression::Name] = &[compression::NONE];

/// Minimum RSA modulus size accepted for host and user keys.
pub const MIN_RSA_BITS: usize = 3072;

/// The algorithm preferences every FeroSSH client config uses.
pub fn preferred() -> Preferred {
    Preferred {
        kex: Cow::Borrowed(KEX),
        key: Cow::Borrowed(HOST_KEYS),
        host_key_certificates: Cow::Borrowed(&[]),
        cipher: Cow::Borrowed(CIPHERS),
        mac: Cow::Borrowed(MACS),
        compression: Cow::Borrowed(COMPRESSION),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(p: &Preferred) -> Vec<String> {
        let mut all = Vec::new();
        all.extend(p.kex.iter().map(|n| n.as_ref().to_owned()));
        all.extend(p.cipher.iter().map(|n| n.as_ref().to_owned()));
        all.extend(p.mac.iter().map(|n| n.as_ref().to_owned()));
        all.extend(p.compression.iter().map(|n| n.as_ref().to_owned()));
        all.extend(p.key.iter().map(|a| a.as_str().to_owned()));
        all
    }

    #[test]
    fn offers_no_banned_algorithm() {
        let banned = |n: &str| {
            n.contains("sha1")
                || n.ends_with("-cbc")
                || n.ends_with("-ctr")
                || n.starts_with("diffie-hellman")
                || n.starts_with("ecdh-sha2-")
                || n.contains("dss")
                || n == "ssh-rsa"
                || n.starts_with("zlib")
                || (n.starts_with("hmac-") && !n.ends_with("-etm@openssh.com"))
        };
        let offered = names(&preferred());
        let bad: Vec<_> = offered.iter().filter(|n| banned(n)).collect();
        assert!(bad.is_empty(), "banned algorithms offered: {bad:?}");
    }

    #[test]
    fn prefers_post_quantum_kex_and_requires_strict_kex() {
        let p = preferred();
        assert_eq!(p.kex.first(), Some(&kex::MLKEM768X25519_SHA256));
        assert!(p.kex.contains(&kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT));
    }

    #[test]
    fn rsa_host_keys_use_sha2_only() {
        for alg in preferred().key.iter() {
            if let Algorithm::Rsa { hash } = alg {
                assert!(hash.is_some(), "ssh-rsa (SHA-1) must not be offered");
            }
        }
    }
}
