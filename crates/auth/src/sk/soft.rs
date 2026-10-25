//! A software FIDO2 authenticator, for tests on machines without a
//! security key. It is only built for tests (the `soft-authenticator`
//! feature), never into FeroSSH itself: its "key handle" is the private
//! key, as in OpenSSH's own test authenticator (`regress/misc/sk-dummy`),
//! whose key files it can also sign with.
//!
//! Key handles: 64 bytes are an Ed25519 seed and public key (sk-dummy's
//! and this module's); 32 bytes a P-256 scalar (this module's); a PEM
//! `EC PRIVATE KEY` a P-256 key (sk-dummy's).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use russh::keys::PrivateKey;
use russh::keys::signature::Signer;
use russh::keys::ssh_encoding::{Base64Reader, Reader as _};
use russh::keys::ssh_key::private::Ed25519Keypair;

use super::{
    AD_AT, AD_UP, AD_UV, Assertion, Authenticator, EnrollRequest, SignRequest, SkAlgorithm,
    USER_VERIFICATION, sha256,
};

/// The signing key a key handle holds.
enum Secret {
    Ed25519([u8; 32]),
    P256(p256::ecdsa::SigningKey),
}

/// The P-256 scalar in a SEC1 `ECPrivateKey` (RFC 5915), PEM encoded.
fn sec1_pem_scalar(pem: &[u8]) -> Option<[u8; 32]> {
    let text = std::str::from_utf8(pem).ok()?;
    let b64: String = text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .map(str::trim)
        .collect();
    let mut reader = Base64Reader::new(b64.as_bytes()).ok()?;
    let mut der = vec![0u8; reader.remaining_len()];
    reader.read(&mut der).ok()?;
    // SEQUENCE { INTEGER 1, OCTET STRING (32) d, ... }
    let body = match der.as_slice() {
        [0x30, 0x81, _, rest @ ..] | [0x30, _, rest @ ..] => rest,
        _ => return None,
    };
    match body {
        [0x02, 0x01, 0x01, 0x04, 0x20, rest @ ..] => rest.first_chunk::<32>().copied(),
        _ => None,
    }
}

fn secret(handle: &[u8]) -> Option<Secret> {
    if handle.starts_with(b"-----BEGIN EC PRIVATE KEY-----") {
        let d = sec1_pem_scalar(handle)?;
        return p256::ecdsa::SigningKey::from_slice(&d)
            .ok()
            .map(Secret::P256);
    }
    match handle.len() {
        64 => handle.first_chunk::<32>().map(|s| Secret::Ed25519(*s)),
        32 => p256::ecdsa::SigningKey::from_slice(handle)
            .ok()
            .map(Secret::P256),
        _ => None,
    }
}

/// See the module documentation.
#[derive(Debug)]
pub struct SoftAuthenticator {
    counter: AtomicU32,
    touched: AtomicBool,
    verified: AtomicBool,
}

impl Default for SoftAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftAuthenticator {
    pub fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
            touched: AtomicBool::new(true),
            verified: AtomicBool::new(true),
        }
    }

    /// Whether the next assertions report a touch.
    pub fn set_touched(&self, touched: bool) {
        self.touched.store(touched, Ordering::Relaxed);
    }

    /// Whether the next assertions report a verified user, when asked to.
    pub fn set_user_verified(&self, verified: bool) {
        self.verified.store(verified, Ordering::Relaxed);
    }

    fn flags(&self, user_verification: bool) -> u8 {
        let mut flags = 0;
        if self.touched.load(Ordering::Relaxed) {
            flags |= AD_UP;
        }
        if user_verification && self.verified.load(Ordering::Relaxed) {
            flags |= AD_UV;
        }
        flags
    }
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

impl Authenticator for SoftAuthenticator {
    fn sign(&self, request: &SignRequest<'_>) -> Result<Assertion, String> {
        let flags = self.flags(request.flags & USER_VERIFICATION != 0);
        let counter = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let mut data = sha256(request.application.as_bytes()).to_vec();
        data.push(flags);
        data.extend_from_slice(&counter.to_be_bytes());
        let signed = [&data[..], &sha256(request.message)].concat();
        let signature = match (secret(request.key_handle), request.algorithm) {
            (Some(Secret::Ed25519(seed)), SkAlgorithm::Ed25519) => {
                let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
                let sig = key.try_sign(&signed).map_err(|e| e.to_string())?;
                sig.as_bytes().to_vec()
            }
            (Some(Secret::P256(key)), SkAlgorithm::EcdsaP256) => {
                let sig: p256::ecdsa::Signature = key.sign(&signed);
                sig.to_der().as_bytes().to_vec()
            }
            _ => return Err("the key handle is not one of this authenticator's".into()),
        };
        Ok(Assertion {
            authenticator_data: data,
            signature,
        })
    }

    fn enroll(&self, request: &EnrollRequest<'_>) -> Result<Vec<u8>, String> {
        let (handle, cose) = match request.algorithm {
            SkAlgorithm::Ed25519 => {
                let seed = random::<32>()?;
                let public = Ed25519Keypair::from_seed(&seed).public.0;
                // OKP, EdDSA, Ed25519, x.
                let mut cose = vec![0xa4, 0x01, 0x01, 0x03, 0x27, 0x20, 0x06, 0x21, 0x58, 0x20];
                cose.extend_from_slice(&public);
                ([seed, public].concat(), cose)
            }
            SkAlgorithm::EcdsaP256 => {
                let (d, key) = loop {
                    let d = random::<32>()?;
                    if let Ok(k) = p256::ecdsa::SigningKey::from_slice(&d) {
                        break (d, k);
                    }
                };
                let point = key.verifying_key().to_sec1_point(false);
                let xy = point.as_bytes().get(1..).ok_or("bad point")?;
                let (x, y) = xy.split_at(32);
                // EC2, ES256, P-256, x, y.
                let mut cose = vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20];
                cose.extend_from_slice(x);
                cose.extend_from_slice(&[0x22, 0x58, 0x20]);
                cose.extend_from_slice(y);
                (d.to_vec(), cose)
            }
        };
        let mut data = sha256(request.application.as_bytes()).to_vec();
        data.push(self.flags(request.user_verification) | AD_AT);
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&[0; 16]);
        data.extend_from_slice(&u16::try_from(handle.len()).unwrap_or(0).to_be_bytes());
        data.extend_from_slice(&handle);
        data.extend_from_slice(&cose);
        Ok(data)
    }
}

/// Shared, so a test can change what it reports while a broker owns it.
impl Authenticator for std::sync::Arc<SoftAuthenticator> {
    fn sign(&self, request: &SignRequest<'_>) -> Result<Assertion, String> {
        self.as_ref().sign(request)
    }

    fn enroll(&self, request: &EnrollRequest<'_>) -> Result<Vec<u8>, String> {
        self.as_ref().enroll(request)
    }
}
