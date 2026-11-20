//! Comparing a host key with SSHFP records from DNS (RFC 4255, 6594,
//! 7479), for `VerifyHostKeyDNS`.
//!
//! The worker fetches the records; this decides whether they vouch for
//! the key. As in OpenSSH, SHA-1 records of an algorithm are ignored when
//! SHA-256 ones exist for it, so a weaker hash cannot outvote a stronger
//! one.

/// What DNS says about a host key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A record for the key's algorithm holds its fingerprint.
    Match,
    /// There are records for the key's algorithm, and none holds its
    /// fingerprint: DNS names another key.
    Mismatch,
    /// No usable record for this kind of key (none at all, or only for
    /// other algorithms or unknown hashes).
    NotListed,
}

/// The SSHFP algorithm number of an SSH wire-format public key; `None`
/// for kinds SSHFP has none for (security keys, certificates).
pub fn algorithm(blob: &[u8]) -> Option<u8> {
    Some(match crate::known_hosts::key_type(blob)? {
        "ssh-rsa" => 1,
        "ssh-dss" => 2,
        "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" | "ecdsa-sha2-nistp521" => 3,
        "ssh-ed25519" => 4,
        "ssh-ed448" => 6,
        _ => return None,
    })
}

/// Compares `key` (SSH wire format) with records given as (algorithm,
/// fingerprint type, digest).
pub fn check(key: &[u8], records: &[(u8, u8, &[u8])]) -> Verdict {
    let Some(alg) = algorithm(key) else {
        return Verdict::NotListed;
    };
    let mine: Vec<&(u8, u8, &[u8])> = records.iter().filter(|r| r.0 == alg).collect();
    let has_sha256 = mine.iter().any(|r| r.1 == 2);
    let usable: Vec<(u8, &[u8])> = mine
        .iter()
        .filter(|r| r.1 == 2 || (r.1 == 1 && !has_sha256))
        .map(|r| (r.1, r.2))
        .collect();
    if usable.is_empty() {
        return Verdict::NotListed;
    }
    let sha256 = ring::digest::digest(&ring::digest::SHA256, key);
    let sha1 = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, key);
    let matched = usable.iter().any(|&(t, digest)| match t {
        2 => digest == sha256.as_ref(),
        _ => digest == sha1.as_ref(),
    });
    if matched {
        Verdict::Match
    } else {
        Verdict::Mismatch
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn key(kind: &str, body: &[u8]) -> Vec<u8> {
        let mut b = (kind.len() as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind.as_bytes());
        b.extend_from_slice(body);
        b
    }

    fn sha256(b: &[u8]) -> Vec<u8> {
        ring::digest::digest(&ring::digest::SHA256, b)
            .as_ref()
            .to_vec()
    }

    fn sha1(b: &[u8]) -> Vec<u8> {
        ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, b)
            .as_ref()
            .to_vec()
    }

    #[test]
    fn algorithms() {
        assert_eq!(algorithm(&key("ssh-ed25519", b"k")), Some(4));
        assert_eq!(algorithm(&key("ssh-rsa", b"k")), Some(1));
        assert_eq!(algorithm(&key("ecdsa-sha2-nistp384", b"k")), Some(3));
        assert_eq!(algorithm(&key("sk-ssh-ed25519@openssh.com", b"k")), None);
        assert_eq!(algorithm(b"\x00"), None);
    }

    #[test]
    fn matches_and_mismatches() {
        let k = key("ssh-ed25519", &[7; 32]);
        let other = key("ssh-ed25519", &[8; 32]);
        let good = sha256(&k);
        let bad = sha256(&other);
        assert_eq!(check(&k, &[(4, 2, &good)]), Verdict::Match);
        assert_eq!(check(&k, &[(4, 2, &bad), (4, 2, &good)]), Verdict::Match);
        assert_eq!(check(&k, &[(4, 2, &bad)]), Verdict::Mismatch);
        // Records for other kinds of key say nothing about this one.
        assert_eq!(check(&k, &[(1, 2, &good)]), Verdict::NotListed);
        assert_eq!(check(&k, &[]), Verdict::NotListed);
        // An unknown hash type is skipped.
        assert_eq!(check(&k, &[(4, 9, &good)]), Verdict::NotListed);
        // SHA-1 counts only without SHA-256 records for the algorithm.
        let s1 = sha1(&k);
        assert_eq!(check(&k, &[(4, 1, &s1)]), Verdict::Match);
        assert_eq!(check(&k, &[(4, 1, &s1), (4, 2, &bad)]), Verdict::Mismatch);
        assert_eq!(check(&k, &[(4, 1, &sha1(&other))]), Verdict::Mismatch);
        // A key SSHFP has no number for.
        let sk = key("sk-ssh-ed25519@openssh.com", &[7; 32]);
        assert_eq!(check(&sk, &[(4, 2, &sha256(&sk))]), Verdict::NotListed);
    }
}
