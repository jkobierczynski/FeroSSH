//! FIDO2 security keys: `sk-ssh-ed25519@openssh.com` and
//! `sk-ecdsa-sha2-nistp256@openssh.com` (OpenSSH's `PROTOCOL.u2f`).
//!
//! An sk key file holds no secret. It names the WebAuthn relying party
//! (the "application", `ssh:` unless set otherwise), the credential's key
//! handle and flags; the private key never leaves the authenticator. An
//! [`Authenticator`] makes the assertion (Windows' `webauthn.dll` in the
//! window, a software one in tests) and this module checks it and turns
//! it into the SSH signature:
//!
//! ```text
//! string  "sk-ssh-ed25519@openssh.com" | "sk-ecdsa-sha2-nistp256@openssh.com"
//! string  signature (Ed25519: 64 bytes; ECDSA: mpint r, mpint s)
//! byte    flags     (from the authenticator data)
//! uint32  counter
//! ```
//!
//! which the server checks over `SHA-256(application) || flags || counter
//! || SHA-256(data)`: exactly what an authenticator signs when the client
//! data hash is `SHA-256(data)`. So the data to sign is passed to the
//! authenticator as the client data, and it hashes it.
//!
//! Keys held by ssh-agent or Pageant need none of this: the agent talks to
//! the token and returns the finished signature.

use russh::keys::signature::Verifier;
use russh::keys::ssh_encoding::Decode;
use russh::keys::ssh_key::private::{KeypairData, SkEcdsaSha2NistP256, SkEd25519};
use russh::keys::ssh_key::public::{self, EcdsaPublicKey, Ed25519PublicKey};
use russh::keys::ssh_key::sha2::{Digest, Sha256};
use russh::keys::ssh_key::{LineEnding, Signature};
use russh::keys::{PrivateKey, PublicKey};

/// Key flag: the authenticator must see a touch (`SSH_SK_USER_PRESENCE_REQD`).
pub const USER_PRESENCE: u8 = 0x01;
/// Key flag: a PIN or biometric too (`SSH_SK_USER_VERIFICATION_REQD`).
pub const USER_VERIFICATION: u8 = 0x04;

/// Authenticator data flags (WebAuthn section 6.1).
const AD_UP: u8 = 0x01;
const AD_UV: u8 = 0x04;
const AD_AT: u8 = 0x40;
const AD_ED: u8 = 0x80;

/// The application OpenSSH uses when none is given.
pub const DEFAULT_APPLICATION: &str = "ssh:";

/// The two security key types OpenSSH defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkAlgorithm {
    Ed25519,
    EcdsaP256,
}

impl SkAlgorithm {
    pub fn ssh_name(self) -> &'static str {
        match self {
            Self::Ed25519 => "sk-ssh-ed25519@openssh.com",
            Self::EcdsaP256 => "sk-ecdsa-sha2-nistp256@openssh.com",
        }
    }

    /// The COSE algorithm identifier (EdDSA or ES256).
    pub fn cose(self) -> i32 {
        match self {
            Self::Ed25519 => -8,
            Self::EcdsaP256 => -7,
        }
    }
}

/// What an authenticator is asked to sign.
#[derive(Debug)]
pub struct SignRequest<'a> {
    pub algorithm: SkAlgorithm,
    /// The relying party id.
    pub application: &'a str,
    /// The credential id.
    pub key_handle: &'a [u8],
    /// The key's flags: [`USER_PRESENCE`], [`USER_VERIFICATION`].
    pub flags: u8,
    /// The data to sign, given to the authenticator as the client data;
    /// its SHA-256 is the client data hash.
    pub message: &'a [u8],
}

/// An authenticator's answer to a [`SignRequest`].
#[derive(Debug, Clone)]
pub struct Assertion {
    pub authenticator_data: Vec<u8>,
    /// Ed25519: 64 bytes. ECDSA: ASN.1 DER, as WebAuthn returns it.
    pub signature: Vec<u8>,
}

/// What an authenticator is asked to create.
#[derive(Debug)]
pub struct EnrollRequest<'a> {
    pub algorithm: SkAlgorithm,
    pub application: &'a str,
    /// Random bytes, given as the client data.
    pub challenge: &'a [u8],
    /// Only the computer's own authenticator (Windows Hello).
    pub platform: bool,
    /// Ask for a PIN or biometric on every use.
    pub user_verification: bool,
}

/// A FIDO2 authenticator, or something that reaches one.
pub trait Authenticator: Send {
    /// Makes an assertion with an existing credential.
    fn sign(&self, request: &SignRequest<'_>) -> Result<Assertion, String>;
    /// Creates a credential; returns its authenticator data, which holds
    /// the credential id and public key.
    fn enroll(&self, request: &EnrollRequest<'_>) -> Result<Vec<u8>, String>;
}

/// No authenticator on this system.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unavailable;

impl Authenticator for Unavailable {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Assertion, String> {
        Err(UNAVAILABLE.into())
    }

    fn enroll(&self, _: &EnrollRequest<'_>) -> Result<Vec<u8>, String> {
        Err(UNAVAILABLE.into())
    }
}

const UNAVAILABLE: &str = "security keys in key files need Windows' WebAuthn; \
                           load the key into ssh-agent to use it here";

/// The parts of an sk private key file an authenticator needs.
#[derive(Debug, Clone)]
pub struct SkKey {
    pub algorithm: SkAlgorithm,
    /// The public key in SSH wire format.
    pub public: Vec<u8>,
    pub application: String,
    pub key_handle: Vec<u8>,
    pub flags: u8,
}

impl SkKey {
    /// The sk parts of a decrypted key, if it is an sk key.
    pub fn from_private(key: &PrivateKey) -> Option<Self> {
        let public = key.public_key().to_bytes().ok()?;
        let (algorithm, application, key_handle, flags) = match key.key_data() {
            KeypairData::SkEd25519(k) => (
                SkAlgorithm::Ed25519,
                k.public().application(),
                k.key_handle(),
                k.flags(),
            ),
            KeypairData::SkEcdsaSha2NistP256(k) => (
                SkAlgorithm::EcdsaP256,
                k.public().application(),
                k.key_handle(),
                k.flags(),
            ),
            _ => return None,
        };
        Some(Self {
            algorithm,
            public,
            application: application.to_owned(),
            key_handle: key_handle.to_vec(),
            flags,
        })
    }

    /// Asks `authenticator` to sign `message`; returns the SSH signature
    /// blob, checked against the public key.
    pub fn sign(
        &self,
        authenticator: &dyn Authenticator,
        message: &[u8],
    ) -> Result<Vec<u8>, String> {
        let assertion = authenticator.sign(&SignRequest {
            algorithm: self.algorithm,
            application: &self.application,
            key_handle: &self.key_handle,
            flags: self.flags,
            message,
        })?;
        let blob = signature_blob(self, &assertion)?;
        verify(&self.public, message, &blob)?;
        Ok(blob)
    }
}

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&u32::try_from(s.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(s);
}

/// The flags and counter of authenticator data with no extensions and no
/// attested credential, after checking it is for `application`.
fn parse_assertion_data(data: &[u8], application: &str) -> Result<(u8, u32), String> {
    let (rp, rest) = data
        .split_first_chunk::<32>()
        .ok_or("the authenticator data is truncated")?;
    if *rp != sha256(application.as_bytes()) {
        return Err("the authenticator answered for another application".into());
    }
    let (&flags, rest) = rest
        .split_first()
        .ok_or("the authenticator data is truncated")?;
    let (counter, rest) = rest
        .split_first_chunk::<4>()
        .ok_or("the authenticator data is truncated")?;
    // OpenSSH signs no extensions, so a server could not check them.
    if !rest.is_empty() || flags & (AD_ED | AD_AT) != 0 {
        return Err("the authenticator added data OpenSSH does not accept".into());
    }
    Ok((flags, u32::from_be_bytes(*counter)))
}

/// Checks `assertion` and builds the SSH signature blob.
fn signature_blob(key: &SkKey, assertion: &Assertion) -> Result<Vec<u8>, String> {
    let (flags, counter) = parse_assertion_data(&assertion.authenticator_data, &key.application)?;
    if key.flags & USER_PRESENCE != 0 && flags & AD_UP == 0 {
        return Err("the security key was not touched".into());
    }
    if key.flags & USER_VERIFICATION != 0 && flags & AD_UV == 0 {
        return Err("the security key did not verify the user".into());
    }
    let signature = match key.algorithm {
        SkAlgorithm::Ed25519 if assertion.signature.len() == 64 => assertion.signature.clone(),
        SkAlgorithm::Ed25519 => return Err("malformed Ed25519 signature".into()),
        SkAlgorithm::EcdsaP256 => der_to_ssh(&assertion.signature)
            .ok_or("malformed ECDSA signature from the authenticator")?,
    };
    let mut blob = Vec::new();
    put_string(&mut blob, key.algorithm.ssh_name().as_bytes());
    put_string(&mut blob, &signature);
    blob.push(flags);
    blob.extend_from_slice(&counter.to_be_bytes());
    Ok(blob)
}

/// Checks an SSH signature blob against a public key, as the server will.
pub fn verify(public: &[u8], message: &[u8], blob: &[u8]) -> Result<(), String> {
    let key = PublicKey::from_bytes(public).map_err(|e| e.to_string())?;
    let sig = Signature::decode(&mut &blob[..]).map_err(|e| e.to_string())?;
    Verifier::verify(&key, message, &sig)
        .map_err(|_| "the security key's signature does not verify".to_owned())
}

/// One DER element: (tag, contents, rest).
fn der(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = data.split_first()?;
    let (&len, rest) = rest.split_first()?;
    let (len, rest) = match len {
        0..=0x7f => (usize::from(len), rest),
        0x81 => {
            let (&l, rest) = rest.split_first()?;
            (usize::from(l), rest)
        }
        _ => return None,
    };
    (len <= rest.len()).then(|| {
        let (body, rest) = rest.split_at(len);
        (tag, body, rest)
    })
}

/// An SSH mpint from a big-endian unsigned integer.
fn put_mpint(out: &mut Vec<u8>, n: &[u8]) {
    let zeros = n.iter().take_while(|&&b| b == 0).count();
    let n = n.get(zeros..).unwrap_or_default();
    let pad = n.first().is_some_and(|&b| b & 0x80 != 0);
    let len = n.len() + usize::from(pad);
    out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
    if pad {
        out.push(0);
    }
    out.extend_from_slice(n);
}

/// An ECDSA signature from ASN.1 DER `SEQUENCE { r INTEGER, s INTEGER }`
/// to SSH's `mpint r, mpint s`.
fn der_to_ssh(sig: &[u8]) -> Option<Vec<u8>> {
    let (0x30, body, []) = der(sig)? else {
        return None;
    };
    let (0x02, r, rest) = der(body)? else {
        return None;
    };
    let (0x02, s, []) = der(rest)? else {
        return None;
    };
    // P-256 scalars are 32 bytes, plus a sign byte.
    if r.is_empty() || s.is_empty() || r.len() > 33 || s.len() > 33 {
        return None;
    }
    let mut out = Vec::new();
    put_mpint(&mut out, r);
    put_mpint(&mut out, s);
    Some(out)
}

/// A minimal CBOR reader for COSE keys: unsigned and negative integers,
/// byte strings and maps.
struct Cbor<'a>(&'a [u8]);

enum Item<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    Map(u64),
}

impl<'a> Cbor<'a> {
    fn item(&mut self) -> Option<Item<'a>> {
        let (&first, rest) = self.0.split_first()?;
        self.0 = rest;
        let major = first >> 5;
        let arg = match first & 0x1f {
            n @ 0..=23 => u64::from(n),
            24 => u64::from(self.take::<1>()?[0]),
            25 => u64::from(u16::from_be_bytes(self.take::<2>()?)),
            26 => u64::from(u32::from_be_bytes(self.take::<4>()?)),
            _ => return None,
        };
        match major {
            0 => Some(Item::Int(i64::try_from(arg).ok()?)),
            1 => Some(Item::Int(-1 - i64::try_from(arg).ok()?)),
            2 => {
                let len = usize::try_from(arg).ok()?;
                if len > self.0.len() {
                    return None;
                }
                let (b, rest) = self.0.split_at(len);
                self.0 = rest;
                Some(Item::Bytes(b))
            }
            5 => Some(Item::Map(arg)),
            _ => None,
        }
    }

    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (b, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*b)
    }
}

/// The SSH public key data of a COSE key (RFC 9053): an Ed25519 OKP key or
/// a P-256 EC2 key; `rest` is what follows it.
fn cose_key(data: &[u8], algorithm: SkAlgorithm) -> Option<(Vec<u8>, &[u8])> {
    let mut r = Cbor(data);
    let Item::Map(n) = r.item()? else {
        return None;
    };
    let (mut kty, mut alg, mut crv) = (None, None, None);
    let (mut x, mut y) = (None, None);
    for _ in 0..n.min(16) {
        let Item::Int(label) = r.item()? else {
            return None;
        };
        match (label, r.item()?) {
            (1, Item::Int(v)) => kty = Some(v),
            (3, Item::Int(v)) => alg = Some(v),
            (-1, Item::Int(v)) => crv = Some(v),
            (-2, Item::Bytes(v)) => x = Some(v),
            (-3, Item::Bytes(v)) => y = Some(v),
            (_, Item::Map(_)) => return None,
            _ => {}
        }
    }
    if n > 16 || alg != Some(i64::from(algorithm.cose())) {
        return None;
    }
    let point = match algorithm {
        // OKP, Ed25519.
        SkAlgorithm::Ed25519 if kty == Some(1) && crv == Some(6) => {
            x.filter(|x| x.len() == 32)?.to_vec()
        }
        // EC2, P-256: an uncompressed SEC1 point.
        SkAlgorithm::EcdsaP256 if kty == Some(2) && crv == Some(1) => {
            let (x, y) = (x?, y?);
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            let mut p = vec![4];
            p.extend_from_slice(x);
            p.extend_from_slice(y);
            p
        }
        _ => return None,
    };
    Some((point, r.0))
}

/// A new key from an authenticator: the OpenSSH private key file (which
/// holds only the key handle) and its `.pub` line.
#[derive(Debug)]
pub struct Enrolled {
    pub private: String,
    pub public: String,
}

/// Creates a credential with `authenticator` and writes it as an OpenSSH
/// sk key, as `ssh-keygen -t ecdsa-sk` / `-t ed25519-sk` would.
pub fn enroll(
    authenticator: &dyn Authenticator,
    request: &EnrollRequest<'_>,
    comment: &str,
) -> Result<Enrolled, String> {
    let data = authenticator.enroll(request)?;
    let malformed = || "the authenticator returned malformed credential data".to_owned();
    let (rp, rest) = data.split_first_chunk::<32>().ok_or_else(malformed)?;
    if *rp != sha256(request.application.as_bytes()) {
        return Err("the authenticator answered for another application".into());
    }
    let (&flags, rest) = rest.split_first().ok_or_else(malformed)?;
    if flags & AD_AT == 0 {
        return Err(malformed());
    }
    // Counter, then the attested credential: AAGUID, id length, id, key.
    let rest = rest.get(4 + 16..).ok_or_else(malformed)?;
    let (len, rest) = rest.split_first_chunk::<2>().ok_or_else(malformed)?;
    let len = usize::from(u16::from_be_bytes(*len));
    if len > rest.len() || len > 255 {
        return Err(malformed());
    }
    let (handle, rest) = rest.split_at(len);
    let (point, rest) = cose_key(rest, request.algorithm).ok_or_else(malformed)?;
    if !rest.is_empty() && flags & AD_ED == 0 {
        return Err(malformed());
    }
    let key_flags = USER_PRESENCE
        | if request.user_verification {
            USER_VERIFICATION
        } else {
            0
        };
    let app = request.application;
    let keypair = match request.algorithm {
        SkAlgorithm::Ed25519 => {
            let point: [u8; 32] = point.try_into().map_err(|_| malformed())?;
            let public = public::SkEd25519::new(Ed25519PublicKey(point), app);
            KeypairData::SkEd25519(
                SkEd25519::new(public, key_flags, handle).map_err(|e| e.to_string())?,
            )
        }
        SkAlgorithm::EcdsaP256 => {
            let EcdsaPublicKey::NistP256(point) =
                EcdsaPublicKey::from_sec1_bytes(&point).map_err(|_| malformed())?
            else {
                return Err(malformed());
            };
            let public = public::SkEcdsaSha2NistP256::new(point, app);
            KeypairData::SkEcdsaSha2NistP256(
                SkEcdsaSha2NistP256::new(public, key_flags, handle).map_err(|e| e.to_string())?,
            )
        }
    };
    let key = PrivateKey::new(keypair, comment).map_err(|e| e.to_string())?;
    let private = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| e.to_string())?
        .to_string();
    let public = key.public_key().to_openssh().map_err(|e| e.to_string())?;
    Ok(Enrolled {
        private,
        public: format!("{public}\n"),
    })
}

#[cfg(any(test, feature = "soft-authenticator"))]
pub mod soft;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::soft::SoftAuthenticator;
    use super::*;

    fn new_key(alg: SkAlgorithm, uv: bool) -> (SoftAuthenticator, SkKey) {
        let soft = SoftAuthenticator::new();
        let enrolled = enroll(
            &soft,
            &EnrollRequest {
                algorithm: alg,
                application: DEFAULT_APPLICATION,
                challenge: b"challenge",
                platform: false,
                user_verification: uv,
            },
            "soft",
        )
        .unwrap();
        let key = PrivateKey::from_openssh(&enrolled.private).unwrap();
        assert!(
            enrolled.public.starts_with(alg.ssh_name()),
            "{}",
            enrolled.public
        );
        assert!(enrolled.public.trim_end().ends_with(" soft"));
        (soft, SkKey::from_private(&key).unwrap())
    }

    #[test]
    fn enrolled_keys_sign_and_verify() {
        for alg in [SkAlgorithm::Ed25519, SkAlgorithm::EcdsaP256] {
            let (soft, key) = new_key(alg, false);
            assert_eq!(key.flags, USER_PRESENCE);
            assert_eq!(key.application, "ssh:");
            let blob = key.sign(&soft, b"login request").unwrap();
            verify(&key.public, b"login request", &blob).unwrap();
            // The counter goes up, and is signed.
            let again = key.sign(&soft, b"login request").unwrap();
            assert_ne!(blob, again);
            assert!(verify(&key.public, b"another request", &blob).is_err());
        }
    }

    #[test]
    fn refuses_assertions_that_do_not_fit() {
        let (soft, key) = new_key(SkAlgorithm::EcdsaP256, true);
        assert_eq!(key.flags, USER_PRESENCE | USER_VERIFICATION);
        key.sign(&soft, b"m").unwrap();

        soft.set_user_verified(false);
        assert!(
            key.sign(&soft, b"m")
                .unwrap_err()
                .contains("verify the user")
        );
        soft.set_user_verified(true);

        soft.set_touched(false);
        assert!(key.sign(&soft, b"m").unwrap_err().contains("not touched"));
        soft.set_touched(true);

        let mut other = key.clone();
        other.application = "ssh:other".into();
        // The soft authenticator signs for whatever it is asked; the
        // public key names "ssh:", so the signature cannot verify.
        assert!(other.sign(&soft, b"m").is_err());

        let a = Assertion {
            authenticator_data: [&sha256(b"ssh:")[..], &[AD_UP | AD_ED, 0, 0, 0, 1, 0xa0]].concat(),
            signature: vec![],
        };
        assert!(
            signature_blob(&key, &a)
                .unwrap_err()
                .contains("does not accept")
        );
        let a = Assertion {
            authenticator_data: [&sha256(b"web")[..], &[AD_UP, 0, 0, 0, 1]].concat(),
            signature: vec![],
        };
        assert!(
            signature_blob(&key, &a)
                .unwrap_err()
                .contains("another application")
        );
    }

    #[test]
    fn der_signatures_become_mpints() {
        // r with its sign byte, s short.
        let mut der = vec![0x30, 0x27, 0x02, 0x21, 0x00];
        der.extend([0x80; 32]);
        der.extend([0x02, 0x02, 0x00, 0x7f]);
        let ssh = der_to_ssh(&der).unwrap();
        assert_eq!(&ssh[..5], &[0, 0, 0, 0x21, 0]);
        assert_eq!(&ssh[37..], &[0, 0, 0, 1, 0x7f]);
        assert!(der_to_ssh(&der[..der.len() - 1]).is_none());
        assert!(der_to_ssh(&[0x30, 0x00]).is_none());
        assert!(der_to_ssh(b"").is_none());
    }

    #[test]
    fn unavailable_says_what_to_do() {
        let (_, key) = new_key(SkAlgorithm::Ed25519, false);
        assert!(
            key.sign(&Unavailable, b"m")
                .unwrap_err()
                .contains("ssh-agent")
        );
    }
}
