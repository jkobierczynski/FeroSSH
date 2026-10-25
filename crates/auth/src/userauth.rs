//! Checks what the worker asks the UI to sign.
//!
//! A compromised worker must not be able to use the user's keys as a
//! general signing oracle. So the UI signs only a well-formed SSH
//! `publickey` user-auth request (RFC 4252 section 7) for the user it is
//! logging in as, naming the key it was asked to sign with and an
//! algorithm FeroSSH allows for that key.

/// SSH_MSG_USERAUTH_REQUEST.
const USERAUTH_REQUEST: u8 = 50;
/// Session identifiers are exchange hashes: 32 or 64 bytes for the
/// algorithms FeroSSH negotiates.
const MAX_SESSION_ID: usize = 64;

/// Agent flags for RSA signatures (draft-miller-ssh-agent).
pub const SSH_AGENT_RSA_SHA2_256: u32 = 2;
pub const SSH_AGENT_RSA_SHA2_512: u32 = 4;

/// A request that passed the checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignTarget {
    /// The signature algorithm, such as `ssh-ed25519` or `rsa-sha2-512`.
    pub algorithm: String,
    /// Flags for the agent's sign request.
    pub flags: u32,
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let (len, rest) = self.0.split_first_chunk::<4>()?;
        let len = u32::from_be_bytes(*len) as usize;
        if len > rest.len() {
            return None;
        }
        let (s, rest) = rest.split_at(len);
        self.0 = rest;
        Some(s)
    }
}

/// The key type (first string) of an SSH wire-format public key.
pub fn key_type(key: &[u8]) -> Option<&str> {
    std::str::from_utf8(Reader(key).string()?).ok()
}

/// The signature algorithms FeroSSH allows for a key type.
pub fn allowed_algorithms(key_type: &str) -> &'static [&'static str] {
    match key_type {
        "ssh-ed25519" => &["ssh-ed25519"],
        "ecdsa-sha2-nistp256" => &["ecdsa-sha2-nistp256"],
        "sk-ssh-ed25519@openssh.com" => &["sk-ssh-ed25519@openssh.com"],
        "sk-ecdsa-sha2-nistp256@openssh.com" => &["sk-ecdsa-sha2-nistp256@openssh.com"],
        // Never "ssh-rsa": that is SHA-1.
        "ssh-rsa" => &["rsa-sha2-512", "rsa-sha2-256"],
        _ => &[],
    }
}

/// Checks that `data` is the user-auth request for `user` with `key`.
pub fn check(data: &[u8], user: &str, key: &[u8]) -> Result<SignTarget, &'static str> {
    let mut r = Reader(data);
    let session_id = r.string().ok_or("truncated")?;
    if session_id.is_empty() || session_id.len() > MAX_SESSION_ID {
        return Err("bad session identifier");
    }
    if r.byte() != Some(USERAUTH_REQUEST) {
        return Err("not a user-auth request");
    }
    if r.string() != Some(user.as_bytes()) {
        return Err("wrong user name");
    }
    if r.string() != Some(b"ssh-connection") {
        return Err("wrong service");
    }
    if r.string() != Some(b"publickey") {
        return Err("not a publickey request");
    }
    if r.byte() != Some(1) {
        return Err("not a signed request");
    }
    let algorithm = r
        .string()
        .and_then(|a| std::str::from_utf8(a).ok())
        .ok_or("bad algorithm")?;
    if r.string() != Some(key) {
        return Err("names a different key");
    }
    if !r.0.is_empty() {
        return Err("trailing data");
    }
    let kind = key_type(key).ok_or("malformed key")?;
    if !allowed_algorithms(kind).contains(&algorithm) {
        return Err("algorithm not allowed for this key");
    }
    let flags = match algorithm {
        "rsa-sha2-256" => SSH_AGENT_RSA_SHA2_256,
        "rsa-sha2-512" => SSH_AGENT_RSA_SHA2_512,
        _ => 0,
    };
    Ok(SignTarget {
        algorithm: algorithm.to_owned(),
        flags,
    })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
pub(crate) mod tests {
    use super::*;

    pub fn string(out: &mut Vec<u8>, s: &[u8]) {
        out.extend_from_slice(&(s.len() as u32).to_be_bytes());
        out.extend_from_slice(s);
    }

    pub fn request(user: &str, alg: &str, key: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        string(&mut d, &[7; 32]);
        d.push(USERAUTH_REQUEST);
        string(&mut d, user.as_bytes());
        string(&mut d, b"ssh-connection");
        string(&mut d, b"publickey");
        d.push(1);
        string(&mut d, alg.as_bytes());
        string(&mut d, key);
        d
    }

    fn key(kind: &str) -> Vec<u8> {
        let mut k = Vec::new();
        string(&mut k, kind.as_bytes());
        string(&mut k, b"material");
        k
    }

    #[test]
    fn accepts_a_real_request() {
        let ed = key("ssh-ed25519");
        assert_eq!(
            check(&request("me", "ssh-ed25519", &ed), "me", &ed),
            Ok(SignTarget {
                algorithm: "ssh-ed25519".into(),
                flags: 0
            })
        );
        let rsa = key("ssh-rsa");
        assert_eq!(
            check(&request("me", "rsa-sha2-512", &rsa), "me", &rsa).map(|t| t.flags),
            Ok(SSH_AGENT_RSA_SHA2_512)
        );
    }

    #[test]
    fn refuses_everything_else() {
        let ed = key("ssh-ed25519");
        let rsa = key("ssh-rsa");
        let good = request("me", "ssh-ed25519", &ed);
        assert!(check(&good, "root", &ed).is_err());
        assert!(check(&good, "me", &key("ssh-ed25519x")).is_err());
        assert!(check(&request("me", "ssh-rsa", &rsa), "me", &rsa).is_err());
        assert!(check(&request("me", "rsa-sha2-512", &ed), "me", &ed).is_err());
        assert!(
            check(
                &request("me", "ssh-dss", &key("ssh-dss")),
                "me",
                &key("ssh-dss")
            )
            .is_err()
        );
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(check(&trailing, "me", &ed).is_err());
        assert!(check(&good[..good.len() - 1], "me", &ed).is_err());
        assert!(check(b"arbitrary document to sign", "me", &ed).is_err());
    }
}
