//! OpenSSH `known_hosts`: lookup, hashed entries, adding and replacing keys.
//!
//! FeroSSH shares the file with Windows OpenSSH (`~/.ssh/known_hosts`), so a
//! host trusted in one is trusted in the other. New entries are written with
//! hashed hostnames (`|1|salt|hmac`), as `HashKnownHosts yes` does.
//! `@cert-authority` lines name the CAs trusted to sign host certificates
//! for the hosts they match; `@revoked` keys (host keys or CAs) are always
//! refused.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use base64ct::{Base64, Encoding};
use hmac::{KeyInit, Mac};

type HmacSha1 = hmac::Hmac<sha1::Sha1>;

/// Length of the random salt in a hashed hostname, as OpenSSH uses.
const SALT_LEN: usize = 20;

/// What known_hosts says about a host's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The key is listed for this host.
    Trusted,
    /// The host is not listed with a key of this type. `other_types` names
    /// the key types it is listed with, if any.
    Unknown { other_types: Vec<String> },
    /// The host is listed with a different key of the same type: a possible
    /// man-in-the-middle. `lines` are the 1-based lines holding the old key.
    Changed { lines: Vec<usize> },
    /// The key is marked `@revoked`.
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    CertAuthority,
    Revoked,
}

#[derive(Debug, Clone)]
enum Hosts {
    Hashed { salt: Vec<u8>, hash: Vec<u8> },
    Patterns(String),
}

#[derive(Debug, Clone)]
struct Entry {
    line: usize,
    marker: Option<Marker>,
    hosts: Hosts,
    /// The key in SSH wire format.
    key: Vec<u8>,
}

/// A parsed known_hosts file. Lines that do not parse are skipped, as
/// OpenSSH skips them, and kept untouched when the file is rewritten.
#[derive(Debug, Clone, Default)]
pub struct KnownHosts {
    entries: Vec<Entry>,
}

/// The name a host is stored under: `host`, or `[host]:port` off port 22.
pub fn host_token(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

/// The key type (first string) of an SSH wire-format public key.
pub fn key_type(blob: &[u8]) -> Option<&str> {
    let (len, rest) = blob.split_first_chunk::<4>()?;
    let len = u32::from_be_bytes(*len) as usize;
    std::str::from_utf8(rest.get(..len)?).ok()
}

impl KnownHosts {
    pub fn parse(text: &str) -> Self {
        let entries = text
            .lines()
            .enumerate()
            .filter_map(|(i, line)| parse_line(i + 1, line))
            .collect();
        Self { entries }
    }

    /// Reads `path`; a missing file is an empty list. Any other read
    /// error is returned: an unreadable file must never pass for an empty
    /// one, or every pinned key would turn into an unknown one. Text that
    /// is not valid UTF-8 is read as well as it can be (see [`decode`]).
    pub fn load(path: &Path) -> io::Result<Self> {
        Ok(Self::parse(&read(path)?))
    }

    fn for_host<'a>(&'a self, token: &'a str) -> impl Iterator<Item = &'a Entry> + 'a {
        self.entries.iter().filter(move |e| {
            e.marker != Some(Marker::CertAuthority) && host_matches(&e.hosts, token)
        })
    }

    /// Checks `key` (SSH wire format) for `host` on `port`.
    pub fn check(&self, host: &str, port: u16, key: &[u8]) -> Status {
        let token = host_token(host, port);
        // A revoked key is refused whatever host it is presented by.
        if self
            .entries
            .iter()
            .any(|e| e.marker == Some(Marker::Revoked) && e.key == key)
        {
            return Status::Revoked;
        }
        let listed: Vec<&Entry> = self
            .for_host(&token)
            .filter(|e| e.marker.is_none())
            .collect();
        if listed.iter().any(|e| e.key == key) {
            return Status::Trusted;
        }
        let kind = key_type(key);
        let changed: Vec<usize> = listed
            .iter()
            .filter(|e| key_type(&e.key) == kind)
            .map(|e| e.line)
            .collect();
        if !changed.is_empty() {
            return Status::Changed { lines: changed };
        }
        let mut other_types: Vec<String> = listed
            .iter()
            .filter_map(|e| key_type(&e.key).map(str::to_owned))
            .collect();
        other_types.dedup();
        Status::Unknown { other_types }
    }

    /// Whether `key` (a host key or a CA) is marked `@revoked`.
    pub fn is_revoked(&self, key: &[u8]) -> bool {
        self.entries
            .iter()
            .any(|e| e.marker == Some(Marker::Revoked) && e.key == key)
    }

    fn authorities<'a>(&'a self, token: &'a str) -> impl Iterator<Item = &'a Entry> + 'a {
        self.entries.iter().filter(move |e| {
            e.marker == Some(Marker::CertAuthority) && host_matches(&e.hosts, token)
        })
    }

    /// Whether an `@cert-authority` line matches `host` on `port`, so the
    /// worker should ask for a host certificate.
    pub fn has_authority(&self, host: &str, port: u16) -> bool {
        self.authorities(&host_token(host, port)).next().is_some()
    }

    /// Whether `ca` (SSH wire format) is trusted to sign host certificates
    /// for `host` on `port`.
    pub fn is_authority_for(&self, host: &str, port: u16, ca: &[u8]) -> bool {
        self.authorities(&host_token(host, port))
            .any(|e| e.key == ca)
    }

    /// The key types listed for a host, so the worker can ask the server
    /// for one of those first and not trigger a needless prompt.
    pub fn key_types(&self, host: &str, port: u16) -> Vec<String> {
        let token = host_token(host, port);
        let mut types: Vec<String> = Vec::new();
        for e in self.for_host(&token).filter(|e| e.marker.is_none()) {
            if let Some(t) = key_type(&e.key)
                && !types.iter().any(|x| x == t)
            {
                types.push(t.to_owned());
            }
        }
        types
    }
}

fn parse_line(line_no: usize, line: &str) -> Option<Entry> {
    let mut fields = line.split_ascii_whitespace();
    let mut first = fields.next()?;
    if first.starts_with('#') {
        return None;
    }
    let marker = match first {
        "@cert-authority" => Some(Marker::CertAuthority),
        "@revoked" => Some(Marker::Revoked),
        m if m.starts_with('@') => return None,
        _ => None,
    };
    if marker.is_some() {
        first = fields.next()?;
    }
    let hosts = if let Some(hashed) = first.strip_prefix("|1|") {
        let (salt, hash) = hashed.split_once('|')?;
        Hosts::Hashed {
            salt: Base64::decode_vec(salt).ok()?,
            hash: Base64::decode_vec(hash).ok()?,
        }
    } else {
        Hosts::Patterns(first.to_owned())
    };
    let alg = fields.next()?;
    let key = Base64::decode_vec(fields.next()?).ok()?;
    // The type inside the blob must agree with the type in the line.
    if key_type(&key) != Some(alg) {
        return None;
    }
    Some(Entry {
        line: line_no,
        marker,
        hosts,
        key,
    })
}

fn hash_host(salt: &[u8], token: &str) -> Option<Vec<u8>> {
    let mut mac = HmacSha1::new_from_slice(salt).ok()?;
    mac.update(token.as_bytes());
    Some(mac.finalize().into_bytes().to_vec())
}

fn host_matches(hosts: &Hosts, token: &str) -> bool {
    match hosts {
        Hosts::Hashed { salt, hash } => {
            let Ok(mut mac) = HmacSha1::new_from_slice(salt) else {
                return false;
            };
            mac.update(token.as_bytes());
            mac.verify_slice(hash).is_ok()
        }
        Hosts::Patterns(list) => match_pattern_list(list, token),
    }
}

/// OpenSSH's comma-separated pattern list: any positive match without a
/// matching `!` negation.
pub(crate) fn match_pattern_list(list: &str, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let mut matched = false;
    for pattern in list.split(',') {
        let pattern = pattern.to_ascii_lowercase();
        if let Some(negated) = pattern.strip_prefix('!') {
            if wildcard(negated.as_bytes(), name.as_bytes()) {
                return false;
            }
        } else if wildcard(pattern.as_bytes(), name.as_bytes()) {
            matched = true;
        }
    }
    matched
}

/// `*` and `?` wildcards, iteratively so a hostile pattern cannot recurse
/// deeply.
fn wildcard(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == b'?' || Some(&c) == text.get(t) => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern
        .get(p..)
        .is_some_and(|rest| rest.iter().all(|&c| c == b'*'))
}

/// A known_hosts line for `key` on `host`/`port` with a hashed hostname.
pub fn hashed_line(host: &str, port: u16, key: &[u8]) -> io::Result<String> {
    let alg = key_type(key).ok_or_else(|| io::Error::other("malformed host key"))?;
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|e| io::Error::other(e.to_string()))?;
    let hash = hash_host(&salt, &host_token(host, port))
        .ok_or_else(|| io::Error::other("hmac key rejected"))?;
    Ok(format!(
        "|1|{}|{} {alg} {}",
        Base64::encode_string(&salt),
        Base64::encode_string(&hash),
        Base64::encode_string(key)
    ))
}

/// Appends a trusted key for a host, creating the file (and `.ssh`) if
/// needed.
pub fn add(path: &Path, host: &str, port: u16, key: &[u8]) -> io::Result<()> {
    let line = hashed_line(host, port, key)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let existing = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if existing.last().is_some_and(|&b| b != b'\n') {
        file.write_all(b"\n")?;
    }
    writeln!(file, "{line}")
}

/// Removes every plain entry for a host (all key types) and adds `key`, as
/// `ssh-keygen -R` followed by a fresh accept would. The file is rewritten
/// through a temporary file so a crash never leaves it half written.
pub fn replace(path: &Path, host: &str, port: u16, key: &[u8]) -> io::Result<()> {
    let text = read(path)?;
    let token = host_token(host, port);
    let mut out = String::with_capacity(text.len() + 128);
    for (i, line) in text.lines().enumerate() {
        let drop = parse_line(i + 1, line)
            .is_some_and(|e| e.marker.is_none() && host_matches(&e.hosts, &token));
        if !drop {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(&hashed_line(host, port, key)?);
    out.push('\n');
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("ferossh-tmp");
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)
}

/// The text of the known_hosts file at `path`; empty when it does not
/// exist. Other read errors are returned.
pub fn read(path: &Path) -> io::Result<String> {
    match fs::read(path) {
        Ok(bytes) => decode(&bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// known_hosts text from raw bytes. A UTF-8 file is read as it is; a
/// UTF-16LE part (what Windows PowerShell 5.1's `>` writes, or `>>`
/// appends to a UTF-8 file) is decoded as UTF-16, from the start of the
/// line holding its first zero byte; byte order marks are dropped.
///
/// A line that is not a comment and does not decode cleanly is an error
/// naming that line, not a line to skip: skipping it could drop a
/// `@revoked` marker or a `!host` exclusion and so trust more than the
/// file says. Comment lines may hold anything.
pub fn decode(bytes: &[u8]) -> io::Result<String> {
    let (utf8, utf16) = match bytes.iter().position(|&b| b == 0) {
        None => (bytes, None),
        Some(nul) => {
            // The UTF-16 part starts after the last line end before its
            // first code unit (a `\n` there may itself be UTF-16).
            let first = nul.saturating_sub(1);
            let start = bytes
                .get(..first)
                .and_then(|b| b.iter().rposition(|&b| b == b'\n'))
                .map_or(0, |i| i + 1);
            let (head, tail) = bytes.split_at(start);
            (head, Some(tail))
        }
    };
    let utf8 = utf8.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(utf8);
    let mut text = String::from_utf8_lossy(utf8).into_owned();
    if let Some(tail) = utf16 {
        let tail = tail.strip_prefix(&[0xFF, 0xFE][..]).unwrap_or(tail);
        let (pairs, odd) = (tail.chunks_exact(2), tail.len() % 2 == 1);
        let units = pairs.map(|p| <[u8; 2]>::try_from(p).map_or(0xFFFD, u16::from_le_bytes));
        text.extend(
            char::decode_utf16(units)
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .filter(|&c| c != '\u{feff}'),
        );
        if odd {
            text.push(char::REPLACEMENT_CHARACTER);
        }
    }
    for (n, line) in text.lines().enumerate() {
        let entry = line.trim_start();
        if !entry.starts_with('#') && entry.contains(char::REPLACEMENT_CHARACTER) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "line {} is not valid text; fix or remove it so no entry is lost",
                    n + 1
                ),
            ));
        }
    }
    Ok(text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn blob(kind: &str, body: &[u8]) -> Vec<u8> {
        let mut b = (kind.len() as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind.as_bytes());
        b.extend_from_slice(body);
        b
    }

    fn line(hosts: &str, key: &[u8]) -> String {
        format!(
            "{hosts} {} {}",
            key_type(key).unwrap(),
            Base64::encode_string(key)
        )
    }

    #[test]
    fn cert_authorities() {
        let ca = blob("ssh-ed25519", b"ca");
        let other = blob("ssh-ed25519", b"other-ca");
        let text = format!(
            "@cert-authority {}\n@cert-authority {}\n@revoked {}\n",
            line("*.example.org,!bad.example.org", &ca),
            line("[*.lab]:2222", &other),
            line("*", &blob("ssh-ed25519", b"gone"))
        );
        let kh = KnownHosts::parse(&text);
        assert!(kh.has_authority("web.example.org", 22));
        assert!(kh.is_authority_for("web.example.org", 22, &ca));
        assert!(!kh.is_authority_for("web.example.org", 22, &other));
        assert!(!kh.has_authority("bad.example.org", 22));
        assert!(!kh.has_authority("example.com", 22));
        assert!(kh.is_authority_for("sw1.lab", 2222, &other));
        assert!(!kh.is_authority_for("sw1.lab", 22, &other));
        assert!(kh.is_revoked(&blob("ssh-ed25519", b"gone")));
        assert!(!kh.is_revoked(&ca));
        // A CA line never trusts the CA key as a plain host key.
        assert_eq!(
            kh.check("web.example.org", 22, &ca),
            Status::Unknown {
                other_types: vec![]
            }
        );
    }

    #[test]
    fn plain_entries_and_ports() {
        let ed = blob("ssh-ed25519", b"one");
        let text = format!(
            "# comment\n{}\n{}\n",
            line("example.org,10.0.0.1", &ed),
            line("[example.org]:2222", &blob("ssh-ed25519", b"two"))
        );
        let kh = KnownHosts::parse(&text);
        assert_eq!(kh.check("Example.ORG", 22, &ed), Status::Trusted);
        assert_eq!(kh.check("10.0.0.1", 22, &ed), Status::Trusted);
        assert_eq!(
            kh.check("example.org", 2222, &ed),
            Status::Changed { lines: vec![3] }
        );
        assert_eq!(
            kh.check("other.org", 22, &ed),
            Status::Unknown {
                other_types: vec![]
            }
        );
    }

    #[test]
    fn other_key_type_is_unknown_not_changed() {
        let rsa = blob("ssh-rsa", b"r");
        let kh = KnownHosts::parse(&line("h", &rsa));
        assert_eq!(
            kh.check("h", 22, &blob("ssh-ed25519", b"e")),
            Status::Unknown {
                other_types: vec!["ssh-rsa".into()]
            }
        );
        assert_eq!(kh.key_types("h", 22), vec!["ssh-rsa".to_owned()]);
    }

    #[test]
    fn hashed_entries_match_only_their_host() {
        let ed = blob("ssh-ed25519", b"k");
        let kh = KnownHosts::parse(&hashed_line("srv.example", 2200, &ed).unwrap());
        assert_eq!(kh.check("srv.example", 2200, &ed), Status::Trusted);
        assert!(matches!(
            kh.check("srv.example", 22, &ed),
            Status::Unknown { .. }
        ));
    }

    #[test]
    fn known_openssh_hash() {
        // Written by `ssh-keygen -H` for "localhost".
        let text = "|1|YD9f6KzGQ8c/6IVZ0tlLk5nfZd0=|2oBijcfcdnHrV9FvsSyIygngo9A= \
                    ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHZvbmU=";
        let key = Base64::decode_vec("AAAAC3NzaC1lZDI1NTE5AAAAIHZvbmU=").unwrap();
        let kh = KnownHosts::parse(text);
        assert_eq!(kh.check("localhost", 22, &key), Status::Trusted);
        assert!(matches!(
            kh.check("localhost2", 22, &key),
            Status::Unknown { .. }
        ));
    }

    #[test]
    fn patterns_and_negation() {
        assert!(match_pattern_list("*.example.org", "a.example.org"));
        assert!(!match_pattern_list("*.example.org", "example.org"));
        assert!(match_pattern_list("10.0.?.1", "10.0.5.1"));
        assert!(!match_pattern_list(
            "*.example.org,!bad.example.org",
            "bad.example.org"
        ));
        assert!(match_pattern_list("a*b*c", "aXXbYYc"));
        assert!(!match_pattern_list("a*b*c", "aXXbYY"));
    }

    #[test]
    fn revoked_wins_and_cert_authority_is_ignored() {
        let ed = blob("ssh-ed25519", b"k");
        let text = format!("{}\n@revoked {}\n", line("h", &ed), line("*", &ed));
        assert_eq!(
            KnownHosts::parse(&text).check("h", 22, &ed),
            Status::Revoked
        );
        let ca = format!("@cert-authority {}\n", line("h", &ed));
        assert!(matches!(
            KnownHosts::parse(&ca).check("h", 22, &ed),
            Status::Unknown { .. }
        ));
    }

    #[test]
    fn mismatched_type_lines_are_skipped() {
        let ed = blob("ssh-ed25519", b"k");
        let text = format!("h ssh-rsa {}\n", Base64::encode_string(&ed));
        assert!(matches!(
            KnownHosts::parse(&text).check("h", 22, &ed),
            Status::Unknown { .. }
        ));
    }

    #[test]
    fn add_and_replace_rewrite_the_file() {
        let dir = std::env::temp_dir().join(format!("ferossh-kh-{}", std::process::id()));
        let path = dir.join("known_hosts");
        let _ = fs::remove_dir_all(&dir);
        let old = blob("ssh-ed25519", b"old");
        let new = blob("ssh-ed25519", b"new");
        let keep = blob("ssh-ed25519", b"keep");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, format!("# mine\n{}", line("other", &keep))).unwrap();
        add(&path, "h", 22, &old).unwrap();
        let kh = KnownHosts::load(&path).unwrap();
        assert_eq!(kh.check("h", 22, &old), Status::Trusted);
        assert_eq!(kh.check("other", 22, &keep), Status::Trusted);

        replace(&path, "h", 22, &new).unwrap();
        let kh = KnownHosts::load(&path).unwrap();
        assert_eq!(kh.check("h", 22, &new), Status::Trusted);
        assert!(matches!(kh.check("h", 22, &old), Status::Changed { .. }));
        assert_eq!(kh.check("other", 22, &keep), Status::Trusted);
        assert!(fs::read_to_string(&path).unwrap().starts_with("# mine\n"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn utf16_and_bad_bytes_keep_the_other_entries() {
        let a = blob("ssh-ed25519", b"a");
        let b = blob("ssh-ed25519", b"b");
        let c = blob("ssh-ed25519", b"c");
        // A UTF-8 file, a Latin-1 comment, then a line appended by
        // PowerShell 5.1's `>>` (UTF-16LE with a byte order mark).
        let mut bytes = format!("{}\n# caf\u{e9}\n", line("a", &a)).into_bytes();
        bytes.extend_from_slice(b"# caf\xe9 latin-1\n");
        bytes.extend_from_slice(&[0xFF, 0xFE]);
        for u in format!("{}\r\n", line("b", &b)).encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let kh = KnownHosts::parse(&decode(&bytes).unwrap());
        assert_eq!(kh.check("a", 22, &a), Status::Trusted);
        assert_eq!(kh.check("b", 22, &b), Status::Trusted);
        assert!(matches!(kh.check("a", 22, &c), Status::Changed { .. }));
        // A whole UTF-8 file with a byte order mark.
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(line("c", &c).as_bytes());
        assert_eq!(
            KnownHosts::parse(&decode(&bom).unwrap()).check("c", 22, &c),
            Status::Trusted
        );
    }

    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn utf16_characters_never_become_line_ends() {
        // U+200A is 0A 20 in UTF-16LE: dropping zero bytes would turn
        // it into a new line and wake the commented-out key.
        let a = blob("ssh-ed25519", b"a");
        let b = blob("ssh-ed25519", b"b");
        let text = format!("# \u{200A}{}\r\n{}\r\n", line("a", &a), line("b", &b));
        let kh = KnownHosts::parse(&decode(&utf16(&text)).unwrap());
        assert!(matches!(kh.check("a", 22, &a), Status::Unknown { .. }));
        assert_eq!(kh.check("b", 22, &b), Status::Trusted);
        // The same after a UTF-8 part, without a byte order mark.
        let mut bytes = format!("{}\n", line("b", &b)).into_bytes();
        bytes.extend_from_slice(utf16(&text).get(2..).unwrap());
        let kh = KnownHosts::parse(&decode(&bytes).unwrap());
        assert!(matches!(kh.check("a", 22, &a), Status::Unknown { .. }));
        assert_eq!(kh.check("b", 22, &b), Status::Trusted);
    }

    #[test]
    fn a_damaged_entry_line_is_an_error_not_skipped() {
        let ca = blob("ssh-ed25519", b"ca");
        let mut bytes = b"@cert-authority *.example.org,!\xFFprod.example.org ".to_vec();
        bytes.extend_from_slice(line("x", &ca).split_once(' ').unwrap().1.as_bytes());
        let e = decode(&bytes).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("line 1"), "{e}");
        // An unpaired surrogate on a UTF-16 entry line, and an odd byte.
        let mut bad = utf16("host ssh-ed25519 AAAA\r\n");
        bad.splice(4..4, [0x00, 0xD8]);
        assert!(decode(&bad).is_err());
        let mut odd = utf16("# fine\r\n");
        odd.push(b'h');
        assert!(decode(&odd).is_err());
        // Anything goes in a comment.
        assert!(decode(b"# caf\xe9\n").is_ok());
    }

    #[test]
    fn an_unreadable_file_is_an_error_not_an_empty_list() {
        let dir = std::env::temp_dir().join(format!("ferossh-kh-err-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Missing: empty. A folder where the file should be: an error.
        assert!(
            KnownHosts::load(&dir.join("missing"))
                .unwrap()
                .entries
                .is_empty()
        );
        assert!(KnownHosts::load(&dir).is_err());
        assert!(read(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
