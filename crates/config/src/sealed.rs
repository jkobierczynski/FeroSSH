//! FeroSSH's encrypted file: an exported connections file and the master
//! password vault are both this. A passphrase is stretched with Argon2id
//! into a 256-bit key, and the content is sealed with ChaCha20-Poly1305,
//! so a wrong passphrase or a changed byte is refused, never half read.
//!
//! The file is text, so it can sit in OneDrive or be committed to git:
//!
//! ```text
//! FeroSSH encrypted file v1
//! kdf argon2id m=65536 t=3 p=1
//! salt 3q2+7wAAAAAAAAAAAAAAAA==
//! nonce AAAAAAAAAAAAAAAA
//! cipher chacha20-poly1305
//!
//! <base64 of the sealed content and its tag, 64 characters a line>
//! ```
//!
//! The header is authenticated as well (as the AEAD's associated data, in
//! its canonical form, so a line-ending change by git or an editor does
//! not break the file but a weaker `kdf` line does). A new salt is drawn
//! whenever a passphrase is set; every write draws a new nonce, so one
//! key can seal many versions of a vault without a KDF run each time.

use std::fmt;

use base64ct::{Base64, Encoding};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use zeroize::Zeroizing;

const MAGIC: &str = "FeroSSH encrypted file v1";
const CIPHER: &str = "chacha20-poly1305";
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;
/// The most a file may hold once decoded: far more than any set of
/// connections, little enough that a hostile file cannot exhaust memory.
pub const MAX_CONTENT: usize = 16 << 20;

/// Argon2id's cost: memory in KiB, passes and lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kdf {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Kdf {
    /// What new files use: 64 MiB and three passes (RFC 9106's second
    /// recommendation, with one lane), about half a second on a laptop.
    pub const DEFAULT: Kdf = Kdf {
        m_kib: 64 * 1024,
        t: 3,
        p: 1,
    };

    /// The lowest costs accepted, for tests.
    pub const LOWEST: Kdf = Kdf {
        m_kib: 8 * 1024,
        t: 1,
        p: 1,
    };

    /// Whether a file's costs are ones FeroSSH accepts: not so weak that
    /// a guessed passphrase is cheap to try, not so large that opening a
    /// hostile file takes the PC's memory or minutes.
    fn acceptable(self) -> bool {
        (8 * 1024..=1024 * 1024).contains(&self.m_kib)
            && (1..=16).contains(&self.t)
            && (1..=8).contains(&self.p)
    }

    fn line(self) -> String {
        format!("kdf argon2id m={} t={} p={}", self.m_kib, self.t, self.p)
    }

    fn parse(line: &str) -> Option<Kdf> {
        let mut words = line.split_ascii_whitespace();
        if words.next()? != "kdf" || words.next()? != "argon2id" {
            return None;
        }
        let mut get = |name: &str| -> Option<u32> {
            let w = words.next()?;
            w.strip_prefix(name)?.strip_prefix('=')?.parse().ok()
        };
        let kdf = Kdf {
            m_kib: get("m")?,
            t: get("t")?,
            p: get("p")?,
        };
        words.next().is_none().then_some(kdf)
    }
}

/// Why a file could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Not a FeroSSH encrypted file, or a newer version of one.
    NotSealed,
    /// The header asks for an unknown cipher or for costs outside the
    /// accepted range.
    Unsupported,
    /// A wrong passphrase, or the file was changed.
    WrongPassphrase,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OpenError::NotSealed => "This is not a FeroSSH encrypted file.",
            OpenError::Unsupported => {
                "This encrypted file was made with settings this FeroSSH does not accept."
            }
            OpenError::WrongPassphrase => {
                "Wrong passphrase, or the file was changed after it was written."
            }
        })
    }
}

/// The parsed header of a sealed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub kdf: Kdf,
    pub salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
}

impl Header {
    /// The header as it is written and authenticated.
    fn canonical(&self) -> String {
        let mut salt = [0u8; 24];
        let mut nonce = [0u8; 16];
        let salt = Base64::encode(&self.salt, &mut salt).unwrap_or_default();
        let nonce = Base64::encode(&self.nonce, &mut nonce).unwrap_or_default();
        format!(
            "{MAGIC}\n{}\nsalt {salt}\nnonce {nonce}\ncipher {CIPHER}\n",
            self.kdf.line()
        )
    }
}

/// Splits a file into its header and the base64 body.
fn split(text: &str) -> Result<(Header, Vec<u8>), OpenError> {
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    if lines.next().map(str::trim) != Some(MAGIC) {
        return Err(OpenError::NotSealed);
    }
    let kdf = lines
        .next()
        .and_then(Kdf::parse)
        .ok_or(OpenError::Unsupported)?;
    let field = |line: Option<&str>, name: &str| -> Option<Vec<u8>> {
        let value = line?.strip_prefix(name)?.strip_prefix(' ')?;
        Base64::decode_vec(value.trim()).ok()
    };
    let salt = field(lines.next(), "salt")
        .and_then(|s| <[u8; SALT_LEN]>::try_from(s).ok())
        .ok_or(OpenError::NotSealed)?;
    let nonce = field(lines.next(), "nonce")
        .and_then(|s| <[u8; NONCE_LEN]>::try_from(s).ok())
        .ok_or(OpenError::NotSealed)?;
    if lines.next().map(str::trim) != Some(&format!("cipher {CIPHER}")) {
        return Err(OpenError::Unsupported);
    }
    if !kdf.acceptable() {
        return Err(OpenError::Unsupported);
    }
    if lines.next().map(str::trim) != Some("") {
        return Err(OpenError::NotSealed);
    }
    let mut body = String::new();
    for line in lines {
        body.push_str(line.trim());
        if body.len() > MAX_CONTENT / 3 * 4 + 64 {
            return Err(OpenError::NotSealed);
        }
    }
    let body = Base64::decode_vec(&body).map_err(|_| OpenError::NotSealed)?;
    if body.len() < CHACHA20_POLY1305.tag_len() {
        return Err(OpenError::NotSealed);
    }
    Ok((Header { kdf, salt, nonce }, body))
}

/// The header of `text`, to derive the key it needs.
pub fn header(text: &str) -> Result<Header, OpenError> {
    split(text).map(|(h, _)| h)
}

/// A key derived from a passphrase, with the salt and costs it came from:
/// what the vault keeps while it is unlocked, so a save needs no KDF run.
pub struct Key {
    key: LessSafeKey,
    kdf: Kdf,
    salt: [u8; SALT_LEN],
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Key")
            .field("kdf", &self.kdf)
            .finish_non_exhaustive()
    }
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).map_err(|e| format!("Windows gave no random bytes ({e})."))?;
    Ok(out)
}

impl Key {
    /// A key for a new file: a fresh salt and the default costs.
    pub fn new(passphrase: &str) -> Result<Key, String> {
        Key::with_kdf(passphrase, Kdf::DEFAULT)
    }

    /// A key for a new file with other costs (tests use the lowest).
    pub fn with_kdf(passphrase: &str, kdf: Kdf) -> Result<Key, String> {
        if !kdf.acceptable() {
            return Err("Those key settings are not accepted.".into());
        }
        Key::derive(passphrase, kdf, random()?)
    }

    /// The key `passphrase` gives with the file's salt and costs.
    pub fn for_header(passphrase: &str, header: &Header) -> Result<Key, OpenError> {
        Key::derive(passphrase, header.kdf, header.salt).map_err(|_| OpenError::Unsupported)
    }

    pub fn derive(passphrase: &str, kdf: Kdf, salt: [u8; SALT_LEN]) -> Result<Key, String> {
        let params = argon2::Params::new(kdf.m_kib, kdf.t, kdf.p, Some(KEY_LEN))
            .map_err(|e| format!("Bad key settings ({e})."))?;
        let argon =
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        let mut raw = Zeroizing::new([0u8; KEY_LEN]);
        argon
            .hash_password_into(passphrase.as_bytes(), &salt, raw.as_mut())
            .map_err(|e| format!("The key could not be made ({e})."))?;
        let key = UnboundKey::new(&CHACHA20_POLY1305, raw.as_ref())
            .map_err(|_| "The key could not be made.".to_owned())?;
        Ok(Key {
            key: LessSafeKey::new(key),
            kdf,
            salt,
        })
    }

    /// Whether this key was made for `header`'s salt and costs (it may
    /// still be the wrong passphrase).
    pub fn fits(&self, header: &Header) -> bool {
        self.kdf == header.kdf && self.salt == header.salt
    }

    /// `content` as a sealed file, under a fresh nonce.
    pub fn seal(&self, content: &[u8]) -> Result<String, String> {
        if content.len() > MAX_CONTENT {
            return Err("There is too much to encrypt.".into());
        }
        let header = Header {
            kdf: self.kdf,
            salt: self.salt,
            nonce: random()?,
        };
        let aad = header.canonical();
        let mut buf = Zeroizing::new(content.to_vec());
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(header.nonce),
                Aad::from(aad.as_bytes()),
                &mut *buf,
            )
            .map_err(|_| "The file could not be encrypted.".to_owned())?;
        let body = Base64::encode_string(&buf);
        let mut out = aad;
        out.push('\n');
        let mut rest = body.as_str();
        while !rest.is_empty() {
            let (line, after) = rest.split_at(rest.len().min(64));
            out.push_str(line);
            out.push('\n');
            rest = after;
        }
        Ok(out)
    }

    /// The content of a file this key sealed.
    pub fn open(&self, text: &str) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        let (header, body) = split(text)?;
        if !self.fits(&header) {
            return Err(OpenError::WrongPassphrase);
        }
        let aad = header.canonical();
        let mut buf = Zeroizing::new(body);
        let len = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(header.nonce),
                Aad::from(aad.as_bytes()),
                &mut buf,
            )
            .map_err(|_| OpenError::WrongPassphrase)?
            .len();
        buf.truncate(len);
        Ok(buf)
    }
}

/// Opens `text` with `passphrase`: the content, and the key to seal the
/// next version with.
pub fn open(passphrase: &str, text: &str) -> Result<(Zeroizing<Vec<u8>>, Key), OpenError> {
    let header = header(text)?;
    let key = Key::for_header(passphrase, &header)?;
    let content = key.open(text)?;
    Ok((content, key))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// The smallest accepted costs, so debug tests stay fast.
    const QUICK: Kdf = Kdf::LOWEST;

    fn key(pass: &str) -> Key {
        Key::derive(pass, QUICK, [7; SALT_LEN]).unwrap()
    }

    #[test]
    fn round_trip() {
        let k = key("correct horse");
        let text = k.seal(b"Host web\n    User me\n").unwrap();
        assert!(text.starts_with("FeroSSH encrypted file v1\nkdf argon2id m=8192 t=1 p=1\n"));
        let (content, again) = open("correct horse", &text).unwrap();
        assert_eq!(&content[..], b"Host web\n    User me\n");
        // The key it hands back seals the next version.
        let next = again.seal(b"two").unwrap();
        assert_eq!(&k.open(&next).unwrap()[..], b"two");
        // A fresh nonce each time.
        assert_ne!(k.seal(b"x").unwrap(), k.seal(b"x").unwrap());
    }

    #[test]
    fn empty_content() {
        let k = key("p");
        let text = k.seal(b"").unwrap();
        assert!(k.open(&text).unwrap().is_empty());
    }

    #[test]
    fn wrong_passphrase_and_changes_are_refused() {
        let text = key("right").seal(b"secret").unwrap();
        assert_eq!(open("wrong", &text).err(), Some(OpenError::WrongPassphrase));
        // A flipped bit in the body.
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let last = lines.last_mut().unwrap();
        let c = last.remove(0);
        last.insert(0, if c == 'A' { 'B' } else { 'A' });
        let changed = lines.join("\n");
        assert!(open("right", &changed).is_err());
        // A changed salt gives another key: refused.
        let salt = text.replace(
            "salt BwcHBwcHBwcHBwcHBwcHBw==",
            "salt CAcHBwcHBwcHBwcHBwcHBw==",
        );
        assert_ne!(salt, text);
        assert_eq!(open("right", &salt).err(), Some(OpenError::WrongPassphrase));
    }

    #[test]
    fn header_is_authenticated() {
        let k = key("p");
        let text = k.seal(b"abc").unwrap();
        // Raise the cost: a valid header, but not the one sealed.
        let higher = text.replace("t=1", "t=2");
        let h = header(&higher).unwrap();
        let k2 = Key::for_header("p", &h).unwrap();
        assert_eq!(k2.open(&higher).err(), Some(OpenError::WrongPassphrase));
    }

    #[test]
    fn crlf_and_trailing_space_still_open() {
        let k = key("p");
        let text = k.seal(&[1u8; 200]).unwrap();
        let crlf = text.replace('\n', "\r\n");
        assert_eq!(&k.open(&crlf).unwrap()[..], &[1u8; 200][..]);
    }

    #[test]
    fn weak_or_huge_costs_are_refused() {
        let text = key("p").seal(b"abc").unwrap();
        for (from, to) in [
            ("m=8192", "m=1024"),
            ("m=8192", "m=99999999"),
            ("t=1", "t=0"),
            ("p=1", "p=64"),
        ] {
            let bad = text.replace(from, to);
            assert_eq!(header(&bad).err(), Some(OpenError::Unsupported), "{to}");
        }
        assert_eq!(
            header(&text.replace("chacha20-poly1305", "rot13")).err(),
            Some(OpenError::Unsupported)
        );
    }

    #[test]
    fn other_files_are_not_sealed() {
        for t in ["", "Host web\n", MAGIC, "FeroSSH encrypted file v2\n"] {
            assert!(header(t).is_err(), "{t:?}");
        }
    }

    #[test]
    fn default_costs_are_accepted() {
        assert!(Kdf::DEFAULT.acceptable());
    }
}
