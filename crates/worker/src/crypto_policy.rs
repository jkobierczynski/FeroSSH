//! The only algorithms FeroSSH will negotiate.
//!
//! russh compiles legacy algorithms in unconditionally, so this list is the
//! enforcement point: every client config is built from [`preferred`], and
//! the only per-host addition is [`with_legacy`], an opt-in set for older
//! switches, routers and SSH servers that is still free of SHA-1, CBC, DSA,
//! small DH groups and compression. The tests below fail if a banned
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

/// The opt-in legacy set (`LegacyAlgorithms yes` in a host's profile),
/// tried after everything above.
const LEGACY_KEX: &[kex::Name] = &[
    kex::ECDH_SHA2_NISTP256,
    kex::ECDH_SHA2_NISTP384,
    kex::ECDH_SHA2_NISTP521,
    kex::DH_G16_SHA512,
    kex::DH_G18_SHA512,
    kex::DH_G14_SHA256,
];

const LEGACY_CIPHERS: &[cipher::Name] = &[cipher::AES_256_CTR, cipher::AES_128_CTR];

/// Plain (encrypt-and-MAC) HMACs: many switches offer no EtM ones.
const LEGACY_MACS: &[mac::Name] = &[mac::HMAC_SHA512, mac::HMAC_SHA256];

const LEGACY_HOST_KEYS: &[Algorithm] = &[
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP384,
    },
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP521,
    },
];

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

/// The pseudo key type the UI lists in `known_types` when known_hosts has
/// an `@cert-authority` line for the host.
pub const CERT_AUTHORITY: &str = "@cert-authority";

/// [`preferred`] with the host key algorithms for `known_types` (the key
/// types known_hosts lists for the host) moved to the front, so a server
/// with several host keys presents the one already trusted. With
/// [`CERT_AUTHORITY`] among them, host certificates of the same
/// algorithms are asked for first. Nothing outside the policy can be added.
pub fn preferred_for(known_types: &[String]) -> Preferred {
    let mut keys: Vec<Algorithm> = HOST_KEYS.to_vec();
    keys.sort_by_key(|a| !known_types.iter().any(|t| t == key_type(a)));
    let certificates = if known_types.iter().any(|t| t == CERT_AUTHORITY) {
        Cow::Borrowed(HOST_KEYS)
    } else {
        Cow::Borrowed(&[][..])
    };
    Preferred {
        key: Cow::Owned(keys),
        host_key_certificates: certificates,
        ..preferred()
    }
}

/// `p` without what [`terrapin_prone`] rejects, for a server that lacks
/// strict key exchange: no ChaCha20-Poly1305, and no EtM MACs (which only
/// the legacy CTR ciphers use).
pub fn without_terrapin(p: Preferred) -> Preferred {
    Preferred {
        cipher: Cow::Owned(
            p.cipher
                .iter()
                .filter(|c| **c != cipher::CHACHA20_POLY1305)
                .cloned()
                .collect(),
        ),
        mac: Cow::Owned(
            p.mac
                .iter()
                .filter(|m| !m.as_ref().ends_with("-etm@openssh.com"))
                .cloned()
                .collect(),
        ),
        ..p
    }
}

/// `p` with the legacy set appended, each list keeping its order.
pub fn with_legacy(p: Preferred) -> Preferred {
    fn add<T: Clone>(list: &[T], more: &[T]) -> Vec<T> {
        list.iter().chain(more).cloned().collect()
    }
    // The strict-kex and ext-info markers stay last.
    let (algs, markers): (Vec<_>, Vec<_>) = p.kex.iter().cloned().partition(|k| {
        *k != kex::EXTENSION_SUPPORT_AS_CLIENT && *k != kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT
    });
    let certificates = if p.host_key_certificates.is_empty() {
        p.host_key_certificates.clone()
    } else {
        Cow::Owned(add(&p.host_key_certificates, LEGACY_HOST_KEYS))
    };
    Preferred {
        kex: Cow::Owned([algs, LEGACY_KEX.to_vec(), markers].concat()),
        key: Cow::Owned(add(&p.key, LEGACY_HOST_KEYS)),
        host_key_certificates: certificates,
        cipher: Cow::Owned(add(&p.cipher, LEGACY_CIPHERS)),
        mac: Cow::Owned(add(&p.mac, LEGACY_MACS)),
        ..p
    }
}

/// Whether `name` (a kex, cipher, MAC or host key algorithm) is only in
/// the legacy set, so a connection that negotiated it gets the warning
/// badge.
pub fn is_legacy(name: &str) -> bool {
    LEGACY_KEX.iter().any(|n| n.as_ref() == name)
        || LEGACY_CIPHERS.iter().any(|n| n.as_ref() == name)
        || LEGACY_MACS.iter().any(|n| n.as_ref() == name)
        || LEGACY_HOST_KEYS.iter().any(|a| a.as_str() == name)
}

/// Whether the server's offer for one kind of algorithm meets the legacy
/// set, so turning it on would help.
pub fn legacy_would_help(theirs: &[String]) -> bool {
    theirs.iter().any(|n| is_legacy(n))
}

/// Whether the negotiated cipher and MAC are open to Terrapin
/// (CVE-2023-48795) without strict key exchange: ChaCha20-Poly1305, or a
/// CTR cipher with an EtM MAC. AES-GCM, and CTR with a plain MAC, are not.
pub fn terrapin_prone(cipher: &cipher::Name, mac: &mac::Name) -> bool {
    *cipher == cipher::CHACHA20_POLY1305
        || (LEGACY_CIPHERS.contains(cipher) && mac.as_ref().ends_with("-etm@openssh.com"))
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
    fn legacy_set_still_bans_the_broken_algorithms() {
        let banned = |n: &str| {
            n.contains("sha1")
                || n.ends_with("-cbc")
                || n.contains("3des")
                || n.contains("dss")
                || n == "ssh-rsa"
                || n.starts_with("zlib")
                || n == "diffie-hellman-group1-sha1"
                || n.starts_with("diffie-hellman-group-exchange")
        };
        let p = with_legacy(preferred());
        assert!(!p.cipher.contains(&cipher::NONE) && !p.mac.contains(&mac::NONE));
        let offered = names(&p);
        let bad: Vec<_> = offered.iter().filter(|n| banned(n)).collect();
        assert!(bad.is_empty(), "banned algorithms offered: {bad:?}");
        // Modern ones keep priority; the markers stay at the end.
        assert_eq!(p.kex.first(), Some(&kex::MLKEM768X25519_SHA256));
        assert_eq!(
            p.kex.last(),
            Some(&kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT)
        );
        assert_eq!(p.cipher.first(), Some(&cipher::CHACHA20_POLY1305));
        assert!(p.kex.contains(&kex::ECDH_SHA2_NISTP256));
        assert!(p.cipher.contains(&cipher::AES_256_CTR));
        assert!(p.mac.contains(&mac::HMAC_SHA256));
    }

    #[test]
    fn legacy_names_are_recognised() {
        assert!(is_legacy("ecdh-sha2-nistp256"));
        assert!(is_legacy("aes128-ctr"));
        assert!(is_legacy("hmac-sha2-256"));
        assert!(is_legacy("ecdsa-sha2-nistp521"));
        assert!(!is_legacy("curve25519-sha256"));
        assert!(!is_legacy("aes256-cbc"));
        assert!(legacy_would_help(&[
            "aes256-cbc".into(),
            "aes128-ctr".into()
        ]));
        assert!(!legacy_would_help(&[
            "aes256-cbc".into(),
            "3des-cbc".into()
        ]));
    }

    #[test]
    fn terrapin_rules() {
        assert!(terrapin_prone(&cipher::CHACHA20_POLY1305, &mac::NONE));
        assert!(terrapin_prone(&cipher::AES_128_CTR, &mac::HMAC_SHA256_ETM));
        assert!(!terrapin_prone(&cipher::AES_128_CTR, &mac::HMAC_SHA256));
        assert!(!terrapin_prone(&cipher::AES_256_GCM, &mac::NONE));
        let p = without_terrapin(with_legacy(preferred()));
        assert!(!p.cipher.contains(&cipher::CHACHA20_POLY1305));
        assert!(
            p.mac
                .iter()
                .all(|m| !m.as_ref().ends_with("-etm@openssh.com"))
        );
    }

    #[test]
    fn certificates_only_with_a_cert_authority() {
        assert!(
            preferred_for(&["ssh-ed25519".into()])
                .host_key_certificates
                .is_empty()
        );
        let p = preferred_for(&[CERT_AUTHORITY.into()]);
        assert_eq!(&p.host_key_certificates[..], HOST_KEYS);
        assert!(with_legacy(preferred()).host_key_certificates.is_empty());
        let legacy = with_legacy(p);
        assert!(
            LEGACY_HOST_KEYS
                .iter()
                .all(|a| legacy.host_key_certificates.contains(a))
        );
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
