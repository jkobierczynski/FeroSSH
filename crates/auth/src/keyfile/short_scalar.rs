//! OpenSSH writes an ECDSA private scalar as a minimal `mpint`, so about
//! one P-256 key in 256 stores it in 31 bytes (one in 65536 in 30).
//! ssh-key 0.7.0-rc.11 refuses scalars under 32 bytes, which made those
//! keys "not an OpenSSH private key". This module reads such a key itself,
//! pads the scalar to 32 bytes and hands ssh-key a re-encoded, unencrypted
//! copy, so every other check stays ssh-key's.
//!
//! Only `ecdsa-sha2-nistp256` keys with a short scalar take this path; for
//! anything else it answers `Ok(None)` and the caller keeps its own error.

use russh::keys::PrivateKey;
use russh::keys::signature::{Signer, Verifier};
use russh::keys::ssh_encoding::{Base64Reader, Decode, Encode, Reader};
use russh::keys::ssh_key::Signature;
use russh::keys::ssh_key::cipher::Tag;
use zeroize::Zeroizing;

use super::KeyFileError;

const MAGIC: &[u8] = b"openssh-key-v1\0";
const P256: &str = "ecdsa-sha2-nistp256";
const SCALAR: usize = 32;

/// The parts of an `openssh-key-v1` container this module needs.
struct Container {
    encrypted: bool,
    public: Vec<u8>,
    /// The private section, still encrypted if `encrypted`.
    private: Zeroizing<Vec<u8>>,
    tag: Option<Tag>,
}

fn malformed<E>(_: E) -> KeyFileError {
    KeyFileError::Malformed
}

/// The binary container inside `-----BEGIN OPENSSH PRIVATE KEY-----`.
fn pem_body(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut lines = text.lines().map(str::trim);
    lines.find(|l| *l == "-----BEGIN OPENSSH PRIVATE KEY-----")?;
    let mut b64 = Zeroizing::new(String::new());
    for line in lines {
        if line == "-----END OPENSSH PRIVATE KEY-----" {
            let mut reader = Base64Reader::new(b64.as_bytes()).ok()?;
            let mut body = Zeroizing::new(vec![0u8; reader.remaining_len()]);
            reader.read(&mut body).ok()?;
            return Some(body);
        }
        b64.push_str(line);
    }
    None
}

fn container(mut bytes: &[u8]) -> Result<Container, KeyFileError> {
    let r = &mut bytes;
    let mut magic = [0u8; MAGIC.len()];
    r.read(&mut magic).map_err(malformed)?;
    if magic != MAGIC {
        return Err(KeyFileError::Malformed);
    }
    let cipher = String::decode(r).map_err(malformed)?;
    let _kdf_name = String::decode(r).map_err(malformed)?;
    let _kdf_options = Vec::<u8>::decode(r).map_err(malformed)?;
    if u32::decode(r).map_err(malformed)? != 1 {
        return Err(KeyFileError::Malformed);
    }
    let public = Vec::<u8>::decode(r).map_err(malformed)?;
    let private = Zeroizing::new(Vec::<u8>::decode(r).map_err(malformed)?);
    let tag = match r.remaining_len() {
        0 => None,
        16 => {
            let mut tag = Tag::default();
            r.read(&mut tag).map_err(malformed)?;
            Some(tag)
        }
        _ => return Err(KeyFileError::Malformed),
    };
    Ok(Container {
        encrypted: cipher != "none",
        public,
        private,
        tag,
    })
}

/// Rebuilds a decrypted private section with the scalar padded, as an
/// unencrypted container, and decodes that. `Ok(None)` when the key is not
/// a P-256 key with a short scalar.
fn rebuild(
    public: &[u8],
    mut plain: &[u8],
    encrypted: bool,
) -> Result<Option<PrivateKey>, KeyFileError> {
    let r = &mut plain;
    let check1 = u32::decode(r).map_err(malformed)?;
    let check2 = u32::decode(r).map_err(malformed)?;
    if check1 != check2 {
        // The checkints are how OpenSSH spots a wrong passphrase.
        return Err(if encrypted {
            KeyFileError::WrongPassphrase
        } else {
            KeyFileError::Malformed
        });
    }
    if String::decode(r).map_err(malformed)? != P256 {
        return Ok(None);
    }
    let curve = String::decode(r).map_err(malformed)?;
    let point = Vec::<u8>::decode(r).map_err(malformed)?;
    let scalar = Zeroizing::new(Vec::<u8>::decode(r).map_err(malformed)?);
    let comment = String::decode(r).map_err(malformed)?;
    let zeros = scalar.iter().take_while(|&&b| b == 0).count();
    let digits = scalar.get(zeros..).unwrap_or_default();
    if digits.len() >= SCALAR {
        // ssh-key reads these itself: the failure was something else.
        return Ok(None);
    }
    let mut padded = Zeroizing::new([0u8; SCALAR]);
    let tail = SCALAR
        .checked_sub(digits.len())
        .and_then(|at| padded.get_mut(at..))
        .ok_or(KeyFileError::Malformed)?;
    tail.copy_from_slice(digits);

    let mut section = Zeroizing::new(Vec::new());
    let encode = |section: &mut Vec<u8>| -> Result<(), russh::keys::ssh_encoding::Error> {
        check1.encode(section)?;
        check1.encode(section)?;
        P256.encode(section)?;
        curve.encode(section)?;
        point.encode(section)?;
        // A padded scalar starts with a zero byte, so it needs no sign byte.
        padded.as_slice().encode(section)?;
        comment.encode(section)
    };
    encode(&mut section).map_err(malformed)?;
    let mut pad = 1u8;
    while section.len() % 8 != 0 {
        section.push(pad);
        pad += 1;
    }
    let mut outer = Zeroizing::new(MAGIC.to_vec());
    let encode = |outer: &mut Vec<u8>| -> Result<(), russh::keys::ssh_encoding::Error> {
        "none".encode(outer)?;
        "none".encode(outer)?;
        b"".as_slice().encode(outer)?;
        1u32.encode(outer)?;
        public.encode(outer)?;
        section.as_slice().encode(outer)
    };
    encode(&mut outer).map_err(malformed)?;
    let key = PrivateKey::from_bytes(&outer).map_err(malformed)?;

    // ssh-key copies the public point; make sure the scalar belongs to it.
    let probe = b"ferossh short scalar check";
    let sig: Signature = key.try_sign(probe).map_err(malformed)?;
    Verifier::verify(key.public_key(), probe, &sig).map_err(malformed)?;
    Ok(Some(key))
}

/// An unencrypted OpenSSH key file that ssh-key refused.
pub(super) fn parse(text: &str) -> Result<Option<PrivateKey>, KeyFileError> {
    let Some(body) = pem_body(text) else {
        return Ok(None);
    };
    let c = container(&body)?;
    if c.encrypted {
        return Ok(None);
    }
    rebuild(&c.public, &c.private, false)
}

/// An encrypted OpenSSH key that ssh-key could not decrypt.
pub(super) fn decrypt(
    key: &PrivateKey,
    passphrase: &str,
) -> Result<Option<PrivateKey>, KeyFileError> {
    let bytes = key.to_bytes().map_err(malformed)?;
    let c = container(&bytes)?;
    if !c.encrypted {
        return Ok(None);
    }
    let cipher = key.cipher();
    let (k, iv) = key
        .kdf()
        .derive_key_and_iv(cipher, passphrase)
        .map_err(|_| KeyFileError::WrongPassphrase)?;
    let mut plain = c.private;
    cipher
        .decrypt(&k, &iv, &mut plain, c.tag)
        .map_err(|_| KeyFileError::WrongPassphrase)?;
    rebuild(&c.public, &plain, true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::keyfile::KeyFile;
    use getrandom::SysRng;
    use getrandom::rand_core::UnwrapErr;
    use russh::keys::ssh_encoding::base64::{Base64, Encoding};
    use russh::keys::ssh_key::private::{EcdsaKeypair, KeypairData};
    use russh::keys::ssh_key::{Cipher, Kdf, KdfAlg, LineEnding};
    use russh::keys::{Algorithm, EcdsaCurve};
    use std::path::Path;

    /// A P-256 key whose scalar starts with a zero byte, as OpenSSH writes
    /// it: a minimal mpint of 31 bytes.
    fn short_key() -> PrivateKey {
        let mut rng = UnwrapErr(SysRng);
        let alg = Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        };
        loop {
            let mut key = PrivateKey::random(&mut rng, alg.clone()).unwrap();
            if let KeypairData::Ecdsa(EcdsaKeypair::NistP256 { private, .. }) = key.key_data()
                && private.as_ref()[0] == 0
                && private.as_ref()[1] != 0
            {
                key.set_comment("short");
                return key;
            }
        }
    }

    /// The file `ssh-keygen` would write for `key`, optionally encrypted.
    fn openssh_file(key: &PrivateKey, encrypt: Option<(Cipher, &str)>) -> String {
        let KeypairData::Ecdsa(EcdsaKeypair::NistP256 { public, private }) = key.key_data() else {
            unreachable!()
        };
        let mut section = Vec::new();
        7u32.encode(&mut section).unwrap();
        7u32.encode(&mut section).unwrap();
        P256.encode(&mut section).unwrap();
        "nistp256".encode(&mut section).unwrap();
        public.as_bytes().encode(&mut section).unwrap();
        private.as_ref()[1..].encode(&mut section).unwrap();
        "short".encode(&mut section).unwrap();
        let block = encrypt.map_or(8, |(c, _)| c.block_size());
        let mut pad = 1u8;
        while section.len() % block != 0 {
            section.push(pad);
            pad += 1;
        }
        let mut outer = MAGIC.to_vec();
        let mut tag = None;
        match encrypt {
            None => {
                Cipher::None.encode(&mut outer).unwrap();
                Kdf::None.encode(&mut outer).unwrap();
            }
            Some((cipher, pass)) => {
                let kdf = Kdf::new(KdfAlg::Bcrypt, &mut UnwrapErr(SysRng)).unwrap();
                let (k, iv) = kdf.derive_key_and_iv(cipher, pass).unwrap();
                tag = cipher.encrypt(&k, &iv, &mut section).unwrap();
                cipher.encode(&mut outer).unwrap();
                kdf.encode(&mut outer).unwrap();
            }
        }
        1u32.encode(&mut outer).unwrap();
        key.public_key()
            .key_data()
            .encode_prefixed(&mut outer)
            .unwrap();
        section.encode(&mut outer).unwrap();
        if let Some(tag) = tag {
            outer.extend_from_slice(&tag);
        }
        let b64 = Base64::encode_string(&outer);
        let mut text = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
        for chunk in b64.as_bytes().chunks(70) {
            text.push_str(std::str::from_utf8(chunk).unwrap());
            text.push('\n');
        }
        text.push_str("-----END OPENSSH PRIVATE KEY-----\n");
        text
    }

    fn signs_as(kf: &KeyFile, pass: Option<&str>, key: &PrivateKey) {
        let blob = kf.sign(pass, b"data").unwrap();
        let sig = Signature::decode(&mut &blob[..]).unwrap();
        Verifier::verify(key.public_key(), b"data", &sig).unwrap();
        assert_eq!(kf.public, key.public_key().to_bytes().unwrap());
    }

    #[test]
    fn ssh_key_still_refuses_short_scalars() {
        // When this starts failing, ssh-key reads them itself and this
        // module can go.
        let key = short_key();
        assert!(PrivateKey::from_openssh(openssh_file(&key, None)).is_err());
        // Its own encoder pads, so the round trip hides the problem.
        let own = key.to_openssh(LineEnding::LF).unwrap();
        assert!(PrivateKey::from_openssh(own.as_str()).is_ok());
    }

    #[test]
    fn plain_short_scalar_key_signs() {
        let key = short_key();
        let kf = KeyFile::parse(Path::new("k"), &openssh_file(&key, None)).unwrap();
        assert!(!kf.is_encrypted());
        assert_eq!(kf.comment, "short");
        signs_as(&kf, None, &key);
    }

    #[test]
    fn encrypted_short_scalar_key_signs() {
        let key = short_key();
        for cipher in [
            Cipher::Aes256Ctr,
            Cipher::Aes256Gcm,
            Cipher::ChaCha20Poly1305,
        ] {
            let text = openssh_file(&key, Some((cipher, "pw")));
            let kf = KeyFile::parse(Path::new("k"), &text).unwrap();
            assert!(kf.is_encrypted());
            assert_eq!(kf.sign(None, b"x"), Err(KeyFileError::NeedsPassphrase));
            assert_eq!(
                kf.sign(Some("no"), b"x"),
                Err(KeyFileError::WrongPassphrase)
            );
            signs_as(&kf, Some("pw"), &key);
        }
    }

    #[test]
    fn a_scalar_for_another_point_is_refused() {
        let key = short_key();
        let other = short_key();
        let text = openssh_file(&key, None);
        let theirs = openssh_file(&other, None);
        // Graft the other key's private section onto this key's public key.
        let body = pem_body(&text).unwrap();
        let c = container(&body).unwrap();
        let o = container(&pem_body(&theirs).unwrap()).unwrap();
        assert!(rebuild(&c.public, &o.private, false).is_err());
        assert!(rebuild(&c.public, &c.private, false).unwrap().is_some());
    }

    #[test]
    fn other_keys_and_junk_are_left_alone() {
        assert_eq!(parse("hello").unwrap(), None);
        let ed = PrivateKey::random(&mut UnwrapErr(SysRng), Algorithm::Ed25519).unwrap();
        let text = ed.to_openssh(LineEnding::LF).unwrap();
        assert_eq!(parse(&text).unwrap(), None);
        let truncated = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEA\n-----END OPENSSH PRIVATE KEY-----\n";
        assert_eq!(parse(truncated), Err(KeyFileError::Malformed));
    }

    /// Real `ssh-keygen` files: generates P-256 keys until one has a short
    /// scalar (one in 256), then encrypts a copy with `ssh-keygen -p`.
    #[test]
    fn ssh_keygen_short_scalar_keys_load() {
        use std::process::{Command, Stdio};
        let keygen = |args: &[&str]| {
            Command::new("ssh-keygen")
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if Command::new("ssh-keygen").arg("-?").output().is_err() {
            assert!(
                std::env::var_os("FEROSSH_REQUIRE_SSHD").is_none(),
                "FEROSSH_REQUIRE_SSHD is set but ssh-keygen is missing"
            );
            eprintln!("ssh-keygen not found; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ferossh-short-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("k");
        let path = file.to_str().unwrap();
        let mut found = None;
        for _ in 0..4096 {
            let _ = std::fs::remove_file(&file);
            let _ = std::fs::remove_file(dir.join("k.pub"));
            assert!(keygen(&[
                "-q", "-t", "ecdsa", "-b", "256", "-N", "", "-C", "real", "-f", path
            ]));
            let text = std::fs::read_to_string(&file).unwrap();
            if PrivateKey::from_openssh(&text).is_err() {
                found = Some(text);
                break;
            }
        }
        let text = found.unwrap();
        let kf = KeyFile::parse(Path::new("k"), &text).unwrap();
        assert_eq!(kf.comment, "real");
        let key = parse(&text).unwrap().unwrap();
        signs_as(&kf, None, &key);

        assert!(keygen(&["-q", "-p", "-P", "", "-N", "pw", "-f", path]));
        let locked = std::fs::read_to_string(&file).unwrap();
        let kf = KeyFile::parse(Path::new("k"), &locked).unwrap();
        assert!(kf.is_encrypted());
        assert_eq!(
            kf.sign(Some("no"), b"x"),
            Err(KeyFileError::WrongPassphrase)
        );
        signs_as(&kf, Some("pw"), &key);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
