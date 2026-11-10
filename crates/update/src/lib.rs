//! The opt-in update check's update file.
//!
//! Each release publishes `update.txt` and its Ed25519 signature
//! `update.txt.sig` next to the exes. FeroSSH fetches both from the latest
//! release, checks the signature against the public key built into the
//! exe (`update-key.pub`), and only then reads the file:
//!
//! ```text
//! FeroSSH update 1
//! version 1.0.1
//! released 2026-11-02
//! url https://github.com/jkobierczynski/FeroSSH/releases/tag/v1.0.1
//! ```
//!
//! Nothing in the file is looked at before the signature checks out, and
//! the file only ever leads to a page on the project's Releases: FeroSSH
//! never downloads or runs an update by itself.

use std::cmp::Ordering;
use std::fmt;

use base64ct::{Base64, Encoding};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

/// Where the latest release's update file and signature are published.
pub const UPDATE_URL: &str =
    "https://github.com/jkobierczynski/FeroSSH/releases/latest/download/update.txt";
pub const SIGNATURE_URL: &str =
    "https://github.com/jkobierczynski/FeroSSH/releases/latest/download/update.txt.sig";

/// The only pages an update file may point to.
const RELEASES: &str = "https://github.com/jkobierczynski/FeroSSH/releases/";

/// The first line of every update file.
const MAGIC: &str = "FeroSSH update 1";

/// The most an update file or signature may be; anything longer is refused
/// unread.
pub const MAX_SIZE: usize = 4096;

/// Seconds between automatic checks, when they are on: a week.
pub const INTERVAL_SECS: u64 = 7 * 24 * 60 * 60;

/// The release signing key's public half, `update-key.pub` as written by
/// `cargo run -p ferossh-update --example keygen`. Empty until the
/// project has a key, and then update checks are not offered.
const PUBLIC_KEY: &str = include_str!("../update-key.pub");

/// The built-in public key, if this build has one.
pub fn public_key() -> Option<[u8; 32]> {
    decode_key(PUBLIC_KEY)
}

fn decode_key(text: &str) -> Option<[u8; 32]> {
    let bytes = Base64::decode_vec(text.trim()).ok()?;
    bytes.try_into().ok()
}

/// A release version: major.minor.patch, with an optional pre-release
/// such as `-rc1`, which sorts before the release itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub pre: Option<String>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches('v');
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_owned())),
            Some(_) => return None,
            None => (text, None),
        };
        let mut parts = core.split('.').map(|p| {
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<u32>().ok())
                .flatten()
        });
        let version = Version {
            major: parts.next()??,
            minor: parts.next()??,
            patch: parts.next()??,
            pre,
        };
        parts.next().is_none().then_some(version)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

/// What a verified update file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub version: Version,
    /// The release date as written, `YYYY-MM-DD`.
    pub released: String,
    /// The release's page on GitHub.
    pub url: String,
}

/// Why an update file was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    TooLarge,
    /// The signature is malformed or does not match the file and key.
    BadSignature,
    /// Signed, but not an update file this version understands.
    BadFile,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::TooLarge => "the update file is too large",
            Error::BadSignature => "the update file's signature does not match",
            Error::BadFile => "the update file could not be read",
        })
    }
}

impl std::error::Error for Error {}

/// Checks `signature` (base64, as in `update.txt.sig`) over `file` with
/// `key`, and only then reads the file.
pub fn verify(file: &[u8], signature: &[u8], key: &[u8; 32]) -> Result<Update, Error> {
    if file.len() > MAX_SIZE || signature.len() > MAX_SIZE {
        return Err(Error::TooLarge);
    }
    let signature = std::str::from_utf8(signature).map_err(|_| Error::BadSignature)?;
    let signature = Base64::decode_vec(signature.trim()).map_err(|_| Error::BadSignature)?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(file, &signature)
        .map_err(|_| Error::BadSignature)?;
    parse(file).ok_or(Error::BadFile)
}

fn parse(file: &[u8]) -> Option<Update> {
    let text = std::str::from_utf8(file).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim_end() != MAGIC {
        return None;
    }
    let (mut version, mut released, mut url) = (None, None, None);
    for line in lines {
        let Some((key, value)) = line.trim_end().split_once(' ') else {
            continue;
        };
        match key {
            "version" => version = Version::parse(value),
            "released" => released = Some(value.to_owned()),
            "url" => url = Some(value.to_owned()),
            // Later versions of the file may add lines.
            _ => {}
        }
    }
    let url = url.filter(|u| safe_url(u))?;
    Some(Update {
        version: version?,
        released: released.unwrap_or_default(),
        url,
    })
}

/// A page under the project's Releases, with nothing a shell could take
/// for anything but a URL.
fn safe_url(url: &str) -> bool {
    url.starts_with(RELEASES)
        && url
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/.-_:".contains(&b))
}

/// Whether `update` is newer than the running `current` version.
pub fn is_newer(update: &Update, current: &str) -> bool {
    Version::parse(current).is_none_or(|current| update.version > current)
}

/// Whether an automatic check is due, `last` and `now` in seconds since
/// 1970. A clock that went backwards counts as due.
pub fn due(last: Option<u64>, now: u64) -> bool {
    last.is_none_or(|last| now < last || now - last >= INTERVAL_SECS)
}

/// The update file for a release, as the release workflow writes it.
pub fn file_for(version: &Version, released: &str) -> String {
    format!("{MAGIC}\nversion {version}\nreleased {released}\nurl {RELEASES}tag/v{version}\n")
}

/// A new signing key: the PKCS#8 private key (for the release workflow's
/// secret) and the public key (for `update-key.pub`), both base64.
pub fn generate() -> Result<(String, String), ring::error::Unspecified> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| ring::error::Unspecified)?;
    Ok((
        Base64::encode_string(pkcs8.as_ref()),
        Base64::encode_string(pair.public_key().as_ref()),
    ))
}

/// Signs `file` with a base64 PKCS#8 key from [`generate`], giving the
/// base64 signature for `update.txt.sig`.
pub fn sign(file: &[u8], private_key: &str) -> Result<String, ring::error::Unspecified> {
    let pkcs8 = Base64::decode_vec(private_key.trim()).map_err(|_| ring::error::Unspecified)?;
    let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| ring::error::Unspecified)?;
    Ok(Base64::encode_string(pair.sign(file).as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (String, [u8; 32]) {
        let Ok((private, public)) = generate() else {
            unreachable!("ring can make a key")
        };
        let Some(public) = decode_key(&public) else {
            unreachable!("a fresh public key decodes")
        };
        (private, public)
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| unreachable!("{text} parses"))
    }

    #[test]
    fn a_signed_file_round_trips() {
        let (private, public) = keys();
        let file = file_for(&version("1.0.1"), "2026-11-02");
        let Ok(sig) = sign(file.as_bytes(), &private) else {
            unreachable!("signing works")
        };
        let update = verify(file.as_bytes(), sig.as_bytes(), &public);
        assert_eq!(
            update,
            Ok(Update {
                version: version("1.0.1"),
                released: "2026-11-02".into(),
                url: "https://github.com/jkobierczynski/FeroSSH/releases/tag/v1.0.1".into(),
            })
        );
        // A trailing newline in the .sig file is fine.
        let sig = format!("{sig}\r\n");
        assert!(verify(file.as_bytes(), sig.as_bytes(), &public).is_ok());
    }

    #[test]
    fn anything_changed_or_unsigned_is_refused() {
        let (private, public) = keys();
        let (other, _) = keys();
        let file = file_for(&version("1.0.1"), "2026-11-02");
        let Ok(sig) = sign(file.as_bytes(), &private) else {
            unreachable!("signing works")
        };
        let changed = file.replace("1.0.1", "9.9.9");
        assert_eq!(
            verify(changed.as_bytes(), sig.as_bytes(), &public),
            Err(Error::BadSignature)
        );
        let Ok(wrong) = sign(file.as_bytes(), &other) else {
            unreachable!("signing works")
        };
        assert_eq!(
            verify(file.as_bytes(), wrong.as_bytes(), &public),
            Err(Error::BadSignature)
        );
        assert_eq!(
            verify(file.as_bytes(), b"not base64!", &public),
            Err(Error::BadSignature)
        );
        assert_eq!(
            verify(file.as_bytes(), b"", &public),
            Err(Error::BadSignature)
        );
        let big = vec![b'a'; MAX_SIZE + 1];
        assert_eq!(verify(&big, sig.as_bytes(), &public), Err(Error::TooLarge));
    }

    #[test]
    fn signed_files_must_still_make_sense() {
        let (private, public) = keys();
        for file in [
            "FeroSSH update 2\nversion 1.0.1\nurl https://github.com/jkobierczynski/FeroSSH/releases/tag/v1.0.1\n",
            "FeroSSH update 1\nversion one\nurl https://github.com/jkobierczynski/FeroSSH/releases/tag/v1.0.1\n",
            "FeroSSH update 1\nversion 1.0.1\nurl https://evil.example/releases/tag/v1.0.1\n",
            "FeroSSH update 1\nversion 1.0.1\nurl https://github.com/jkobierczynski/FeroSSH/releases/tag/v1\"&calc\n",
            "FeroSSH update 1\nversion 1.0.1\n",
        ] {
            let Ok(sig) = sign(file.as_bytes(), &private) else {
                unreachable!("signing works")
            };
            assert_eq!(
                verify(file.as_bytes(), sig.as_bytes(), &public),
                Err(Error::BadFile),
                "{file}"
            );
        }
        // Unknown lines are skipped.
        let file = "FeroSSH update 1\nversion 1.2.0\nnotes later\nurl https://github.com/jkobierczynski/FeroSSH/releases/tag/v1.2.0\n";
        let Ok(sig) = sign(file.as_bytes(), &private) else {
            unreachable!("signing works")
        };
        assert!(verify(file.as_bytes(), sig.as_bytes(), &public).is_ok());
    }

    #[test]
    fn versions_compare_like_releases() {
        assert!(version("1.0.1") > version("1.0.0"));
        assert!(version("1.10.0") > version("1.9.9"));
        assert!(version("2.0.0") > version("1.99.99"));
        assert!(version("1.0.0") > version("1.0.0-rc1"));
        assert!(version("1.0.0-rc2") > version("1.0.0-rc1"));
        assert_eq!(version("v1.2.3"), version("1.2.3"));
        for bad in [
            "", "1", "1.2", "1.2.3.4", "1.2.x", "1.-2.3", "1.2.3-", "+1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        let update = |v: &str| Update {
            version: version(v),
            released: String::new(),
            url: String::new(),
        };
        assert!(is_newer(&update("1.0.1"), "1.0.0"));
        assert!(!is_newer(&update("1.0.0"), "1.0.0"));
        assert!(!is_newer(&update("0.9.0"), "1.0.0"));
        assert!(is_newer(&update("1.0.0"), "1.0.0-rc1"));
    }

    #[test]
    fn checks_are_weekly() {
        assert!(due(None, 1_000));
        assert!(!due(Some(1_000), 1_000 + INTERVAL_SECS - 1));
        assert!(due(Some(1_000), 1_000 + INTERVAL_SECS));
        assert!(due(Some(5_000), 1_000));
    }

    #[test]
    fn the_built_in_key_is_absent_or_valid() {
        assert_eq!(public_key().is_some(), !PUBLIC_KEY.trim().is_empty());
    }
}
