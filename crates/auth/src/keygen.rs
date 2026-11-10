//! New OpenSSH key files, as `ssh-keygen -t ed25519` makes them: the
//! private key in OpenSSH's format, encrypted with the passphrase
//! (bcrypt-pbkdf and AES-256-CTR) unless it is empty, and the public
//! key line for `authorized_keys`.

use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::ssh_key::{LineEnding, PrivateKey, PublicKey};
use zeroize::Zeroizing;

/// A key pair as its two files' text.
pub struct KeyPair {
    /// The private key file, OpenSSH's PEM-like format.
    pub private: Zeroizing<String>,
    /// The public key line, `ssh-ed25519 AAAA... comment`, with a newline.
    pub public: String,
}

/// A new Ed25519 key with `comment`; an empty `passphrase` leaves the
/// private key unencrypted, as ssh-keygen does.
pub fn ed25519(comment: &str, passphrase: &str) -> Result<KeyPair, String> {
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut seed[..]).map_err(|e| e.to_string())?;
    let mut key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    key.set_comment(clean_comment(comment));
    // Before encrypting: an encrypted key keeps its comment inside.
    let public = key.public_key().to_openssh().map_err(|e| e.to_string())?;
    let key = if passphrase.is_empty() {
        key
    } else {
        key.encrypt(&mut getrandom::SysRng, passphrase)
            .map_err(|e| e.to_string())?
    };
    let private = key.to_openssh(LineEnding::LF).map_err(|e| e.to_string())?;
    Ok(KeyPair {
        private,
        public: format!("{public}\n"),
    })
}

impl std::fmt::Debug for KeyPair {
    /// The public half only: the private key never goes to a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyPair")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

/// A comment fit for one line of `authorized_keys`: control characters
/// go, and so do spaces at the ends.
pub fn clean_comment(comment: &str) -> String {
    comment
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The public key line in `text` (a `.pub` file), checked and written
/// out again, so nothing but a key and its comment can come of it.
pub fn public_line(text: &str) -> Result<String, String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or("the file holds no public key")?;
    let mut key = PublicKey::from_openssh(line).map_err(|_| "it is not an OpenSSH public key")?;
    let comment = clean_comment(key.comment().as_str_lossy());
    key.set_comment(comment);
    key.to_openssh()
        .map_err(|e| e.to_string())
        .map(|l| l.trim_end().to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn makes_an_unencrypted_key_that_loads() {
        let pair = ed25519(" me@pc\n", "").unwrap();
        assert!(
            pair.private
                .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----")
        );
        assert!(pair.public.starts_with("ssh-ed25519 AAAA"));
        assert!(pair.public.ends_with(" me@pc\n"));
        let key = PrivateKey::from_openssh(pair.private.as_bytes()).unwrap();
        assert!(!key.is_encrypted());
        assert_eq!(
            key.public_key().to_openssh().unwrap(),
            pair.public.trim_end()
        );
    }

    #[test]
    fn encrypts_with_the_passphrase() {
        let pair = ed25519("me@pc", "correct horse").unwrap();
        let key = PrivateKey::from_openssh(pair.private.as_bytes()).unwrap();
        assert!(key.is_encrypted());
        assert!(key.decrypt("wrong").is_err());
        let plain = key.decrypt("correct horse").unwrap();
        assert_eq!(
            plain.public_key().to_openssh().unwrap(),
            pair.public.trim_end()
        );
        // Two keys are never the same.
        assert_ne!(ed25519("me@pc", "").unwrap().public, pair.public);
    }

    #[test]
    fn reads_a_public_key_line_and_nothing_else() {
        let pair = ed25519("me@pc", "").unwrap();
        let text = format!("# mine\n\n{}", pair.public);
        assert_eq!(public_line(&text).unwrap(), pair.public.trim_end());
        assert!(public_line("").is_err());
        assert!(public_line("hello world").is_err());
        assert!(public_line("-----BEGIN OPENSSH PRIVATE KEY-----").is_err());
    }

    /// OpenSSH's own ssh-keygen reads the files, with the passphrase.
    #[test]
    #[cfg(unix)]
    fn ssh_keygen_reads_the_files() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        if Command::new("ssh-keygen").arg("-?").output().is_err() {
            assert!(
                std::env::var_os("FEROSSH_REQUIRE_SSHD").is_none(),
                "FEROSSH_REQUIRE_SSHD is set but ssh-keygen is missing"
            );
            eprintln!("ssh-keygen not found; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ferossh-keygen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for pass in ["", "two words"] {
            let pair = ed25519("me@pc", pass).unwrap();
            let file = dir.join("k");
            std::fs::write(&file, pair.private.as_bytes()).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            let out = Command::new("ssh-keygen")
                .args(["-y", "-P", pass, "-f"])
                .arg(&file)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            let line = String::from_utf8(out.stdout).unwrap();
            // ssh-keygen -y prints the comment only when it can read it.
            assert!(
                pair.public.starts_with(
                    line.split(' ')
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" ")
                        .as_str()
                )
            );
            if !pass.is_empty() {
                let wrong = Command::new("ssh-keygen")
                    .args(["-y", "-P", "wrong", "-f"])
                    .arg(&file)
                    .output()
                    .unwrap();
                assert!(!wrong.status.success());
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
