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
    /// The host is not listed.
    Unknown,
    /// The host is listed with a different key of the same type: a possible
    /// man-in-the-middle. `lines` are the 1-based lines holding the old key.
    Changed { lines: Vec<usize> },
    /// The host is listed only with keys of other types (`types`, on
    /// `lines`). A server offers the type already trusted first, so a
    /// switch of type is as suspect as a changed key: a man-in-the-middle
    /// with a key of another type, or a server that lost the old one.
    TypeChanged {
        types: Vec<String>,
        lines: Vec<usize>,
    },
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
        if listed.is_empty() {
            return Status::Unknown;
        }
        let mut types: Vec<String> = listed
            .iter()
            .filter_map(|e| key_type(&e.key).map(str::to_owned))
            .collect();
        types.sort();
        types.dedup();
        Status::TypeChanged {
            types,
            lines: listed.iter().map(|e| e.line).collect(),
        }
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
    if existing.contains(&0) {
        // UTF-16 (PowerShell 5.1's `>`): UTF-8 appended to it would not
        // read back, so the whole file is rewritten as UTF-8 first.
        let mut text = decode(&existing)?;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&line);
        text.push('\n');
        return replace_file(path, text.as_bytes());
    }
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
    rewrite(path, host, port, Some(key))
}

/// Removes every plain entry for a host, as `ssh-keygen -R` does, from
/// a file other than the one the new key goes to. A missing file is
/// left missing.
pub fn forget(path: &Path, host: &str, port: u16) -> io::Result<()> {
    rewrite(path, host, port, None)
}

fn rewrite(path: &Path, host: &str, port: u16, key: Option<&[u8]>) -> io::Result<()> {
    let text = read(path)?;
    let token = host_token(host, port);
    let mut out = String::with_capacity(text.len() + 128);
    let mut changed = false;
    for (i, line) in text.lines().enumerate() {
        let entry = parse_line(i + 1, line)
            .filter(|e| e.marker.is_none() && host_matches(&e.hosts, &token));
        changed |= entry.is_some();
        match entry.map(|e| e.hosts) {
            None => out.push_str(line),
            // One host per hashed line: it goes.
            Some(Hosts::Hashed { .. }) => continue,
            Some(Hosts::Patterns(list)) => match without_host(&list, &token) {
                Some(rest) => {
                    // The other hosts keep their key: only the list changes.
                    let fields = line.trim_start();
                    let tail = fields.get(list.len()..).unwrap_or_default();
                    out.push_str(&rest);
                    out.push_str(tail);
                }
                None => continue,
            },
        }
        out.push('\n');
    }
    // Nothing to forget here: leave the file (and its folder) untouched.
    if key.is_none() && !changed {
        return Ok(());
    }
    if let Some(key) = key {
        out.push_str(&hashed_line(host, port, key)?);
        out.push('\n');
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    replace_file(path, out.as_bytes())
}

/// What UpdateHostKeys changes for one host in one known_hosts file: the
/// server's own list of its host keys (`hostkeys-00@openssh.com`) set
/// against the keys the file holds for the host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostKeyChanges {
    /// Keys the server has that the file lacks. They are written only
    /// once the server proved it holds them.
    pub add: Vec<Vec<u8>>,
    /// Keys the file holds for the host that the server no longer has.
    pub remove: Vec<Vec<u8>>,
}

impl HostKeyChanges {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty()
    }
}

/// What UpdateHostKeys would change in known_hosts `text` for `host` on
/// `port`, which logged in with key `used` and then listed `announced`;
/// `Err` says why the file is left alone, as OpenSSH leaves it:
///
/// - the server did not list the key it logged in with;
/// - the file does not hold that key for the host (it was trusted some
///   other way: a certificate, DNS, another file);
/// - a line holding a key for the host also names other hosts or
///   patterns, so changing it would change their trust too.
///
/// Keys marked `@revoked` are never added. Hashed lines name one host
/// each and may be changed.
pub fn plan_host_keys(
    text: &str,
    host: &str,
    port: u16,
    used: &[u8],
    announced: &[Vec<u8>],
) -> Result<HostKeyChanges, String> {
    plan_lines(text, host, port, used, announced).map(|(changes, _)| changes)
}

/// [`plan_host_keys`], with the 1-based numbers of the lines that go.
fn plan_lines(
    text: &str,
    host: &str,
    port: u16,
    used: &[u8],
    announced: &[Vec<u8>],
) -> Result<(HostKeyChanges, Vec<usize>), String> {
    let token = host_token(host, port);
    if !announced.iter().any(|k| k == used) {
        return Err("the server did not list the key it logged in with".into());
    }
    let entries: Vec<Entry> = text
        .lines()
        .enumerate()
        .filter_map(|(i, line)| parse_line(i + 1, line))
        .collect();
    let revoked = |key: &[u8]| {
        entries
            .iter()
            .any(|e| e.marker == Some(Marker::Revoked) && e.key == key)
    };
    let mut listed: Vec<&Entry> = Vec::new();
    for e in &entries {
        if e.marker.is_some() || !host_matches(&e.hosts, &token) {
            continue;
        }
        if let Hosts::Patterns(list) = &e.hosts
            && !list.eq_ignore_ascii_case(&token)
        {
            return Err(format!(
                "line {} also names other hosts or patterns ({list})",
                e.line
            ));
        }
        listed.push(e);
    }
    if !listed.iter().any(|e| e.key == used) {
        return Err("the file does not hold the key the host logged in with".into());
    }
    let mut changes = HostKeyChanges::default();
    for key in announced {
        if !revoked(key) && !listed.iter().any(|e| &e.key == key) && !changes.add.contains(key) {
            changes.add.push(key.clone());
        }
    }
    let mut lines = Vec::new();
    for e in &listed {
        if !announced.contains(&e.key) {
            lines.push(e.line);
            if !changes.remove.contains(&e.key) {
                changes.remove.push(e.key.clone());
            }
        }
    }
    Ok((changes, lines))
}

/// Carries out UpdateHostKeys in the known_hosts file at `path`: the
/// lines holding keys the server no longer has go, and each key in
/// `proven` the file lacks is added with a hashed hostname. The changes
/// are worked out again from the file as it is now ([`plan_host_keys`]);
/// nothing is written when that would add a key the server has not
/// proven (the file changed meanwhile), or when the file changes while it
/// is written. Kept lines stay byte for byte. Returns what changed.
pub fn update_host_keys(
    path: &Path,
    host: &str,
    port: u16,
    used: &[u8],
    announced: &[Vec<u8>],
    proven: &[Vec<u8>],
) -> io::Result<HostKeyChanges> {
    let raw = fs::read(path)?;
    let text = decode(&raw)?;
    let (changes, gone) =
        plan_lines(&text, host, port, used, announced).map_err(io::Error::other)?;
    if changes.add.iter().any(|k| !proven.contains(k)) {
        return Err(io::Error::other(format!(
            "{} changed while the server proved its keys; nothing was written",
            path.display()
        )));
    }
    if changes.is_empty() {
        return Ok(changes);
    }
    let utf16 = raw.contains(&0);
    if utf16 && text.contains('\u{FFFD}') {
        return Err(io::Error::other(format!(
            "{} mixes UTF-16 with bytes that are not text; nothing was written",
            path.display()
        )));
    }
    let parts: Vec<&[u8]> = if utf16 {
        text.split_inclusive('\n').map(str::as_bytes).collect()
    } else {
        raw.split_inclusive(|&b| b == b'\n').collect()
    };
    let eol: &[u8] = if raw.windows(2).any(|w| w == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut out: Vec<u8> = Vec::with_capacity(raw.len() + 256 * changes.add.len());
    for (i, line) in parts.into_iter().enumerate() {
        if !gone.contains(&(i + 1)) {
            out.extend_from_slice(line);
        }
    }
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.extend_from_slice(eol);
    }
    for key in &changes.add {
        out.extend_from_slice(hashed_line(host, port, key)?.as_bytes());
        out.extend_from_slice(eol);
    }
    write_over(path, &out, Some(&raw))?;
    Ok(changes)
}

/// What a known_hosts line is, as the known hosts manager lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A trusted host key.
    HostKey,
    /// `@cert-authority`: a CA trusted to sign host certificates.
    CertAuthority,
    /// `@revoked`: a key that is always refused.
    Revoked,
    /// A line OpenSSH and FeroSSH skip: an unknown `@` marker, a key that
    /// does not decode, or one whose type disagrees with the line's.
    Unreadable,
}

impl LineKind {
    pub fn label(self) -> &'static str {
        match self {
            LineKind::HostKey => "Host key",
            LineKind::CertAuthority => "CA",
            LineKind::Revoked => "Revoked",
            LineKind::Unreadable => "Unreadable",
        }
    }
}

/// One line of a known_hosts file that is not blank or a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// 1-based.
    pub line: usize,
    /// The line as it is in the file, so a removal can check it is still
    /// there.
    pub text: String,
    pub kind: LineKind,
    /// The host patterns as written; `None` for a hashed name
    /// (`|1|salt|hash`), which only a host name typed in can match.
    pub hosts: Option<String>,
    /// The key type the line names (`ssh-ed25519`, ...).
    pub key_type: String,
    /// The key in SSH wire format; empty for an unreadable line.
    pub key: Vec<u8>,
    /// Whatever follows the key.
    pub comment: String,
    hashed: Option<(Vec<u8>, Vec<u8>)>,
}

impl Listing {
    /// Whether the line is for `host` on `port`, the way a connection
    /// looks it up: a hashed name is checked against the name, a pattern
    /// list matched with its wildcards and `!` negations.
    pub fn matches_host(&self, host: &str, port: u16) -> bool {
        let token = host_token(host, port);
        match (&self.hashed, &self.hosts) {
            (Some((salt, hash)), _) => host_matches(
                &Hosts::Hashed {
                    salt: salt.clone(),
                    hash: hash.clone(),
                },
                &token,
            ),
            (None, Some(list)) => match_pattern_list(list, &token),
            (None, None) => false,
        }
    }
}

/// Every line of known_hosts text that is not blank or a comment, in
/// order, unreadable ones included so they can be found and removed.
pub fn list(text: &str) -> Vec<Listing> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| list_line(i + 1, line))
        .collect()
}

fn list_line(line_no: usize, line: &str) -> Option<Listing> {
    let mut fields = line.split_ascii_whitespace();
    let first = fields.next()?;
    if first.starts_with('#') {
        return None;
    }
    let mut unknown_marker = false;
    let host_field = match first {
        "@cert-authority" | "@revoked" => fields.next(),
        m if m.starts_with('@') => {
            unknown_marker = true;
            fields.next()
        }
        _ => Some(first),
    };
    let key_type = fields.next().unwrap_or_default().to_owned();
    let _key_field = fields.next();
    let comment = fields.collect::<Vec<_>>().join(" ");
    let parsed = (!unknown_marker)
        .then(|| parse_line(line_no, line))
        .flatten();
    let hosts_text = host_field.unwrap_or_default();
    let hashed = match parsed.as_ref().map(|e| &e.hosts) {
        Some(Hosts::Hashed { salt, hash }) => Some((salt.clone(), hash.clone())),
        _ => None,
    };
    let hosts = if hashed.is_some() || hosts_text.starts_with("|1|") {
        None
    } else {
        Some(hosts_text.to_owned())
    };
    let kind = match parsed.as_ref().map(|e| e.marker) {
        None => LineKind::Unreadable,
        Some(None) => LineKind::HostKey,
        Some(Some(Marker::CertAuthority)) => LineKind::CertAuthority,
        Some(Some(Marker::Revoked)) => LineKind::Revoked,
    };
    Some(Listing {
        line: line_no,
        text: line.to_owned(),
        kind,
        hosts,
        key_type,
        key: parsed.map(|e| e.key).unwrap_or_default(),
        comment,
        hashed,
    })
}

/// Removes whole lines from the known_hosts file at `path`: each given
/// by its 1-based number and its text as listed. Nothing is written when
/// any of them is no longer there as listed (the file changed since):
/// the error says so, and the caller lists the file again. Returns how
/// many lines went. The file is rewritten through a temporary file.
pub fn remove_lines(path: &Path, lines: &[(usize, String)]) -> io::Result<usize> {
    if lines.is_empty() {
        return Ok(0);
    }
    let raw = fs::read(path)?;
    let text = decode(&raw)?;
    check_lines(path, &text, lines)?;
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut removed = 0;
    // Kept lines are copied byte for byte, line ends (CRLF too) and
    // bytes that are not UTF-8 in comments included. A file with UTF-16
    // in it is rewritten as UTF-8, as adding a key does.
    let utf16 = raw.contains(&0);
    if utf16 && text.contains('\u{FFFD}') {
        // Rewriting it as UTF-8 would turn bytes it could not read in a
        // comment into U+FFFD: nothing is written instead.
        return Err(io::Error::other(format!(
            "{} mixes UTF-16 with bytes that are not text; nothing was removed. \
             Save it as UTF-8 (or remove the line in an editor) and try again.",
            path.display()
        )));
    }
    let parts: Vec<&[u8]> = if utf16 {
        text.split_inclusive('\n').map(str::as_bytes).collect()
    } else {
        raw.split_inclusive(|&b| b == b'\n').collect()
    };
    for (i, line) in parts.into_iter().enumerate() {
        if lines.iter().any(|(n, _)| *n == i + 1) {
            removed += 1;
            continue;
        }
        out.extend_from_slice(line);
    }
    write_over(path, &out, Some(&raw))?;
    Ok(removed)
}

/// Checks, before anything is written, that each listed line of `path`
/// is still the text that was shown.
pub fn check_lines(path: &Path, text: &str, lines: &[(usize, String)]) -> io::Result<()> {
    let all: Vec<&str> = text.lines().collect();
    for (n, expected) in lines {
        let found = n
            .checked_sub(1)
            .and_then(|i| all.get(i))
            .is_some_and(|l| l == expected);
        if !found {
            return Err(changed_error(path, *n));
        }
    }
    Ok(())
}

fn changed_error(path: &Path, n: usize) -> io::Error {
    io::Error::other(format!(
        "{} changed since it was listed (line {n} is not what was shown). \
         The list was read again; check it and try once more.",
        path.display()
    ))
}

/// Writes `data` to `path` through a fresh temporary file next to it,
/// then renames it into place. The temporary file is created new, never
/// opened over an existing one, so it can never be another file the
/// user named (a second known_hosts called `pin.ferossh-tmp`, say).
fn replace_file(path: &Path, data: &[u8]) -> io::Result<()> {
    write_over(path, data, None)
}

/// [`replace_file`], with two more guards. A symbolic link is followed,
/// so the file it points at is rewritten and the link stays a link. When
/// `unchanged` is given, the file is read again just before the rename
/// and nothing is replaced if it no longer holds those bytes (another
/// program wrote to it meanwhile). The new file gets the old one's
/// permissions; on Unix it is created owner-only first.
fn write_over(path: &Path, data: &[u8], unchanged: Option<&[u8]>) -> io::Result<()> {
    let real = match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let path = real.as_path();
    let old_perms = fs::metadata(path).ok().map(|m| m.permissions());
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    for n in 0u32..1000 {
        let tmp = path.with_file_name(format!(".{name}.{}-{n}.ferossh-tmp", std::process::id()));
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = match opts.open(&tmp) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let written = file.write_all(data).and_then(|()| file.sync_all());
        drop(file);
        let done = written
            .and_then(|()| match &old_perms {
                Some(p) => fs::set_permissions(&tmp, p.clone()),
                None => Ok(()),
            })
            .and_then(|()| match unchanged {
                Some(before) if fs::read(path)? != before => Err(io::Error::other(format!(
                    "{} was changed by another program just now; nothing was removed. \
                     The list was read again; check it and try once more.",
                    path.display()
                ))),
                _ => Ok(()),
            })
            .and_then(|()| commit(&tmp, path, old_perms.is_some()));
        if let Err(e) = done {
            // A file that was there and is gone now (a replace that failed
            // halfway): the temporary file holds the only copy, so it stays
            // and the error names it.
            if old_perms.is_some() && fs::symlink_metadata(path).is_err() {
                return Err(io::Error::other(format!(
                    "{e}. {} is missing now; its new contents are in {}: rename that file to {} to restore it.",
                    path.display(),
                    tmp.display(),
                    name
                )));
            }
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free temporary file name next to the known_hosts file",
    ))
}

/// Puts `tmp` in `path`'s place. On Windows an existing file is replaced
/// with `ReplaceFileW`, which keeps the old file's security descriptor
/// (its ACL, which may be stricter than the folder's) and attributes; a
/// plain rename would give the new file the folder's inherited ACL.
#[cfg(windows)]
#[allow(unsafe_code)]
fn commit(tmp: &Path, path: &Path, existed: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW};
    use windows::core::PCWSTR;
    if !existed {
        return fs::rename(tmp, path);
    }
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (to, from) = (wide(path), wide(tmp));
    // SAFETY: both names are NUL-terminated and live through the call.
    unsafe {
        ReplaceFileW(
            PCWSTR(to.as_ptr()),
            PCWSTR(from.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    }
    .or_else(|e| {
        // ERROR_UNABLE_TO_MOVE_REPLACEMENT: the old file is gone but the
        // new one is still at its temporary name; move it into place
        // rather than leave no known_hosts at all.
        if e.code() == windows::core::HRESULT::from_win32(1176) {
            return fs::rename(tmp, path);
        }
        Err(io::Error::other(format!("{}: {e}", path.display())))
    })
}

#[cfg(not(windows))]
fn commit(tmp: &Path, path: &Path, _existed: bool) -> io::Result<()> {
    fs::rename(tmp, path)
}

/// A plain host list with `token` taken out, for a line that keeps the
/// other hosts' trust; `None` when nothing else is left. A name written
/// out is removed; a wildcard that still covers `token` stays, with a
/// `!token` added so it no longer does.
fn without_host(list: &str, token: &str) -> Option<String> {
    let kept: Vec<&str> = list
        .split(',')
        .filter(|p| !p.eq_ignore_ascii_case(token))
        .collect();
    if !kept.iter().any(|p| !p.starts_with('!') && !p.is_empty()) {
        return None;
    }
    let mut rest = kept.join(",");
    if match_pattern_list(&rest, token) {
        rest.push_str(",!");
        rest.push_str(token);
    }
    Some(rest)
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
        let units: Vec<u16> = pairs
            .map(|p| <[u8; 2]>::try_from(p).map_or(0xFFFD, u16::from_le_bytes))
            .collect();
        // What PowerShell writes for known_hosts is Latin text: every
        // high byte zero. Anything else means the zero byte that started
        // this part was not UTF-16 at all (a stray one in a UTF-8 file),
        // and reading on would hide the lines after it, a @revoked one
        // among them.
        if let Some(bad) = units.iter().position(|&u| u > 0xFF && u != 0xFEFF) {
            let before = units.get(..bad).unwrap_or_default();
            let line = utf8.iter().filter(|&&b| b == b'\n').count()
                + before.iter().filter(|&&u| u == 0x0A).count()
                + 1;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "line {line} is not UTF-8 or the Latin-only UTF-16 PowerShell writes; save the file as UTF-8 or fix that line so no entry is lost"
                ),
            ));
        }
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
    fn listing_shows_every_kind_of_line() {
        let host = blob("ssh-ed25519", b"host");
        let ca = blob("ssh-ed25519", b"ca");
        let gone = blob("ssh-rsa", b"gone");
        let hashed = hashed_line("secret.example", 2222, &host).unwrap();
        let text = format!(
            "# a comment\n\n{} me@pc\n@cert-authority {}\n@revoked {}\n{hashed}\n\
             @weird host ssh-ed25519 AAAA\nbad ssh-ed25519 !!!\n",
            line("a.example,10.0.0.1", &host),
            line("*.example", &ca),
            line("*", &gone),
        );
        let l = list(&text);
        assert_eq!(l.len(), 6);
        assert_eq!(l[0].line, 3);
        assert_eq!(l[0].kind, LineKind::HostKey);
        assert_eq!(l[0].hosts.as_deref(), Some("a.example,10.0.0.1"));
        assert_eq!(l[0].key_type, "ssh-ed25519");
        assert_eq!(l[0].key, host);
        assert_eq!(l[0].comment, "me@pc");
        assert!(l[0].matches_host("A.example", 22));
        assert!(!l[0].matches_host("a.example", 2200));
        assert_eq!(l[1].kind, LineKind::CertAuthority);
        assert!(l[1].matches_host("web.example", 22));
        assert_eq!(l[2].kind, LineKind::Revoked);
        assert_eq!(l[2].key_type, "ssh-rsa");
        // A hashed name shows no host; only the right name and port match.
        assert_eq!(l[3].hosts, None);
        assert!(l[3].matches_host("secret.example", 2222));
        assert!(!l[3].matches_host("secret.example", 22));
        assert!(!l[3].matches_host("other.example", 2222));
        // Lines a lookup skips are listed so they can be removed.
        assert_eq!(l[4].kind, LineKind::Unreadable);
        assert_eq!(l[4].hosts.as_deref(), Some("host"));
        assert_eq!(l[5].kind, LineKind::Unreadable);
        assert!(l[5].key.is_empty());
        // Still found by its host, to be removed.
        assert!(l[5].matches_host("bad", 22));
        // Each keeps its text for a checked removal.
        for e in &l {
            assert_eq!(text.lines().nth(e.line - 1), Some(e.text.as_str()));
        }
    }

    #[test]
    fn host_key_plan_adds_new_and_drops_old_keys() {
        let ed = blob("ssh-ed25519", b"ed");
        let ecdsa = blob("ecdsa-sha2-nistp256", b"ec");
        let old_rsa = blob("rsa-sha2-512", b"old");
        let bad = blob("ssh-ed25519", b"revoked");
        let text = format!(
            "{}\n{}\n{}\n@revoked {}\n",
            line("web", &ed),
            line("web", &old_rsa),
            line("other", &old_rsa),
            line("*", &bad)
        );
        let announced = vec![ed.clone(), ecdsa.clone(), bad.clone(), ecdsa.clone()];
        let plan = plan_host_keys(&text, "web", 22, &ed, &announced).unwrap();
        assert_eq!(plan.add, vec![ecdsa.clone()]);
        assert_eq!(plan.remove, vec![old_rsa.clone()]);
        // Nothing new, nothing gone.
        let same = plan_host_keys(&text, "web", 22, &ed, &[ed.clone(), old_rsa.clone()]).unwrap();
        assert!(same.is_empty());
        // The key used must be listed by the server and held by the file.
        assert!(plan_host_keys(&text, "web", 22, &ed, std::slice::from_ref(&ecdsa)).is_err());
        assert!(plan_host_keys(&text, "web", 22, &ecdsa, &announced).is_err());
        assert!(plan_host_keys(&text, "web", 2222, &ed, &announced).is_err());
    }

    #[test]
    fn host_key_plan_leaves_shared_lines_alone() {
        let ed = blob("ssh-ed25519", b"ed");
        let new = blob("ecdsa-sha2-nistp256", b"ec");
        for hosts in ["web,10.0.0.5", "web*", "*,!db"] {
            let text = format!("{}\n", line(hosts, &ed));
            let why = plan_host_keys(&text, "web", 22, &ed, &[ed.clone(), new.clone()]);
            assert!(why.unwrap_err().contains("other hosts"), "{hosts}");
        }
        // A hashed line names one host.
        let text = format!("{}\n", hashed_line("web", 22, &ed).unwrap());
        let plan = plan_host_keys(&text, "WEB", 22, &ed, &[ed.clone(), new.clone()]).unwrap();
        assert_eq!(plan.add, vec![new]);
        // A non-standard port is its own name.
        let text = format!("{}\n", line("[web]:2222", &ed));
        assert!(plan_host_keys(&text, "web", 2222, &ed, std::slice::from_ref(&ed)).is_ok());
    }

    #[test]
    fn host_key_update_rewrites_only_the_host() {
        let dir = std::env::temp_dir().join(format!("ferossh-kh-upd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("known_hosts");
        let ed = blob("ssh-ed25519", b"ed");
        let ecdsa = blob("ecdsa-sha2-nistp256", b"ec");
        let old = blob("rsa-sha2-512", b"old");
        let text = format!(
            "# mine\r\n{} me@pc\r\n{}\r\n{}",
            line("web", &ed),
            line("web", &old),
            line("db", &old)
        );
        fs::write(&path, &text).unwrap();
        let announced = vec![ed.clone(), ecdsa.clone()];
        // Not proven: nothing is written.
        assert!(update_host_keys(&path, "web", 22, &ed, &announced, &[]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        let done = update_host_keys(
            &path,
            "web",
            22,
            &ed,
            &announced,
            std::slice::from_ref(&ecdsa),
        )
        .unwrap();
        assert_eq!(done.add, vec![ecdsa.clone()]);
        assert_eq!(done.remove, vec![old.clone()]);
        let after = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = after.split("\r\n").collect();
        assert_eq!(lines[0], "# mine");
        assert_eq!(lines[1], format!("{} me@pc", line("web", &ed)));
        assert_eq!(lines[2], line("db", &old));
        assert!(lines[3].starts_with("|1|"));
        assert_eq!(lines[4], "");
        let kh = KnownHosts::parse(&after);
        assert_eq!(kh.check("web", 22, &ecdsa), Status::Trusted);
        assert_eq!(kh.check("web", 22, &ed), Status::Trusted);
        assert!(!matches!(kh.check("web", 22, &old), Status::Trusted));
        assert_eq!(kh.check("db", 22, &old), Status::Trusted);
        // Run again: nothing left to do, the file stays as it is.
        let again = update_host_keys(
            &path,
            "web",
            22,
            &ed,
            &announced,
            std::slice::from_ref(&ecdsa),
        )
        .unwrap();
        assert!(again.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), after);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_lines_checks_they_are_unchanged() {
        let dir = std::env::temp_dir().join(format!("ferossh-kh-rm-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("known_hosts");
        let a = blob("ssh-ed25519", b"a");
        let b = blob("ssh-ed25519", b"b");
        let text = format!(
            "# keep me\r\n{}\r\n{}\n@revoked {}\n",
            line("a", &a),
            line("b", &b),
            line("*", &a)
        );
        fs::write(&path, &text).unwrap();
        let l = list(&read(&path).unwrap());
        // A line that is not what was listed: nothing is written.
        let stale = vec![
            (l[0].line, l[0].text.clone()),
            (l[1].line, "b x y".to_owned()),
        ];
        assert!(remove_lines(&path, &stale).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        let past_end = vec![(99, l[0].text.clone())];
        assert!(remove_lines(&path, &past_end).is_err());
        assert_eq!(remove_lines(&path, &[]).unwrap(), 0);
        // The right lines go; the rest stay, comments included.
        let both = vec![
            (l[0].line, l[0].text.clone()),
            (l[2].line, l[2].text.clone()),
        ];
        assert_eq!(remove_lines(&path, &both).unwrap(), 2);
        let after = fs::read_to_string(&path).unwrap();
        // Kept lines keep their own line ends.
        assert_eq!(after, format!("# keep me\r\n{}\n", line("b", &b)));
        // ...and their bytes, even ones that are not UTF-8 in a comment.
        let mut raw = b"# caf\xe9\r\n".to_vec();
        raw.extend_from_slice(format!("{}\n", line("a", &a)).as_bytes());
        fs::write(&path, &raw).unwrap();
        let l = list(&read(&path).unwrap());
        let a_line = l.iter().find(|x| x.kind == LineKind::HostKey).unwrap();
        assert_eq!(
            remove_lines(&path, &[(a_line.line, a_line.text.clone())]).unwrap(),
            1
        );
        assert_eq!(fs::read(&path).unwrap(), b"# caf\xe9\r\n");
        // Such a byte before a UTF-16 part cannot be kept: nothing is written.
        let mut mixed = b"# caf\xe9\n".to_vec();
        for u in format!("{}\n", line("a", &a)).encode_utf16() {
            mixed.extend_from_slice(&u.to_le_bytes());
        }
        fs::write(&path, &mixed).unwrap();
        let l = list(&read(&path).unwrap());
        let a_line = l.iter().find(|x| x.kind == LineKind::HostKey).unwrap();
        assert!(remove_lines(&path, &[(a_line.line, a_line.text.clone())]).is_err());
        assert_eq!(fs::read(&path).unwrap(), mixed);
        fs::write(&path, &after).unwrap();
        let kh = KnownHosts::parse(&after);
        assert_eq!(kh.check("a", 22, &a), Status::Unknown);
        assert_eq!(kh.check("b", 22, &b), Status::Trusted);
        // No temporary file is left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn removing_lines_keeps_links_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ferossh-kh-link-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real_hosts");
        let link = dir.join("known_hosts");
        let a = blob("ssh-ed25519", b"a");
        let b = blob("ssh-ed25519", b"b");
        fs::write(&real, format!("{}\n{}\n", line("a", &a), line("b", &b))).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let l = list(&read(&link).unwrap());
        assert_eq!(
            remove_lines(&link, &[(l[0].line, l[0].text.clone())]).unwrap(),
            1
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(&real).unwrap(),
            format!("{}\n", line("b", &b))
        );
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_dir_all(&dir).unwrap();
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
        assert_eq!(kh.check("web.example.org", 22, &ca), Status::Unknown);
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
        assert_eq!(kh.check("other.org", 22, &ed), Status::Unknown);
    }

    #[test]
    fn other_key_type_is_a_type_change() {
        let rsa = blob("ssh-rsa", b"r");
        let kh = KnownHosts::parse(&format!("# mine\n{}", line("h", &rsa)));
        assert_eq!(
            kh.check("h", 22, &blob("ssh-ed25519", b"e")),
            Status::TypeChanged {
                types: vec!["ssh-rsa".into()],
                lines: vec![2]
            }
        );
        assert_eq!(kh.key_types("h", 22), vec!["ssh-rsa".to_owned()]);
    }

    #[test]
    fn hashed_entries_match_only_their_host() {
        let ed = blob("ssh-ed25519", b"k");
        let kh = KnownHosts::parse(&hashed_line("srv.example", 2200, &ed).unwrap());
        assert_eq!(kh.check("srv.example", 2200, &ed), Status::Trusted);
        assert!(matches!(kh.check("srv.example", 22, &ed), Status::Unknown));
    }

    #[test]
    fn known_openssh_hash() {
        // Written by `ssh-keygen -H` for "localhost".
        let text = "|1|YD9f6KzGQ8c/6IVZ0tlLk5nfZd0=|2oBijcfcdnHrV9FvsSyIygngo9A= \
                    ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHZvbmU=";
        let key = Base64::decode_vec("AAAAC3NzaC1lZDI1NTE5AAAAIHZvbmU=").unwrap();
        let kh = KnownHosts::parse(text);
        assert_eq!(kh.check("localhost", 22, &key), Status::Trusted);
        assert!(matches!(kh.check("localhost2", 22, &key), Status::Unknown));
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
            Status::Unknown
        ));
    }

    #[test]
    fn mismatched_type_lines_are_skipped() {
        let ed = blob("ssh-ed25519", b"k");
        let text = format!("h ssh-rsa {}\n", Base64::encode_string(&ed));
        assert!(matches!(
            KnownHosts::parse(&text).check("h", 22, &ed),
            Status::Unknown
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
    fn forgetting_a_host_a_file_does_not_name_never_writes_it() {
        let dir = std::env::temp_dir().join(format!("ferossh-kh-noop-{}", std::process::id()));
        let path = dir.join("known_hosts");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let text = format!("{}\n", line("other", &blob("ssh-rsa", b"o")));
        fs::write(&path, &text).unwrap();
        let before = fs::metadata(&path).unwrap();
        forget(&path, "h", 22).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        // Not even rewritten with the same text: a rewrite renames a new
        // file into place.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&path).unwrap().ino(), before.ino());
        }
        let _ = before;
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewriting_never_writes_over_a_file_named_like_its_temporary() {
        let dir = std::env::temp_dir().join(format!("ferossh-kh-tmpname-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pin = dir.join("pin");
        // Every name the old code (and a plain extension) would use.
        let others = [
            pin.with_extension("ferossh-tmp"),
            dir.join(".pin.ferossh-tmp"),
        ];
        let k = blob("ssh-ed25519", b"k");
        let text = format!("{}\n", line("h", &k));
        for o in &others {
            fs::write(o, &text).unwrap();
        }
        fs::write(&pin, &text).unwrap();
        forget(&pin, "h", 22).unwrap();
        for o in &others {
            assert_eq!(fs::read_to_string(o).unwrap(), text, "{}", o.display());
        }
        assert_eq!(fs::read_to_string(&pin).unwrap(), "");
        // No temporary file is left behind.
        let left = fs::read_dir(&dir).unwrap().count();
        assert_eq!(left, others.len() + 1);
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
        // it into a new line and wake the commented-out key. Beyond
        // Latin-1 the file is refused instead.
        let a = blob("ssh-ed25519", b"a");
        let b = blob("ssh-ed25519", b"b");
        let text = format!("# \u{200A}{}\r\n{}\r\n", line("a", &a), line("b", &b));
        assert!(decode(&utf16(&text)).is_err());
        let mut bytes = format!("{}\n", line("b", &b)).into_bytes();
        bytes.extend_from_slice(utf16(&text).get(2..).unwrap());
        assert!(decode(&bytes).is_err());
        // A Latin-1 comment is fine, and stays a comment, with or
        // without a UTF-8 part before it.
        let text = format!("# caf\u{e9} {}\r\n{}\r\n", line("a", &a), line("b", &b));
        let mut bytes = format!("{}\n", line("b", &b)).into_bytes();
        bytes.extend_from_slice(utf16(&text).get(2..).unwrap());
        for file in [utf16(&text), bytes] {
            let kh = KnownHosts::parse(&decode(&file).unwrap());
            assert!(matches!(kh.check("a", 22, &a), Status::Unknown));
            assert_eq!(kh.check("b", 22, &b), Status::Trusted);
        }
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
    fn a_stray_zero_byte_in_utf8_is_an_error_not_lost_lines() {
        // A zero byte in a UTF-8 comment must not turn the rest of the
        // file, a @revoked line included, into one unparsable line.
        let k = blob("ssh-ed25519", b"k");
        let text = format!("{}\n# \x00\n@revoked {}\n", line("host", &k), line("*", &k));
        let e = decode(text.as_bytes()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().starts_with("line 2 "), "{e}");
        // Also right after the `#`, where the rest would read as one
        // UTF-16 comment line.
        let text = format!("{}\n#\x00\n@revoked {}\n", line("host", &k), line("*", &k));
        let e = decode(text.as_bytes()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().starts_with("line 2 "), "{e}");
    }

    #[test]
    fn adding_to_a_utf16_file_keeps_it_readable() {
        let dir = std::env::temp_dir().join(format!("ferossh-kh-add16-{}", std::process::id()));
        let path = dir.join("known_hosts");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let a = blob("ssh-ed25519", b"a");
        let b = blob("ssh-ed25519", b"b");
        fs::write(&path, utf16(&format!("{}\r\n", line("a", &a)))).unwrap();
        add(&path, "b", 22, &b).unwrap();
        let kh = KnownHosts::load(&path).unwrap();
        assert_eq!(kh.check("a", 22, &a), Status::Trusted);
        assert_eq!(kh.check("b", 22, &b), Status::Trusted);
        assert!(!fs::read(&path).unwrap().contains(&0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_keeps_the_other_hosts_on_a_shared_line() {
        let dir = std::env::temp_dir().join(format!("ferossh-kh-share-{}", std::process::id()));
        let path = dir.join("known_hosts");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rsa = blob("ssh-rsa", b"shared");
        let wild = blob("ssh-ed25519", b"wild");
        let new = blob("ssh-ed25519", b"new");
        fs::write(
            &path,
            format!(
                "{}\n{}\n{} # only h\n",
                line("a,H,[b]:2222", &rsa),
                line("*.lan,!x.lan", &wild),
                line("h", &rsa)
            ),
        )
        .unwrap();
        replace(&path, "h", 22, &new).unwrap();
        replace(&path, "srv.lan", 22, &new).unwrap();
        let kh = KnownHosts::load(&path).unwrap();
        // The replaced hosts have only the new key.
        for h in ["h", "srv.lan"] {
            assert_eq!(kh.check(h, 22, &new), Status::Trusted);
            assert!(!matches!(kh.check(h, 22, &rsa), Status::Trusted));
            assert!(!matches!(kh.check(h, 22, &wild), Status::Trusted));
        }
        // Everyone else on those lines keeps theirs.
        assert_eq!(kh.check("a", 22, &rsa), Status::Trusted);
        assert_eq!(kh.check("b", 2222, &rsa), Status::Trusted);
        assert_eq!(kh.check("other.lan", 22, &wild), Status::Trusted);
        assert!(!matches!(kh.check("x.lan", 22, &wild), Status::Trusted));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("a,[b]:2222 ssh-rsa"), "{text}");
        assert!(text.contains("*.lan,!x.lan,!srv.lan ssh-ed25519"), "{text}");
        assert!(!text.contains("# only h"), "{text}");
        let _ = fs::remove_dir_all(&dir);
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
