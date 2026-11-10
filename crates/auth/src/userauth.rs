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

/// Whether a public key blob is an OpenSSH certificate.
pub fn is_certificate(key: &[u8]) -> bool {
    key_type(key).is_some_and(|t| t.ends_with("-cert-v01@openssh.com"))
}

/// The key type (first string) of an SSH wire-format public key.
pub fn key_type(key: &[u8]) -> Option<&str> {
    std::str::from_utf8(Reader(key).string()?).ok()
}

/// The signature algorithms FeroSSH allows for a key type. Certificates
/// (`*-cert-v01@openssh.com`) are named by their own algorithm.
pub fn allowed_algorithms(key_type: &str) -> &'static [&'static str] {
    match key_type {
        "ssh-ed25519" => &["ssh-ed25519"],
        "ecdsa-sha2-nistp256" => &["ecdsa-sha2-nistp256"],
        // PIV cards and some HSMs hold these; SHA-384 and SHA-512.
        "ecdsa-sha2-nistp384" => &["ecdsa-sha2-nistp384"],
        "ecdsa-sha2-nistp521" => &["ecdsa-sha2-nistp521"],
        "ssh-ed25519-cert-v01@openssh.com" => &["ssh-ed25519-cert-v01@openssh.com"],
        "ecdsa-sha2-nistp256-cert-v01@openssh.com" => &["ecdsa-sha2-nistp256-cert-v01@openssh.com"],
        "ecdsa-sha2-nistp384-cert-v01@openssh.com" => &["ecdsa-sha2-nistp384-cert-v01@openssh.com"],
        "ecdsa-sha2-nistp521-cert-v01@openssh.com" => &["ecdsa-sha2-nistp521-cert-v01@openssh.com"],
        "ssh-rsa-cert-v01@openssh.com" => &[
            "rsa-sha2-512-cert-v01@openssh.com",
            "rsa-sha2-256-cert-v01@openssh.com",
        ],
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
        "rsa-sha2-256" | "rsa-sha2-256-cert-v01@openssh.com" => SSH_AGENT_RSA_SHA2_256,
        "rsa-sha2-512" | "rsa-sha2-512-cert-v01@openssh.com" => SSH_AGENT_RSA_SHA2_512,
        _ => 0,
    };
    Ok(SignTarget {
        algorithm: algorithm.to_owned(),
        flags,
    })
}

/// What a program on a server asks a forwarded agent to sign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forwarded {
    /// A login to another server, as `user`. OpenSSH 8.9 and later name
    /// that server's host key in the request (`publickey-hostbound`).
    Login {
        user: String,
        algorithm: String,
        host_key: Option<Vec<u8>>,
    },
    /// An `SSHSIG` signature (`ssh-keygen -Y sign`, git's SSH signing) in
    /// this namespace.
    SshSig { namespace: String },
}

/// The `SSHSIG` magic preamble.
const SSHSIG: &[u8] = b"SSHSIG";

/// Checks a forwarded agent's sign request: a well-formed user-auth
/// request with `key` (for any user), or an `SSHSIG` blob; `flags` must
/// ask for the hash an RSA algorithm names, and never SHA-1. Anything
/// else is refused, so the forwarded agent is no general signing oracle
/// either.
pub fn check_forwarded(data: &[u8], key: &[u8], flags: u32) -> Result<Forwarded, &'static str> {
    let kind = key_type(key).ok_or("malformed key")?;
    let allowed = allowed_algorithms(kind);
    if allowed.is_empty() {
        return Err("a key type FeroSSH does not allow");
    }
    let rsa = kind == "ssh-rsa" || kind == "ssh-rsa-cert-v01@openssh.com";
    // Exactly one SHA-2 flag: both set, the agent may fall back to SHA-1.
    if rsa && flags != SSH_AGENT_RSA_SHA2_256 && flags != SSH_AGENT_RSA_SHA2_512 {
        return Err("an RSA signature with SHA-1");
    }
    if let Some(rest) = data.strip_prefix(SSHSIG) {
        let mut r = Reader(rest);
        let namespace = r
            .string()
            .and_then(|n| std::str::from_utf8(n).ok())
            .filter(|n| !n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_ascii_graphic()))
            .ok_or("an SSHSIG blob without a namespace")?;
        let _reserved = r.string().ok_or("truncated")?;
        if !matches!(r.string(), Some(b"sha256" | b"sha512")) {
            return Err("an SSHSIG blob with an unknown hash");
        }
        let hash = r.string().ok_or("truncated")?;
        if !matches!(hash.len(), 32 | 64) || !r.0.is_empty() {
            return Err("a malformed SSHSIG blob");
        }
        return Ok(Forwarded::SshSig {
            namespace: namespace.to_owned(),
        });
    }
    let mut r = Reader(data);
    let session_id = r
        .string()
        .ok_or("neither a login nor an SSHSIG signature")?;
    if session_id.is_empty() || session_id.len() > MAX_SESSION_ID {
        return Err("neither a login nor an SSHSIG signature");
    }
    if r.byte() != Some(USERAUTH_REQUEST) {
        return Err("neither a login nor an SSHSIG signature");
    }
    let user = r
        .string()
        .and_then(|u| std::str::from_utf8(u).ok())
        .filter(|u| !u.is_empty() && u.len() <= 256 && !u.chars().any(char::is_control))
        .ok_or("a login with a malformed user name")?;
    if r.string() != Some(b"ssh-connection") {
        return Err("a login for another service");
    }
    let hostbound = match r.string() {
        Some(b"publickey") => false,
        Some(b"publickey-hostbound-v00@openssh.com") => true,
        _ => return Err("a login for another service"),
    };
    if r.byte() != Some(1) {
        return Err("not a signed request");
    }
    let algorithm = r
        .string()
        .and_then(|a| std::str::from_utf8(a).ok())
        .ok_or("bad algorithm")?;
    if r.string() != Some(key) {
        return Err("a login that names another key");
    }
    let host_key = if hostbound {
        Some(
            r.string()
                .filter(|k| key_type(k).is_some())
                .ok_or("a host-bound login without a host key")?
                .to_vec(),
        )
    } else {
        None
    };
    if !r.0.is_empty() {
        return Err("trailing data");
    }
    if !allowed.contains(&algorithm) {
        return Err("an algorithm not allowed for this key");
    }
    let wanted = match algorithm {
        "rsa-sha2-256" | "rsa-sha2-256-cert-v01@openssh.com" => SSH_AGENT_RSA_SHA2_256,
        "rsa-sha2-512" | "rsa-sha2-512-cert-v01@openssh.com" => SSH_AGENT_RSA_SHA2_512,
        _ => 0,
    };
    if rsa && flags != wanted {
        return Err("flags that do not match the algorithm");
    }
    Ok(Forwarded::Login {
        user: user.to_owned(),
        algorithm: algorithm.to_owned(),
        host_key,
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

    fn sshsig(namespace: &[u8], hash: &[u8], len: usize) -> Vec<u8> {
        let mut d = SSHSIG.to_vec();
        string(&mut d, namespace);
        string(&mut d, b"");
        string(&mut d, hash);
        string(&mut d, &vec![1; len]);
        d
    }

    #[test]
    fn forwarded_logins_and_sshsig() {
        let ed = key("ssh-ed25519");
        let rsa = key("ssh-rsa");
        assert_eq!(
            check_forwarded(&request("git", "ssh-ed25519", &ed), &ed, 0),
            Ok(Forwarded::Login {
                user: "git".into(),
                algorithm: "ssh-ed25519".into(),
                host_key: None,
            })
        );
        // OpenSSH 8.9+: the method names the server's host key after ours.
        let host = key("ssh-ed25519");
        let mut bound = request("git", "ssh-ed25519", &ed);
        let at = bound.len() - ed.len() - 4 - "ssh-ed25519".len() - 4 - 1;
        let mut d = bound[..at - "publickey".len() - 4].to_vec();
        string(&mut d, b"publickey-hostbound-v00@openssh.com");
        d.extend_from_slice(&bound[at..]);
        string(&mut d, &host);
        bound = d;
        assert_eq!(
            check_forwarded(&bound, &ed, 0),
            Ok(Forwarded::Login {
                user: "git".into(),
                algorithm: "ssh-ed25519".into(),
                host_key: Some(host),
            })
        );
        assert!(check_forwarded(&bound[..bound.len() - 1], &ed, 0).is_err());
        assert_eq!(
            check_forwarded(
                &request("x", "rsa-sha2-256", &rsa),
                &rsa,
                SSH_AGENT_RSA_SHA2_256
            )
            .map(|_| ()),
            Ok(())
        );
        assert_eq!(
            check_forwarded(&sshsig(b"git", b"sha512", 64), &ed, 0),
            Ok(Forwarded::SshSig {
                namespace: "git".into()
            })
        );
    }

    #[test]
    fn forwarded_refusals() {
        let ed = key("ssh-ed25519");
        let rsa = key("ssh-rsa");
        // SHA-1, or flags that do not match the algorithm.
        assert!(check_forwarded(&request("x", "rsa-sha2-512", &rsa), &rsa, 0).is_err());
        assert!(
            check_forwarded(
                &request("x", "rsa-sha2-512", &rsa),
                &rsa,
                SSH_AGENT_RSA_SHA2_256
            )
            .is_err()
        );
        // Both SHA-2 flags at once, for a login or an SSHSIG blob: an
        // agent may read that as neither, which is SHA-1.
        let both = SSH_AGENT_RSA_SHA2_256 | SSH_AGENT_RSA_SHA2_512;
        assert!(check_forwarded(&request("x", "rsa-sha2-512", &rsa), &rsa, both).is_err());
        assert!(check_forwarded(&sshsig(b"git", b"sha512", 64), &rsa, both).is_err());
        assert!(
            check_forwarded(&sshsig(b"git", b"sha512", 64), &rsa, SSH_AGENT_RSA_SHA2_512).is_ok()
        );
        // Another key named inside, an algorithm the key cannot have.
        assert!(
            check_forwarded(&request("x", "ssh-ed25519", &key("ssh-ed25519x")), &ed, 0).is_err()
        );
        assert!(check_forwarded(&request("x", "rsa-sha2-512", &ed), &ed, 0).is_err());
        // Plain data, empty or odd SSHSIG blobs, keys FeroSSH refuses.
        assert!(check_forwarded(b"arbitrary document to sign", &ed, 0).is_err());
        assert!(check_forwarded(&sshsig(b"", b"sha512", 64), &ed, 0).is_err());
        assert!(check_forwarded(&sshsig(b"git", b"md5", 16), &ed, 0).is_err());
        assert!(check_forwarded(&sshsig(b"g\nit", b"sha256", 32), &ed, 0).is_err());
        assert!(check_forwarded(&sshsig(b"git", b"sha256", 31), &ed, 0).is_err());
        let dss = key("ssh-dss");
        assert!(check_forwarded(&request("x", "ssh-dss", &dss), &dss, 0).is_err());
        let mut trailing = request("x", "ssh-ed25519", &ed);
        trailing.push(0);
        assert!(check_forwarded(&trailing, &ed, 0).is_err());
    }
}
