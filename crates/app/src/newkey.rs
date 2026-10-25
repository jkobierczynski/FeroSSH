//! `ferossh --new-security-key [--hello] [--ed25519] FILE`: makes an
//! OpenSSH security key (`id_ecdsa_sk`-style) with Windows' WebAuthn, as
//! `ssh-keygen -t ecdsa-sk` does, so a FIDO2 key or Windows Hello can log
//! in without OpenSSH installed. `--hello` asks for Windows Hello only.
//!
//! The file holds no secret, only the key handle; the private key stays
//! in the authenticator.

use std::path::{Path, PathBuf};

use ferossh_auth::sk::{self, Authenticator, DEFAULT_APPLICATION, EnrollRequest, SkAlgorithm};

/// What to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewKey {
    pub path: PathBuf,
    pub algorithm: SkAlgorithm,
    /// Windows Hello only.
    pub hello: bool,
}

/// The rest of the command line after `--new-security-key`.
pub fn parse(args: &[String]) -> Result<NewKey, String> {
    let mut path = None;
    let mut algorithm = SkAlgorithm::EcdsaP256;
    let mut hello = false;
    for arg in args {
        match arg.as_str() {
            "--hello" => hello = true,
            "--ed25519" => algorithm = SkAlgorithm::Ed25519,
            a if a.starts_with('-') => return Err(format!("unknown option {a}")),
            a if path.is_none() => path = Some(PathBuf::from(a)),
            a => return Err(format!("unexpected argument {a}")),
        }
    }
    let path = path.ok_or("--new-security-key needs a file name, such as id_ecdsa_sk")?;
    if hello && algorithm == SkAlgorithm::Ed25519 {
        // Windows Hello makes ES256 (P-256) keys only.
        return Err("Windows Hello makes ECDSA keys only; leave out --ed25519".into());
    }
    Ok(NewKey {
        path,
        algorithm,
        hello,
    })
}

fn pub_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".pub");
    PathBuf::from(p)
}

/// Makes the key with `authenticator` and writes `path` and `path.pub`;
/// returns the public key line for `authorized_keys`.
pub fn make(
    authenticator: &dyn Authenticator,
    key: &NewKey,
    comment: &str,
) -> Result<String, String> {
    let public_path = pub_path(&key.path);
    for p in [&key.path, &public_path] {
        if p.exists() {
            return Err(format!("{} already exists", p.display()));
        }
    }
    let mut challenge = [0u8; 32];
    getrandom::fill(&mut challenge).map_err(|e| e.to_string())?;
    let enrolled = sk::enroll(
        authenticator,
        &EnrollRequest {
            algorithm: key.algorithm,
            application: DEFAULT_APPLICATION,
            challenge: &challenge,
            platform: key.hello,
            user_verification: false,
        },
        comment,
    )?;
    let write = |p: &Path, text: &str| {
        std::fs::write(p, text).map_err(|e| format!("cannot write {}: {e}", p.display()))
    };
    write(&key.path, &enrolled.private)?;
    write(&public_path, &enrolled.public)?;
    Ok(enrolled.public.trim_end().to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ferossh_auth::keyfile::KeyFile;
    use ferossh_auth::sk::soft::SoftAuthenticator;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn command_line() {
        assert_eq!(
            parse(&args(&["--hello", "k"])),
            Ok(NewKey {
                path: "k".into(),
                algorithm: SkAlgorithm::EcdsaP256,
                hello: true
            })
        );
        assert_eq!(
            parse(&args(&["--ed25519", "k"])).map(|k| k.algorithm),
            Ok(SkAlgorithm::Ed25519)
        );
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["--hello", "--ed25519", "k"])).is_err());
        assert!(parse(&args(&["a", "b"])).is_err());
    }

    #[test]
    fn writes_a_key_that_signs() {
        let dir = std::env::temp_dir().join(format!("ferossh-newkey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let soft = SoftAuthenticator::new();
        let key = NewKey {
            path: dir.join("id_ecdsa_sk"),
            algorithm: SkAlgorithm::EcdsaP256,
            hello: false,
        };
        let line = make(&soft, &key, "me@pc").unwrap();
        assert!(
            line.starts_with("sk-ecdsa-sha2-nistp256@openssh.com "),
            "{line}"
        );
        assert!(line.ends_with(" me@pc"));
        assert_eq!(
            std::fs::read_to_string(dir.join("id_ecdsa_sk.pub")).unwrap(),
            format!("{line}\n")
        );
        let kf = KeyFile::load(&key.path).unwrap();
        assert!(kf.is_security_key());
        let sig = kf.sign_with(None, b"data", &soft).unwrap();
        sk::verify(&kf.public, b"data", &sig).unwrap();
        // Never overwrites.
        assert!(
            make(&soft, &key, "me@pc")
                .unwrap_err()
                .contains("already exists")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
