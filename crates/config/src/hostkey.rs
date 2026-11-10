//! How a public key is shown to the user: type, size, SHA-256 fingerprint
//! and OpenSSH's "randomart" picture.

use russh::keys::ssh_key::public::KeyData;
use russh::keys::{HashAlg, PublicKey};

/// A key ready to show in a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDescription {
    /// `ED25519`, `ECDSA`, `RSA`, `ED25519-SK`, ...
    pub kind: String,
    /// Key size in bits.
    pub bits: u32,
    /// `SHA256:...`, as `ssh-keygen -l` prints it.
    pub fingerprint: String,
    /// The 11-line randomart box, lines joined with `\n`.
    pub randomart: String,
}

/// The key's type label and size, as OpenSSH prints them.
pub fn kind_and_bits(key: &PublicKey) -> (String, u32) {
    let (kind, bits) = match key.key_data() {
        KeyData::Ed25519(_) => ("ED25519", 256),
        KeyData::SkEd25519(_) => ("ED25519-SK", 256),
        KeyData::Ecdsa(k) => ("ECDSA", ecdsa_bits(k.curve())),
        KeyData::SkEcdsaSha2NistP256(_) => ("ECDSA-SK", 256),
        KeyData::Rsa(k) => ("RSA", k.key_size()),
        _ => ("UNKNOWN", 0),
    };
    (kind.to_owned(), bits)
}

fn ecdsa_bits(curve: russh::keys::EcdsaCurve) -> u32 {
    match curve {
        russh::keys::EcdsaCurve::NistP256 => 256,
        russh::keys::EcdsaCurve::NistP384 => 384,
        russh::keys::EcdsaCurve::NistP521 => 521,
    }
}

/// Describes a key given in SSH wire format; `None` if it does not parse.
pub fn describe(blob: &[u8]) -> Option<KeyDescription> {
    let key = PublicKey::from_bytes(blob).ok()?;
    let (kind, bits) = kind_and_bits(&key);
    let fp = key.fingerprint(HashAlg::Sha256);
    Some(KeyDescription {
        randomart: fp.to_randomart(&format!("[{kind} {bits}]")),
        fingerprint: fp.to_string(),
        kind,
        bits,
    })
}

/// Describes an OpenSSH certificate by the key inside it; `None` if the
/// blob is not a certificate.
pub fn describe_certificate(blob: &[u8]) -> Option<KeyDescription> {
    let cert = russh::keys::Certificate::from_bytes(blob).ok()?;
    describe(
        &PublicKey::new(cert.public_key().clone(), "")
            .to_bytes()
            .ok()?,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn matches_ssh_keygen() {
        // `ssh-keygen -lv -E sha256` on this key prints the same.
        let key = PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINX+3J1AiintwNo3NPzRD4dq1fDjyMII0jItKqKOYfxb",
        )
        .unwrap();
        let d = describe(&key.to_bytes().unwrap()).unwrap();
        assert_eq!(d.kind, "ED25519");
        assert_eq!(d.bits, 256);
        assert_eq!(
            d.fingerprint,
            "SHA256:vWYvpuNm8Qqc7Z0RA0GFQtnBKGWwNkzadS6MX5uwbZ4"
        );
        let art = "\
+--[ED25519 256]--+
|    o+=*=+.      |
|   =.*+o+.       |
|  . B.+.+        |
|   . o * =       |
|      o S +      |
|     . =.. +     |
|      + Eo=      |
|       o++++     |
|       +==o..    |
+----[SHA256]-----+";
        assert_eq!(d.randomart.trim_end(), art);
    }

    #[test]
    fn rejects_garbage() {
        assert!(describe(b"\0\0\0\x03abc").is_none());
    }
}
