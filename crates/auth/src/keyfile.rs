//! Private key files: OpenSSH's (`~/.ssh/id_ed25519` and friends) and
//! PuTTY's `.ppk` (versions 2 and 3).
//!
//! The file is read in the UI process; its public half is offered to the
//! server, and the private half is decrypted only when the server accepts
//! the key and a signature is needed. Only Ed25519 and ECDSA P-256 keys are
//! used from files: RSA keys must go through the agent or Pageant, so no
//! RSA private-key operation ever runs in FeroSSH (see the `rsa` advisory
//! exception in `deny.toml`). FIDO2 security key files (`id_ed25519_sk`,
//! `id_ecdsa_sk`) hold only a key handle; the security key signs, through
//! an [`Authenticator`].
//!
//! A `.ppk` file keeps its public key in the clear, so FeroSSH reads that
//! part itself; the private part, its MAC and its encryption are checked
//! and decrypted by `ssh-key` only when signing.

use std::fmt;
use std::path::{Path, PathBuf};

use russh::keys::signature::Signer;
use russh::keys::ssh_encoding::Encode;
use russh::keys::{Algorithm, EcdsaCurve, PrivateKey};
use zeroize::Zeroizing;

use crate::sk::{Authenticator, SkKey, Unavailable};

mod short_scalar;

/// Why a key file cannot be used, or a signature failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFileError {
    /// The file is missing or unreadable.
    Unreadable(String),
    /// Not an OpenSSH or PuTTY private key.
    Malformed,
    /// A key type FeroSSH does not use from a file.
    Unsupported(String),
    /// The passphrase was wrong.
    WrongPassphrase,
    /// The key needs a passphrase and none was given.
    NeedsPassphrase,
    /// Signing failed.
    SignFailed,
    /// The security key did not sign.
    SecurityKey(String),
}

impl fmt::Display for KeyFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(e) => write!(f, "cannot read it ({e})"),
            Self::Malformed => f.write_str("it is not an OpenSSH or PuTTY private key"),
            Self::Unsupported(kind) => {
                write!(f, "{kind} keys are only used through the agent or Pageant")
            }
            Self::WrongPassphrase => f.write_str("wrong passphrase"),
            Self::NeedsPassphrase => f.write_str("it needs a passphrase"),
            Self::SignFailed => f.write_str("signing failed"),
            Self::SecurityKey(e) => write!(f, "security key: {e}"),
        }
    }
}

/// The private half, as stored.
enum Secret {
    OpenSsh(Box<PrivateKey>),
    /// The whole `.ppk` text; `encrypted` from its `Encryption` header.
    Ppk {
        text: Zeroizing<String>,
        encrypted: bool,
    },
}

/// A private key file whose public half is known.
pub struct KeyFile {
    pub path: PathBuf,
    /// The public key in SSH wire format.
    pub public: Vec<u8>,
    pub comment: String,
    key: Secret,
}

/// The clear-text header of a `.ppk` file: algorithm, encryption, comment
/// and the public key blob.
fn ppk_public(text: &str) -> Result<(Algorithm, bool, String, Vec<u8>), KeyFileError> {
    use russh::keys::ssh_encoding::Base64Reader;
    use russh::keys::ssh_encoding::Reader as _;
    let mut lines = text.lines();
    let mut header = |name: &str| -> Result<String, KeyFileError> {
        let line = lines.next().ok_or(KeyFileError::Malformed)?;
        let (key, value) = line.split_once(": ").ok_or(KeyFileError::Malformed)?;
        if !(key == name || (name == "PuTTY" && key.starts_with("PuTTY-User-Key-File-"))) {
            return Err(KeyFileError::Malformed);
        }
        Ok(value.trim().to_owned())
    };
    let alg = header("PuTTY")?;
    let encryption = header("Encryption")?;
    let comment = header("Comment")?;
    let count: usize = header("Public-Lines")?
        .parse()
        .map_err(|_| KeyFileError::Malformed)?;
    if count > 64 {
        return Err(KeyFileError::Malformed);
    }
    let b64: String = lines.take(count).map(str::trim).collect();
    let mut reader = Base64Reader::new(b64.as_bytes()).map_err(|_| KeyFileError::Malformed)?;
    let mut blob = vec![0u8; reader.remaining_len()];
    reader
        .read(&mut blob)
        .map_err(|_| KeyFileError::Malformed)?;
    let public = russh::keys::PublicKey::from_bytes(&blob).map_err(|_| KeyFileError::Malformed)?;
    if public.algorithm().as_str() != alg {
        return Err(KeyFileError::Malformed);
    }
    let encrypted = match encryption.as_str() {
        "none" => false,
        "aes256-cbc" => true,
        _ => return Err(KeyFileError::Malformed),
    };
    Ok((public.algorithm(), encrypted, comment, blob))
}

impl fmt::Debug for KeyFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyFile")
            .field("path", &self.path)
            .field("encrypted", &self.is_encrypted())
            .finish_non_exhaustive()
    }
}

fn usable(alg: &Algorithm) -> Result<(), KeyFileError> {
    match alg {
        Algorithm::Ed25519 => Ok(()),
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        } => Ok(()),
        Algorithm::SkEd25519 | Algorithm::SkEcdsaSha2NistP256 => Ok(()),
        other => Err(KeyFileError::Unsupported(other.as_str().to_owned())),
    }
}

impl KeyFile {
    /// Parses an OpenSSH private key; the private half stays encrypted.
    pub fn parse(path: &Path, text: &str) -> Result<Self, KeyFileError> {
        if text.trim_start().starts_with("PuTTY-User-Key-File-") {
            let (alg, encrypted, comment, public) = ppk_public(text.trim_start())?;
            usable(&alg)?;
            return Ok(Self {
                path: path.to_owned(),
                public,
                comment,
                key: Secret::Ppk {
                    text: Zeroizing::new(text.trim_start().to_owned()),
                    encrypted,
                },
            });
        }
        let key = match PrivateKey::from_openssh(text) {
            Ok(key) => key,
            Err(_) => short_scalar::parse(text)?.ok_or(KeyFileError::Malformed)?,
        };
        usable(&key.algorithm())?;
        let public = key
            .public_key()
            .to_bytes()
            .map_err(|_| KeyFileError::Malformed)?;
        Ok(Self {
            path: path.to_owned(),
            public,
            comment: key.comment().as_str_lossy().to_owned(),
            key: Secret::OpenSsh(Box::new(key)),
        })
    }

    pub fn load(path: &Path) -> Result<Self, KeyFileError> {
        let text = Zeroizing::new(
            std::fs::read_to_string(path).map_err(|e| KeyFileError::Unreadable(e.to_string()))?,
        );
        Self::parse(path, &text)
    }

    pub fn is_encrypted(&self) -> bool {
        match &self.key {
            Secret::OpenSsh(k) => k.is_encrypted(),
            Secret::Ppk { encrypted, .. } => *encrypted,
        }
    }

    /// Whether the private half lives on a FIDO2 security key.
    pub fn is_security_key(&self) -> bool {
        crate::userauth::key_type(&self.public).is_some_and(|t| t.starts_with("sk-"))
    }

    /// Signs `data` and returns the SSH signature blob. The decrypted key
    /// lives only for this call. Security keys cannot sign this way; see
    /// [`KeyFile::sign_with`].
    pub fn sign(&self, passphrase: Option<&str>, data: &[u8]) -> Result<Vec<u8>, KeyFileError> {
        self.sign_with(passphrase, data, &Unavailable)
    }

    /// [`KeyFile::sign`], with `authenticator` for security keys.
    pub fn sign_with(
        &self,
        passphrase: Option<&str>,
        data: &[u8],
        authenticator: &dyn Authenticator,
    ) -> Result<Vec<u8>, KeyFileError> {
        let decrypted;
        let key = match &self.key {
            Secret::OpenSsh(key) if key.is_encrypted() => {
                let pass = passphrase.ok_or(KeyFileError::NeedsPassphrase)?;
                decrypted = match key.decrypt(pass) {
                    Ok(key) => key,
                    Err(_) => {
                        short_scalar::decrypt(key, pass)?.ok_or(KeyFileError::WrongPassphrase)?
                    }
                };
                &decrypted
            }
            Secret::OpenSsh(key) => key,
            Secret::Ppk { text, encrypted } => {
                if *encrypted && passphrase.is_none() {
                    return Err(KeyFileError::NeedsPassphrase);
                }
                // The MAC covers the whole file: a wrong passphrase and a
                // tampered file look the same.
                decrypted = PrivateKey::from_ppk(text.as_str(), passphrase.map(str::to_owned))
                    .map_err(|_| {
                        if *encrypted {
                            KeyFileError::WrongPassphrase
                        } else {
                            KeyFileError::Malformed
                        }
                    })?;
                // The key that signs must be the one offered.
                if decrypted.public_key().to_bytes().ok().as_deref() != Some(&self.public[..]) {
                    return Err(KeyFileError::Malformed);
                }
                &decrypted
            }
        };
        if let Some(sk) = SkKey::from_private(key) {
            return sk
                .sign(authenticator, data)
                .map_err(KeyFileError::SecurityKey);
        }
        let sig = key.try_sign(data).map_err(|_| KeyFileError::SignFailed)?;
        let mut blob = Vec::new();
        sig.encode(&mut blob)
            .map_err(|_| KeyFileError::SignFailed)?;
        Ok(blob)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use getrandom::SysRng;
    use getrandom::rand_core::UnwrapErr;
    use russh::keys::signature::Verifier;
    use russh::keys::ssh_encoding::Decode;
    use russh::keys::ssh_key::{LineEnding, Signature};
    use russh::keys::{HashAlg, PublicKey};

    /// A fresh key file text, so no private key lives in the repository.
    fn key_file(alg: Algorithm, passphrase: Option<&str>) -> String {
        let mut rng = UnwrapErr(SysRng);
        let mut key = PrivateKey::random(&mut rng, alg).unwrap();
        key.set_comment("test");
        if let Some(p) = passphrase {
            key = key.encrypt(&mut rng, p).unwrap();
        }
        key.to_openssh(LineEnding::LF).unwrap().to_string()
    }

    fn verify(public: &[u8], data: &[u8], blob: &[u8]) {
        let key = PublicKey::from_bytes(public).unwrap();
        let sig = Signature::decode(&mut &blob[..]).unwrap();
        Verifier::verify(&key, data, &sig).unwrap();
    }

    #[test]
    fn encrypted_key_needs_the_right_passphrase() {
        let text = key_file(Algorithm::Ed25519, Some("secret"));
        let kf = KeyFile::parse(Path::new("k"), &text).unwrap();
        assert!(kf.is_encrypted());
        // The comment is inside the encrypted part.
        assert_eq!(kf.comment, "");
        assert_eq!(kf.sign(None, b"x"), Err(KeyFileError::NeedsPassphrase));
        assert_eq!(
            kf.sign(Some("nope"), b"x"),
            Err(KeyFileError::WrongPassphrase)
        );
        let blob = kf.sign(Some("secret"), b"data").unwrap();
        verify(&kf.public, b"data", &blob);
    }

    #[test]
    fn plain_ecdsa_signs() {
        let alg = Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        };
        let kf = KeyFile::parse(Path::new("k"), &key_file(alg, None)).unwrap();
        assert_eq!(kf.comment, "test");
        let blob = kf.sign(None, b"data").unwrap();
        verify(&kf.public, b"data", &blob);
    }

    #[test]
    fn rsa_other_curves_and_putty_are_refused() {
        let rsa = Algorithm::Rsa {
            hash: Some(HashAlg::Sha512),
        };
        assert!(matches!(usable(&rsa), Err(KeyFileError::Unsupported(_))));
        let p384 = Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP384,
        };
        assert!(KeyFile::parse(Path::new("k"), &key_file(p384, None)).is_err());
        assert_eq!(
            KeyFile::parse(Path::new("k"), "PuTTY-User-Key-File-3: ssh-ed25519\n").err(),
            Some(KeyFileError::Malformed)
        );
        assert_eq!(
            KeyFile::parse(Path::new("k"), "hello").err(),
            Some(KeyFileError::Malformed)
        );
    }

    /// Makes a `.ppk` with puttygen, if installed (the Linux CI job has it).
    fn puttygen(kind: &str, passphrase: Option<&str>) -> Option<String> {
        use std::process::{Command, Stdio};
        let dir = std::env::temp_dir().join(format!(
            "ferossh-ppk-{kind}-{}-{}",
            passphrase.is_some(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        let key = dir.join("k.ppk");
        let mut cmd = Command::new("puttygen");
        cmd.args(["-t", kind, "-q", "-C", "putty test", "-o"])
            .arg(&key);
        if kind == "ecdsa" {
            cmd.args(["-b", "256"]);
        }
        // An empty passphrase file makes an unencrypted key.
        let pass = dir.join("pass");
        std::fs::write(&pass, passphrase.unwrap_or_default()).ok()?;
        cmd.arg("--new-passphrase").arg(pass);
        let ok = cmd
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success();
        let text = std::fs::read_to_string(&key).ok();
        let _ = std::fs::remove_dir_all(&dir);
        if !ok {
            return None;
        }
        text
    }

    fn have_puttygen() -> bool {
        let found = std::process::Command::new("puttygen")
            .arg("--version")
            .output()
            .is_ok();
        if !found {
            assert!(
                std::env::var_os("FEROSSH_REQUIRE_PUTTYGEN").is_none(),
                "FEROSSH_REQUIRE_PUTTYGEN is set but puttygen is missing"
            );
            eprintln!("puttygen not found; skipping");
        }
        found
    }

    #[test]
    fn ppk_keys_sign() {
        if !have_puttygen() {
            return;
        }
        let plain = puttygen("ed25519", None).unwrap();
        let kf = KeyFile::parse(Path::new("k.ppk"), &plain).unwrap();
        assert!(!kf.is_encrypted());
        assert_eq!(kf.comment, "putty test");
        let blob = kf.sign(None, b"data").unwrap();
        verify(&kf.public, b"data", &blob);

        let locked = puttygen("ecdsa", Some("pw")).unwrap();
        let kf = KeyFile::parse(Path::new("k.ppk"), &locked).unwrap();
        assert!(kf.is_encrypted());
        assert_eq!(kf.sign(None, b"x"), Err(KeyFileError::NeedsPassphrase));
        assert_eq!(
            kf.sign(Some("no"), b"x"),
            Err(KeyFileError::WrongPassphrase)
        );
        let blob = kf.sign(Some("pw"), b"data").unwrap();
        verify(&kf.public, b"data", &blob);
    }

    #[test]
    fn ppk_rsa_and_tampering_are_refused() {
        if !have_puttygen() {
            return;
        }
        let rsa = puttygen("rsa", None).unwrap();
        assert!(matches!(
            KeyFile::parse(Path::new("k.ppk"), &rsa),
            Err(KeyFileError::Unsupported(_))
        ));
        // Swap in another key's public lines: the MAC check catches it.
        let a = puttygen("ed25519", None).unwrap();
        let b = puttygen("ed25519", None).unwrap();
        let public = |t: &str| t.lines().nth(4).unwrap_or_default().to_owned();
        let forged = a.replace(&public(&a), &public(&b));
        let kf = KeyFile::parse(Path::new("k.ppk"), &forged).unwrap();
        assert!(kf.sign(None, b"x").is_err());
    }
}
