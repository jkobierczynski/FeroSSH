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

/// The key type a host key algorithm verifies with, as known_hosts names
/// it.
pub fn key_type(alg: &Algorithm) -> &'static str {
    match alg {
        Algorithm::Rsa { .. } => "ssh-rsa",
        Algorithm::Ed25519 => "ssh-ed25519",
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        } => "ecdsa-sha2-nistp256",
        _ => "",
    }
}

/// [`preferred`] with the host key algorithms for `known_types` (the key
/// types known_hosts lists for the host) moved to the front, so a server
/// with several host keys presents the one already trusted. Nothing outside
/// the policy can be added.
pub fn preferred_for(known_types: &[String]) -> Preferred {
    let mut keys: Vec<Algorithm> = HOST_KEYS.to_vec();
    keys.sort_by_key(|a| !known_types.iter().any(|t| t == key_type(a)));
    Preferred {
        key: Cow::Owned(keys),
        ..preferred()
    }
}

/// `p` without ChaCha20-Poly1305, for a server that lacks strict key
/// exchange (Terrapin, CVE-2023-48795). AES-GCM is not affected.
pub fn without_chacha(p: Preferred) -> Preferred {
    Preferred {
        cipher: Cow::Owned(
            p.cipher
                .iter()
                .filter(|c| **c != cipher::CHACHA20_POLY1305)
                .cloned()
                .collect(),
        ),
        ..p
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
    fn known_key_types_go_first_without_widening_the_policy() {
        let p = preferred_for(&["ssh-rsa".into(), "ssh-dss".into()]);
        assert_eq!(
            p.key.first(),
            Some(&Algorithm::Rsa {
                hash: Some(HashAlg::Sha512)
            })
        );
        assert_eq!(p.key.get(2), Some(&Algorithm::Ed25519));
        let mut got = names(&p);
        let mut want = names(&preferred());
        got.sort();
        want.sort();
        assert_eq!(got, want);
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
