//! Tamper-evident logs: a SHA-256 hash chain over a log file's bytes.
//!
//! Next to `host_tab1_….log` FeroSSH writes `host_tab1_….log.chain`:
//!
//! ```text
//! FeroSSH log chain 1
//! log host_tab1_2026-10-26_0915.log
//! seal 4096 <sha-256 hex> <unix time>
//! seal 9012 <sha-256 hex> <unix time>
//! end 9530 <sha-256 hex> <unix time>
//! ```
//!
//! The chain starts from a hash of the log's file name, and each seal is
//! the hash of the previous seal followed by the log's bytes since it.
//! Changing, removing or inserting a byte anywhere breaks every seal from
//! there on; cutting the file short or adding to it after `end` shows too.
//! When a log ends, its last seal goes into `log-index.chain` in the same
//! folder, where each line also hashes the line before it, so a log whose
//! chain was rewritten as a whole no longer matches the index.
//!
//! This detects edits; it cannot prevent them. Someone with the same
//! account can rewrite a log, its chain and the index from that point on.
//! Copying the index off the machine (or noting the last head) is what
//! makes the record hold up later.

use std::io::{self, Write};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

const MAGIC: &str = "FeroSSH log chain 1";
/// File name of the index of ended logs, in the log folder.
pub const INDEX_FILE: &str = "log-index.chain";
/// A seal is written at most this often while output keeps coming...
const SEAL_INTERVAL_SECS: u64 = 10;
/// ...or once this much is unsealed, whichever comes first.
const SEAL_BYTES: u64 = 1024 * 1024;

type Hash = [u8; 32];

fn start(file_name: &str) -> Hash {
    let mut h = Sha256::new();
    h.update(MAGIC.as_bytes());
    h.update(b"\n");
    h.update(file_name.as_bytes());
    h.finalize().into()
}

fn segment(prev: &Hash) -> Sha256 {
    let mut h = Sha256::new();
    h.update(prev);
    h
}

fn hex(hash: &Hash) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Hash> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The log file itself, hashing what goes into it, and the chain file
/// beside it.
pub(crate) struct Chained<W: Write> {
    log: W,
    chain: W,
    head: Hash,
    hasher: Sha256,
    written: u64,
    sealed: u64,
    last_seal: Instant,
}

impl<W: Write> Chained<W> {
    pub(crate) fn new(log: W, mut chain: W, file_name: &str) -> io::Result<Self> {
        write!(chain, "{MAGIC}\nlog {file_name}\n")?;
        chain.flush()?;
        let head = start(file_name);
        Ok(Self {
            log,
            chain,
            head,
            hasher: segment(&head),
            written: 0,
            sealed: 0,
            last_seal: Instant::now(),
        })
    }

    fn record(&mut self, kind: &str) -> io::Result<()> {
        let hasher = std::mem::replace(&mut self.hasher, Sha256::new());
        self.head = hasher.finalize().into();
        self.hasher = segment(&self.head);
        self.sealed = self.written;
        self.last_seal = Instant::now();
        writeln!(
            self.chain,
            "{kind} {} {} {}",
            self.written,
            hex(&self.head),
            unix_now()
        )?;
        self.chain.flush()
    }

    /// Seals what was written since the last seal, if it is time to.
    pub(crate) fn maybe_seal(&mut self) -> io::Result<()> {
        let unsealed = self.written - self.sealed;
        if unsealed > 0
            && (unsealed >= SEAL_BYTES || self.last_seal.elapsed().as_secs() >= SEAL_INTERVAL_SECS)
        {
            self.record("seal")?;
        }
        Ok(())
    }

    /// Seals everything and writes `end`. Returns the log, the chain file,
    /// the log's length and the last seal for the index.
    pub(crate) fn end(mut self) -> io::Result<(W, W, u64, String)> {
        self.log.flush()?;
        self.record("end")?;
        let head = hex(&self.head);
        Ok((self.log, self.chain, self.written, head))
    }
}

impl<W: Write> Write for Chained<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.log.write(buf)?;
        let done = buf.get(..n).unwrap_or_default();
        self.hasher.update(done);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.log.flush()
    }
}

/// What checking a log against its chain found, when nothing was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intact {
    /// Bytes covered by seals.
    pub sealed: u64,
    pub seals: usize,
    /// The chain was closed: the log ended normally.
    pub ended: bool,
    /// Bytes after the last seal (a log still being written, or one whose
    /// program stopped before sealing them). Never set when `ended`.
    pub unsealed: u64,
    /// The last seal, as written in the index.
    pub head: String,
}

/// Why a log does not match its chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Broken {
    /// The chain file is not one FeroSSH wrote (line number, from 1).
    Malformed(usize),
    /// The chain belongs to a log of another name.
    OtherLog(String),
    /// The log's bytes in this range do not hash to seal number `seal`
    /// (from 1).
    Changed { seal: usize, from: u64, to: u64 },
    /// The log is shorter than the chain says.
    Truncated { expected: u64, actual: u64 },
    /// Bytes were added after the log ended.
    Appended { extra: u64 },
}

impl std::fmt::Display for Broken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(line) => write!(f, "the chain file is damaged at line {line}"),
            Self::OtherLog(name) => write!(f, "the chain file belongs to another log ({name})"),
            Self::Changed { seal, from, to } => write!(
                f,
                "the log was changed between bytes {from} and {to} (seal {seal})"
            ),
            Self::Truncated { expected, actual } => write!(
                f,
                "the log was cut short: {actual} bytes where {expected} were sealed"
            ),
            Self::Appended { extra } => {
                write!(f, "{extra} bytes were added after the log ended")
            }
        }
    }
}

/// Checks `log`, the bytes of file `file_name`, against its chain file.
pub fn verify(file_name: &str, log: &[u8], chain: &str) -> Result<Intact, Broken> {
    let mut lines = chain.lines().enumerate();
    if lines.next().map(|(_, l)| l) != Some(MAGIC) {
        return Err(Broken::Malformed(1));
    }
    match lines.next().map(|(_, l)| l.strip_prefix("log ")) {
        Some(Some(name)) if name == file_name => {}
        Some(Some(name)) => return Err(Broken::OtherLog(name.to_owned())),
        _ => return Err(Broken::Malformed(2)),
    }
    let mut head = start(file_name);
    let mut at = 0u64;
    let mut seals = 0;
    let mut ended = false;
    for (i, line) in lines {
        let n = i + 1;
        if ended {
            return Err(Broken::Malformed(n));
        }
        let mut fields = line.split(' ');
        let (Some(kind), Some(offset), Some(hash), Some(_time), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(Broken::Malformed(n));
        };
        let (Ok(offset), Some(hash)) = (offset.parse::<u64>(), unhex(hash)) else {
            return Err(Broken::Malformed(n));
        };
        match kind {
            "seal" => {}
            "end" => ended = true,
            _ => return Err(Broken::Malformed(n)),
        }
        if offset < at {
            return Err(Broken::Malformed(n));
        }
        let Some(bytes) = usize::try_from(at)
            .ok()
            .zip(usize::try_from(offset).ok())
            .and_then(|(a, b)| log.get(a..b))
        else {
            return Err(Broken::Truncated {
                expected: offset,
                actual: log.len() as u64,
            });
        };
        let mut h = segment(&head);
        h.update(bytes);
        let got: Hash = h.finalize().into();
        seals += 1;
        if got != hash {
            return Err(Broken::Changed {
                seal: seals,
                from: at,
                to: offset,
            });
        }
        head = got;
        at = offset;
    }
    let rest = (log.len() as u64).saturating_sub(at);
    if ended && rest > 0 {
        return Err(Broken::Appended { extra: rest });
    }
    Ok(Intact {
        sealed: at,
        seals,
        ended,
        unsealed: rest,
        head: hex(&head),
    })
}

/// One line of the index of ended logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub time: u64,
    pub file_name: String,
    pub bytes: u64,
    pub head: String,
}

fn link(prev: &str, body: &str) -> String {
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(body.as_bytes());
    hex(&h.finalize().into())
}

/// The index line to append for a log that ended, given the index so far.
/// File names never hold spaces (see `log_file_name`); any are replaced.
pub fn index_line(index: &str, file_name: &str, bytes: u64, head: &str) -> String {
    let prev = index
        .lines()
        .last()
        .and_then(|l| l.rsplit(' ').next())
        .unwrap_or(MAGIC);
    let name = file_name.replace(char::is_whitespace, "_");
    let body = format!("{} {name} {bytes} {head}", unix_now());
    format!("{body} {}\n", link(prev, &body))
}

/// Reads the index, checking that each line links to the one before it.
/// The error is the first line (from 1) that was changed or does not
/// follow from the previous one.
pub fn read_index(index: &str) -> Result<Vec<IndexEntry>, usize> {
    let mut prev = MAGIC.to_owned();
    let mut out = Vec::new();
    for (i, line) in index.lines().enumerate() {
        let n = i + 1;
        let Some((body, tail)) = line.rsplit_once(' ') else {
            return Err(n);
        };
        if link(&prev, body) != tail {
            return Err(n);
        }
        let mut f = body.split(' ');
        let (Some(time), Some(name), Some(bytes), Some(head), None) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            return Err(n);
        };
        let (Ok(time), Ok(bytes)) = (time.parse(), bytes.parse()) else {
            return Err(n);
        };
        out.push(IndexEntry {
            time,
            file_name: name.to_owned(),
            bytes,
            head: head.to_owned(),
        });
        prev = tail.to_owned();
    }
    Ok(out)
}

/// A report for the user on a log, its chain and the index.
pub fn report(file_name: &str, log: &[u8], chain: Option<&str>, index: Option<&str>) -> String {
    let Some(chain) = chain else {
        return format!(
            "{file_name} has no chain file ({file_name}.chain), so it cannot be checked. \
             Logs from earlier FeroSSH versions have none."
        );
    };
    let intact = match verify(file_name, log, chain) {
        Ok(intact) => intact,
        Err(broken) => return format!("{file_name} FAILED the check: {broken}."),
    };
    let mut text = if intact.ended {
        format!(
            "{file_name} is intact: all {} bytes match its {} seal{}.",
            intact.sealed,
            intact.seals,
            if intact.seals == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "The first {} bytes of {file_name} match its seals, but the log has not \
             ended (it is still open, or FeroSSH stopped first), so the last {} \
             bytes are not sealed yet.",
            intact.sealed, intact.unsealed
        )
    };
    match index.map(read_index) {
        None => text.push_str(" There is no log index to compare it with."),
        Some(Err(line)) => {
            text.push_str(&format!(
                " WARNING: the log index ({INDEX_FILE}) was changed at line {line}."
            ));
        }
        Some(Ok(entries)) => match entries.iter().rfind(|e| e.file_name == file_name) {
            Some(e) if e.head == intact.head && e.bytes == intact.sealed => {
                text.push_str(" The log index agrees.");
            }
            Some(_) => text.push_str(
                " WARNING: the log index has a different last seal for it, so the log \
                 and its chain were rewritten together.",
            ),
            None if intact.ended => {
                text.push_str(" WARNING: the log index has no entry for it.");
            }
            None => {}
        },
    }
    text.push_str(&format!("\n\nLast seal: {}", intact.head));
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn chained(parts: &[&[u8]], seal_each: bool) -> (Vec<u8>, String, String) {
        let mut c = Chained::new(Vec::new(), Vec::new(), "a.log").unwrap();
        for p in parts {
            c.write_all(p).unwrap();
            if seal_each {
                c.record("seal").unwrap();
            }
        }
        let (log, chain, _, head) = c.end().unwrap();
        (log, String::from_utf8(chain).unwrap(), head)
    }

    #[test]
    fn an_untouched_log_verifies() {
        let (log, chain, head) = chained(&[b"one\r\n", b"two\r\n", b"three\r\n"], true);
        let got = verify("a.log", &log, &chain).unwrap();
        assert_eq!(got.seals, 4);
        assert!(got.ended);
        assert_eq!(got.sealed, log.len() as u64);
        assert_eq!(got.head, head);
    }

    #[test]
    fn any_change_is_found() {
        let (mut log, chain, _) = chained(&[b"one\r\n", b"two\r\n", b"three\r\n"], true);
        log[6] = b'T';
        assert_eq!(
            verify("a.log", &log, &chain),
            Err(Broken::Changed {
                seal: 2,
                from: 5,
                to: 10
            })
        );
    }

    #[test]
    fn cuts_additions_and_renames_are_found() {
        let (log, chain, _) = chained(&[b"one\r\n", b"two\r\n"], false);
        assert!(matches!(
            verify("a.log", &log[..4], &chain),
            Err(Broken::Truncated { .. })
        ));
        let mut more = log.clone();
        more.extend_from_slice(b"forged\r\n");
        assert_eq!(
            verify("a.log", &more, &chain),
            Err(Broken::Appended { extra: 8 })
        );
        assert_eq!(
            verify("b.log", &log, &chain),
            Err(Broken::OtherLog("a.log".into()))
        );
        // Dropping a seal line leaves the chain unable to match.
        let dropped: String = chain
            .lines()
            .filter(|l| !l.starts_with("end"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(verify("a.log", &log, &dropped).unwrap().unsealed > 0);
    }

    #[test]
    fn a_log_still_open_reports_unsealed_bytes() {
        let mut c = Chained::new(Vec::new(), Vec::new(), "a.log").unwrap();
        c.write_all(b"sealed").unwrap();
        c.record("seal").unwrap();
        c.write_all(b"not yet").unwrap();
        let log = c.log.clone();
        let chain = String::from_utf8(c.chain.clone()).unwrap();
        let got = verify("a.log", &log, &chain).unwrap();
        assert!(!got.ended);
        assert_eq!((got.sealed, got.unsealed), (6, 7));
    }

    #[test]
    fn the_index_links_its_lines() {
        let mut index = String::new();
        index.push_str(&index_line(&index, "a.log", 10, &"ab".repeat(32)));
        index.push_str(&index_line(&index, "b.log", 20, &"cd".repeat(32)));
        let entries = read_index(&index).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].file_name, "b.log");
        // Editing the first line breaks it; removing it breaks the second.
        assert_eq!(read_index(&index.replacen(" 10 ", " 11 ", 1)), Err(1));
        let second: String = index.lines().nth(1).unwrap().to_owned();
        assert_eq!(read_index(&second), Err(1));
    }

    #[test]
    fn the_report_compares_with_the_index() {
        let (log, chain, head) = chained(&[b"hello\r\n"], false);
        let index = index_line("", "a.log", log.len() as u64, &head);
        let ok = report("a.log", &log, Some(&chain), Some(&index));
        assert!(
            ok.contains("is intact") && ok.contains("index agrees"),
            "{ok}"
        );
        // A rewritten log with a fresh chain still disagrees with the index.
        let (log2, chain2, _) = chained(&[b"HELLO\r\n"], false);
        let bad = report("a.log", &log2, Some(&chain2), Some(&index));
        assert!(bad.contains("rewritten together"), "{bad}");
        let none = report("a.log", &log, None, None);
        assert!(none.contains("no chain file"), "{none}");
    }
}
