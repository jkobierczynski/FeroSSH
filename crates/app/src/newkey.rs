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

/// Adds what to do when Windows Hello cannot make the key: on many PCs,
/// often ones without a TPM, it makes only RSA keys, which OpenSSH's
/// security key types do not include.
fn explain(key: &NewKey, error: String) -> String {
    if key.hello && error.contains("NotSupportedError") {
        format!(
            "{error}\n\nWindows Hello on this PC cannot make this kind of key \
             (it often makes only RSA keys, and OpenSSH security keys are ECDSA \
             or Ed25519). Leave out --hello to use a security key or a phone instead."
        )
    } else {
        error
    }
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
    )
    .map_err(|e| explain(key, e))?;
    make_dir(&key.path)?;
    write_new(&key.path, enrolled.private.as_bytes())?;
    if let Err(e) = write_new(&public_path, enrolled.public.as_bytes()) {
        let _ = std::fs::remove_file(&key.path);
        return Err(e);
    }
    Ok(enrolled.public.trim_end().to_owned())
}

/// Makes the folder `path` goes in when it is missing.
fn make_dir(path: &Path) -> Result<(), String> {
    match path.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))
        }
        None => Ok(()),
    }
}

/// Writes a new file, never over an old one.
fn write_new(p: &Path, text: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    f.write_all(text)
        .map_err(|e| format!("cannot write {}: {e}", p.display()))
}

/// Makes an Ed25519 key, as `ssh-keygen -t ed25519` does, at `path`
/// and `path.pub`, encrypted with `passphrase` unless it is empty; makes
/// the folder when missing. Returns the public key line.
pub fn make_ed25519(path: &Path, comment: &str, passphrase: &str) -> Result<String, String> {
    let public_path = pub_path(path);
    for p in [path, &public_path] {
        if p.exists() {
            return Err(format!("{} already exists", p.display()));
        }
    }
    make_dir(path)?;
    let pair = ferossh_auth::keygen::ed25519(comment, passphrase)?;
    write_new(path, pair.private.as_bytes())?;
    if let Err(e) = write_new(&public_path, pair.public.as_bytes()) {
        // Half a key pair is no use: take the private half away again.
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(pair.public.trim_end().to_owned())
}

/// The kinds of key Tools > New SSH key makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An Ed25519 key file, optionally with a passphrase.
    File,
    /// A FIDO2 security key or phone, ECDSA (`id_ecdsa_sk`).
    SecurityKey,
    /// A FIDO2 security key, Ed25519 (`id_ed25519_sk`).
    SecurityKeyEd25519,
    /// Windows Hello on this PC (ECDSA).
    Hello,
}

impl Kind {
    pub const ALL: [Kind; 4] = [
        Kind::File,
        Kind::SecurityKey,
        Kind::SecurityKeyEd25519,
        Kind::Hello,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::File => "Ed25519 key file (with or without a passphrase)",
            Kind::SecurityKey => "Security key or phone, ECDSA (ecdsa-sk)",
            Kind::SecurityKeyEd25519 => "Security key, Ed25519 (ed25519-sk)",
            Kind::Hello => "Windows Hello on this PC (ecdsa-sk)",
        }
    }

    /// The file name OpenSSH and FeroSSH try without an IdentityFile.
    pub fn file_name(self) -> &'static str {
        match self {
            Kind::File => "id_ed25519",
            Kind::SecurityKey | Kind::Hello => "id_ecdsa_sk",
            Kind::SecurityKeyEd25519 => "id_ed25519_sk",
        }
    }

    /// Only a key file is encrypted; a security key's file holds no
    /// secret.
    pub fn has_passphrase(self) -> bool {
        self == Kind::File
    }
}

/// What the New SSH key window asks for.
#[derive(Debug)]
pub struct Request {
    pub kind: Kind,
    pub path: PathBuf,
    pub comment: String,
    pub passphrase: zeroize::Zeroizing<String>,
}

/// Whether the window's answers can make a key: a file name that is not
/// taken, and the passphrase typed the same twice.
pub fn check(kind: Kind, path: &Path, passphrase: &str, repeat: &str) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("Choose a file for the key.".into());
    }
    for p in [path.to_path_buf(), pub_path(path)] {
        if p.exists() {
            return Err(format!(
                "{} already exists. Choose another name.",
                p.display()
            ));
        }
    }
    if kind.has_passphrase() && passphrase != repeat {
        return Err("The two passphrases differ.".into());
    }
    Ok(())
}

/// Makes the key `request` asks for, with `authenticator` for a security
/// key; returns the public key line.
pub fn make_kind(authenticator: &dyn Authenticator, request: &Request) -> Result<String, String> {
    match request.kind {
        Kind::File => make_ed25519(&request.path, &request.comment, &request.passphrase),
        kind => make(
            authenticator,
            &NewKey {
                path: request.path.clone(),
                algorithm: match kind {
                    Kind::SecurityKeyEd25519 => SkAlgorithm::Ed25519,
                    _ => SkAlgorithm::EcdsaP256,
                },
                hello: kind == Kind::Hello,
            },
            &request.comment,
        ),
    }
}

/// The first of `base`, `base_2`, `base_3`... in `dir` with neither the
/// file nor its `.pub` there.
pub fn free_name(dir: &Path, base: &str) -> PathBuf {
    (1..1000)
        .map(|n| match n {
            1 => dir.join(base),
            n => dir.join(format!("{base}_{n}")),
        })
        .find(|p| !p.exists() && !pub_path(p).exists())
        .unwrap_or_else(|| dir.join(base))
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
    fn says_what_to_do_when_hello_cannot() {
        let mut key = parse(&args(&["--hello", "k"])).unwrap();
        let e = "The parameter is incorrect. (NotSupportedError)".to_owned();
        assert!(explain(&key, e.clone()).contains("Leave out --hello"));
        key.hello = false;
        assert_eq!(explain(&key, e.clone()), e);
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

    #[test]
    fn writes_an_ed25519_key_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("ferossh-newkey-ed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ssh = dir.join(".ssh");
        let path = free_name(&ssh, "id_ed25519");
        assert_eq!(path, ssh.join("id_ed25519"));
        let line = make_ed25519(&path, "me@pc", "pw").unwrap();
        assert!(line.starts_with("ssh-ed25519 ") && line.ends_with(" me@pc"));
        let kf = KeyFile::load(&path).unwrap();
        assert!(kf.is_encrypted());
        kf.sign(Some("pw"), b"data").unwrap();
        assert!(
            make_ed25519(&path, "me@pc", "")
                .unwrap_err()
                .contains("already exists")
        );
        assert_eq!(free_name(&ssh, "id_ed25519"), ssh.join("id_ed25519_2"));
        // The window's checks.
        assert!(
            check(Kind::File, &path, "a", "a")
                .unwrap_err()
                .contains("already exists")
        );
        let other = ssh.join("other");
        assert!(
            check(Kind::File, &other, "a", "b")
                .unwrap_err()
                .contains("differ")
        );
        assert!(check(Kind::Hello, &other, "a", "b").is_ok());
        assert!(check(Kind::File, Path::new(""), "", "").is_err());
        let line = make_kind(
            &SoftAuthenticator::new(),
            &Request {
                kind: Kind::SecurityKeyEd25519,
                path: ssh.join("sub").join(Kind::SecurityKeyEd25519.file_name()),
                comment: "me@pc".into(),
                passphrase: String::new().into(),
            },
        )
        .unwrap();
        assert!(line.starts_with("sk-ssh-ed25519@openssh.com "), "{line}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
