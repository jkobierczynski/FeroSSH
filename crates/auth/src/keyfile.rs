//! OpenSSH private key files (`~/.ssh/id_ed25519` and friends).
//!
//! The file is read in the UI process; its public half is offered to the
//! server, and the private half is decrypted only when the server accepts
//! the key and a signature is needed. Only Ed25519 and ECDSA P-256 keys are
//! used from files: RSA keys must go through the agent or Pageant, so no
//! RSA private-key operation ever runs in FeroSSH (see the `rsa` advisory
//! exception in `deny.toml`), and FIDO2 key handles need the token
//! (milestone 6).

use std::fmt;
use std::path::{Path, PathBuf};

use russh::keys::signature::Signer;
use russh::keys::ssh_encoding::Encode;
use russh::keys::{Algorithm, EcdsaCurve, PrivateKey};
use zeroize::Zeroizing;

/// Why a key file cannot be used, or a signature failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFileError {
    /// The file is missing or unreadable.
    Unreadable(String),
    /// A PuTTY key: import arrives in milestone 4; Pageant can load it now.
    PuttyFormat,
    /// Not an OpenSSH private key.
    Malformed,
    /// A key type FeroSSH does not use from a file.
    Unsupported(String),
    /// The passphrase was wrong.
    WrongPassphrase,
    /// The key needs a passphrase and none was given.
    NeedsPassphrase,
    /// Signing failed.
    SignFailed,
}

impl fmt::Display for KeyFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(e) => write!(f, "cannot read it ({e})"),
            Self::PuttyFormat => f.write_str(
                "it is a PuTTY key; load it into Pageant for now (import arrives in milestone 4)",
            ),
            Self::Malformed => f.write_str("it is not an OpenSSH private key"),
            Self::Unsupported(kind) => write!(f, "{kind} keys are only used through the agent"),
            Self::WrongPassphrase => f.write_str("wrong passphrase"),
            Self::NeedsPassphrase => f.write_str("it needs a passphrase"),
            Self::SignFailed => f.write_str("signing failed"),
        }
    }
}

/// A private key file whose public half is known.
pub struct KeyFile {
    pub path: PathBuf,
    /// The public key in SSH wire format.
    pub public: Vec<u8>,
    pub comment: String,
    key: PrivateKey,
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
        other => Err(KeyFileError::Unsupported(other.as_str().to_owned())),
    }
}

impl KeyFile {
    /// Parses an OpenSSH private key; the private half stays encrypted.
    pub fn parse(path: &Path, text: &str) -> Result<Self, KeyFileError> {
        if text.trim_start().starts_with("PuTTY-User-Key-File") {
            return Err(KeyFileError::PuttyFormat);
        }
        let key = PrivateKey::from_openssh(text).map_err(|_| KeyFileError::Malformed)?;
        usable(&key.algorithm())?;
        let public = key
            .public_key()
            .to_bytes()
            .map_err(|_| KeyFileError::Malformed)?;
        Ok(Self {
            path: path.to_owned(),
            public,
            comment: key.comment().as_str_lossy().to_owned(),
            key,
        })
    }

    pub fn load(path: &Path) -> Result<Self, KeyFileError> {
        let text = Zeroizing::new(
            std::fs::read_to_string(path).map_err(|e| KeyFileError::Unreadable(e.to_string()))?,
        );
        Self::parse(path, &text)
    }

    pub fn is_encrypted(&self) -> bool {
        self.key.is_encrypted()
    }

    /// Signs `data` and returns the SSH signature blob. The decrypted key
    /// lives only for this call.
    pub fn sign(&self, passphrase: Option<&str>, data: &[u8]) -> Result<Vec<u8>, KeyFileError> {
        let decrypted;
        let key = if self.key.is_encrypted() {
            let pass = passphrase.ok_or(KeyFileError::NeedsPassphrase)?;
            decrypted = self
                .key
                .decrypt(pass)
                .map_err(|_| KeyFileError::WrongPassphrase)?;
            &decrypted
        } else {
            &self.key
        };
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
            Some(KeyFileError::PuttyFormat)
        );
        assert_eq!(
            KeyFile::parse(Path::new("k"), "hello").err(),
            Some(KeyFileError::Malformed)
        );
    }
}
