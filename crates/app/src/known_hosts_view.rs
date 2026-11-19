//! Tools > Known hosts: what the known hosts manager lists and searches.
//!
//! It reads the known_hosts files FeroSSH checks host keys against (the
//! usual one and the `UserKnownHostsFile`s `~/.ssh/config` names), lists
//! every entry with its kind (host key, `@cert-authority`, `@revoked`,
//! unreadable), key type and SHA-256 fingerprint, and removes the ones
//! picked. Platform-neutral, so it is tested on any host; the native
//! controls are in `known_hosts_panel.rs`.
//!
//! A hashed name (`|1|salt|hash`, what `HashKnownHosts yes` and FeroSSH
//! itself write) cannot be shown: the search box tells which hashed lines
//! are for a typed host name (`host`, `host:port` or `[host]:port`), as a
//! connection to it would look them up.

use std::path::{Path, PathBuf};

use ferossh_config::hostkey;
use ferossh_config::known_hosts::{self, LineKind, Listing};

/// One entry, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Which of the files (an index into [`KnownHostsView::files`]).
    pub file: usize,
    pub line: usize,
    /// The host patterns; for a hashed name, "(hashed)", or the host typed
    /// in the search when it is the one the line is for.
    pub hosts: String,
    /// "Host key", "CA", "Revoked" or "Unreadable".
    pub kind: &'static str,
    /// `ED25519 256`, else the line's key type.
    pub key: String,
    /// `SHA256:...`; empty for a key that does not decode.
    pub fingerprint: String,
    /// `known_hosts:12`.
    pub place: String,
    pub comment: String,
}

struct Entry {
    file: usize,
    listing: Listing,
    key: String,
    fingerprint: String,
}

/// The known hosts manager's state.
#[derive(Default)]
pub struct KnownHostsView {
    files: Vec<PathBuf>,
    entries: Vec<Entry>,
    /// Files that could not be read, with why.
    errors: Vec<String>,
    query: String,
}

/// The host a search names, as known_hosts stores it: `host`,
/// `host:port` or `[host]:port`. `None` when the text cannot be a host
/// name (spaces, empty), so a search for a key type or fingerprint does
/// not hash every line for nothing.
pub fn search_host(query: &str) -> Option<(String, u16)> {
    let q = query.trim();
    if q.is_empty() || q.contains(char::is_whitespace) {
        return None;
    }
    if let Some(rest) = q.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if tail.is_empty() => 22,
            None => return None,
        };
        return (!host.is_empty()).then(|| (host.to_owned(), port));
    }
    // One colon: a port. More: an IPv6 address without one.
    match q.split_once(':') {
        Some((host, port)) if !port.contains(':') => {
            Some((host.to_owned(), port.parse().ok().filter(|&p| p != 0)?))
        }
        _ => Some((q.to_owned(), 22)),
    }
}

/// A search that names a host rather than a word: a dot, a colon or a
/// bracket (`web.example`, `10.0.0.1`, `web:2222`, `[::1]`).
fn looks_like_host(query: &str) -> bool {
    query.contains(['.', ':', '['])
}

impl KnownHostsView {
    /// Reads `files` again; a file that does not exist lists nothing.
    pub fn reload(&mut self, files: Vec<PathBuf>) {
        self.entries.clear();
        self.errors.clear();
        for (i, path) in files.iter().enumerate() {
            match known_hosts::read(path) {
                Ok(text) => {
                    for listing in known_hosts::list(&text) {
                        let desc = hostkey::describe(&listing.key);
                        let key = desc.as_ref().map_or_else(
                            || listing.key_type.clone(),
                            |d| format!("{} {}", d.kind, d.bits),
                        );
                        let fingerprint = desc.map(|d| d.fingerprint).unwrap_or_default();
                        self.entries.push(Entry {
                            file: i,
                            listing,
                            key,
                            fingerprint,
                        });
                    }
                }
                Err(e) => self.errors.push(format!("{} ({e})", path.display())),
            }
        }
        self.files = files;
    }

    /// Files that could not be read, for the panel to say so.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.chars().take(200).collect();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Every entry listed, before the search.
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    /// The entries the search shows: every word of it found in the hosts,
    /// kind, key, fingerprint, comment or file name, or the whole search
    /// a host the line is for (which is how hashed names are found).
    pub fn rows(&self) -> Vec<Row> {
        let host = search_host(&self.query);
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.entries
            .iter()
            .filter_map(|e| {
                // Hashed lines can only be found this way; pattern lines
                // also by their text, so a word that merely could be a
                // host name ("ed25519") does not list every `*` line.
                let try_host = e.listing.hosts.is_none() || looks_like_host(&self.query);
                let for_host = try_host
                    && host
                        .as_ref()
                        .is_some_and(|(h, p)| e.listing.matches_host(h, *p));
                let shown = self.row(e, if for_host { host.as_ref() } else { None });
                let hay = format!(
                    "{} {} {} {} {} {} {}",
                    shown.hosts,
                    shown.kind,
                    shown.key,
                    e.listing.key_type,
                    shown.fingerprint,
                    shown.comment,
                    shown.place
                )
                .to_lowercase();
                (for_host || words.iter().all(|w| hay.contains(w.as_str()))).then_some(shown)
            })
            .collect()
    }

    fn row(&self, e: &Entry, typed: Option<&(String, u16)>) -> Row {
        let l = &e.listing;
        let hosts = match (&l.hosts, typed) {
            (Some(h), _) => h.clone(),
            (None, Some((h, p))) => {
                format!("{} (hashed)", known_hosts::host_token(h, *p))
            }
            (None, None) => "(hashed)".to_owned(),
        };
        let name = self
            .files
            .get(e.file)
            .and_then(|f| f.file_name())
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Row {
            file: e.file,
            line: l.line,
            hosts,
            kind: l.kind.label(),
            key: e.key.clone(),
            fingerprint: e.fingerprint.clone(),
            place: format!("{name}:{}", l.line),
            comment: l.comment.clone(),
        }
    }

    fn entry(&self, file: usize, line: usize) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.file == file && e.listing.line == line)
    }

    /// What removing these rows asks the user to confirm.
    pub fn confirm_text(&self, rows: &[(usize, usize)]) -> Option<String> {
        let picked: Vec<&Entry> = rows.iter().filter_map(|&(f, l)| self.entry(f, l)).collect();
        let first = picked.first()?;
        let row = self.row(first, None);
        let what = match (picked.len(), first.listing.kind) {
            (1, LineKind::HostKey) => format!(
                "Remove the {} host key of {}?\n\n{}\n\nThe next connection to {} asks \
                 about its key again, as for a host never seen before.",
                row.key,
                row.hosts,
                row.fingerprint,
                if first.listing.hosts.is_some() {
                    "these hosts"
                } else {
                    "this host"
                },
            ),
            (1, LineKind::CertAuthority) => format!(
                "Remove the certificate authority for {}?\n\n{} {}\n\nHost certificates \
                 it signed are no longer accepted for these hosts.",
                row.hosts, row.key, row.fingerprint
            ),
            (1, LineKind::Revoked) => format!(
                "Remove the @revoked line for {}?\n\n{} {}\n\nThis key will no longer be \
                 refused: a host offering it is checked like any other key again.",
                row.hosts, row.key, row.fingerprint
            ),
            (1, LineKind::Unreadable) => format!(
                "Remove line {} of {}?\n\nFeroSSH and OpenSSH skip this line, as they \
                 cannot read it.",
                first.listing.line,
                self.files
                    .get(first.file)
                    .map_or_else(String::new, |f| f.display().to_string())
            ),
            (n, _) => format!("Remove these {n} known_hosts entries?"),
        };
        Some(what)
    }

    /// Whether the picked rows hold a `@revoked` line, which takes a
    /// second confirmation: removing it trusts a key again.
    pub fn removes_revoked(&self, rows: &[(usize, usize)]) -> bool {
        rows.iter()
            .filter_map(|&(f, l)| self.entry(f, l))
            .any(|e| e.listing.kind == LineKind::Revoked)
    }

    /// Removes the picked rows (file index, line) from their files and
    /// reads the files again; the error names the file that failed. Lines
    /// that changed since they were listed are never removed.
    pub fn remove(&mut self, rows: &[(usize, usize)]) -> Result<usize, String> {
        let mut removed = 0;
        let mut result = Ok(());
        for (i, path) in self.files.iter().enumerate() {
            let lines: Vec<(usize, String)> = rows
                .iter()
                .filter(|(f, _)| *f == i)
                .filter_map(|&(f, l)| self.entry(f, l))
                .map(|e| (e.listing.line, e.listing.text.clone()))
                .collect();
            if lines.is_empty() {
                continue;
            }
            match known_hosts::remove_lines(path, &lines) {
                Ok(n) => removed += n,
                Err(e) => {
                    result = Err(e.to_string());
                    break;
                }
            }
        }
        let files = std::mem::take(&mut self.files);
        self.reload(files);
        result.map(|()| removed)
    }

    /// The fingerprint of an entry, for Copy fingerprint.
    pub fn fingerprint(&self, file: usize, line: usize) -> Option<String> {
        self.entry(file, line)
            .map(|e| e.fingerprint.clone())
            .filter(|f| !f.is_empty())
    }

    /// The whole line of an entry, for Copy line.
    pub fn line_text(&self, file: usize, line: usize) -> Option<String> {
        self.entry(file, line).map(|e| e.listing.text.clone())
    }
}

/// The known_hosts files to list: the one FeroSSH writes (`default`),
/// then every other `UserKnownHostsFile` in the ssh_config text that is
/// a plain path (no `%` tokens but `%d`, not `none`, not on a network
/// share, which Windows would log in to). `~` and `%d` are `home`.
pub fn files_to_list(default: &Path, config: &str, home: &Path) -> Vec<PathBuf> {
    let mut files = vec![default.to_path_buf()];
    let home_text = home.display().to_string();
    for line in config.lines() {
        let line = line.trim();
        let (key, rest) = match line.find(|c: char| c.is_whitespace() || c == '=') {
            Some(i) => line.split_at(i),
            None => continue,
        };
        if !key.eq_ignore_ascii_case("UserKnownHostsFile") {
            continue;
        }
        let rest = rest.trim_start().trim_start_matches('=').trim_start();
        for arg in split_args(rest) {
            if arg.eq_ignore_ascii_case("none") {
                continue;
            }
            let arg = arg.replace("%d", &home_text);
            if arg.contains('%') {
                continue;
            }
            let path = match arg.strip_prefix('~') {
                Some(r) if r.is_empty() || r.starts_with(['/', '\\']) => {
                    PathBuf::from(format!("{home_text}{r}"))
                }
                _ => PathBuf::from(&arg),
            };
            let network = arg.starts_with("\\\\") || arg.starts_with("//");
            if !network && !files.contains(&path) {
                files.push(path);
            }
        }
    }
    files
}

/// Whitespace-separated words, double quotes grouping, as ssh_config
/// reads arguments.
fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '#' if !quoted && !any => break,
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::fs;

    fn ed25519(seed: u8) -> Vec<u8> {
        let mut b = 11u32.to_be_bytes().to_vec();
        b.extend_from_slice(b"ssh-ed25519");
        b.extend_from_slice(&32u32.to_be_bytes());
        b.extend_from_slice(&[seed; 32]);
        b
    }

    fn line(hosts: &str, key: &[u8]) -> String {
        const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::new();
        for chunk in key.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    b64.push(char::from(ABC[(n >> (18 - 6 * i) & 63) as usize]));
                } else {
                    b64.push('=');
                }
            }
        }
        format!("{hosts} ssh-ed25519 {b64}")
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ferossh-khv-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn search_host_reads_ports_and_brackets() {
        assert_eq!(search_host("web"), Some(("web".into(), 22)));
        assert_eq!(search_host(" web:2222 "), Some(("web".into(), 2222)));
        assert_eq!(search_host("[web]:2222"), Some(("web".into(), 2222)));
        assert_eq!(search_host("[::1]"), Some(("::1".into(), 22)));
        assert_eq!(search_host("fe80::1"), Some(("fe80::1".into(), 22)));
        assert_eq!(search_host("web:x"), None);
        assert_eq!(search_host("web:0"), None);
        assert_eq!(search_host("ssh ed25519"), None);
        assert_eq!(search_host(""), None);
        assert_eq!(search_host("[web]x"), None);
    }

    #[test]
    fn lists_searches_and_finds_hashed_hosts() {
        let d = dir("list");
        let first = d.join("known_hosts");
        let second = d.join("more");
        let hashed = known_hosts::hashed_line("secret.example", 2222, &ed25519(3)).unwrap();
        fs::write(
            &first,
            format!(
                "{}\n{hashed}\n@revoked {}\n",
                line("alpha.example,10.0.0.1", &ed25519(1)),
                line("*", &ed25519(9))
            ),
        )
        .unwrap();
        fs::write(
            &second,
            format!("@cert-authority {}\n", line("*.corp", &ed25519(5))),
        )
        .unwrap();
        let mut v = KnownHostsView::default();
        v.reload(vec![first.clone(), second.clone(), d.join("missing")]);
        assert!(v.errors().is_empty());
        assert_eq!(v.total(), 4);
        let all = v.rows();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].hosts, "alpha.example,10.0.0.1");
        assert_eq!(all[0].kind, "Host key");
        assert_eq!(all[0].key, "ED25519 256");
        assert!(all[0].fingerprint.starts_with("SHA256:"));
        assert_eq!(all[0].place, "known_hosts:1");
        assert_eq!(all[1].hosts, "(hashed)");
        assert_eq!(all[2].kind, "Revoked");
        assert_eq!(all[3].kind, "CA");
        assert_eq!(all[3].place, "more:1");
        // Words anywhere in the row.
        v.set_query("alpha");
        assert_eq!(v.rows().len(), 1);
        v.set_query("revoked ed25519");
        assert_eq!(v.rows().len(), 1);
        let fp = all[3].fingerprint.clone();
        v.set_query(&fp[7..20]);
        assert_eq!(v.rows()[0].kind, "CA");
        // A host name finds the hashed line for it, and the lines whose
        // patterns cover it.
        v.set_query("secret.example:2222");
        let rows = v.rows();
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].hosts, "[secret.example]:2222 (hashed)");
        assert_eq!(rows[1].kind, "Revoked");
        v.set_query("[secret.example]:2222");
        assert_eq!(v.rows().len(), 2);
        v.set_query("secret.example");
        assert!(
            v.rows()
                .iter()
                .all(|r| r.hosts != "[secret.example]:2222 (hashed)")
        );
        v.set_query("db.corp");
        assert_eq!(v.rows().iter().filter(|r| r.kind == "CA").count(), 1);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn removes_picked_rows_and_reloads() {
        let d = dir("remove");
        let path = d.join("known_hosts");
        let hashed = known_hosts::hashed_line("secret.example", 22, &ed25519(3)).unwrap();
        fs::write(
            &path,
            format!(
                "# mine\n{}\n{hashed}\n@revoked {}\n",
                line("alpha", &ed25519(1)),
                line("*", &ed25519(9))
            ),
        )
        .unwrap();
        let mut v = KnownHostsView::default();
        v.reload(vec![path.clone()]);
        let rows = v.rows();
        let hashed_row = (rows[1].file, rows[1].line);
        assert!(v.confirm_text(&[hashed_row]).unwrap().contains("(hashed)"));
        assert!(!v.removes_revoked(&[hashed_row]));
        let revoked = (rows[2].file, rows[2].line);
        assert!(v.removes_revoked(&[revoked]));
        assert!(
            v.confirm_text(&[revoked])
                .unwrap()
                .contains("no longer be refused")
        );
        assert!(v.confirm_text(&[(0, 99)]).is_none());
        assert_eq!(v.remove(&[hashed_row, revoked]), Ok(2));
        assert_eq!(v.total(), 1);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("# mine\n{}\n", line("alpha", &ed25519(1)))
        );
        // The file changed under the list: nothing is removed, and the
        // list is read again.
        let stale = (v.rows()[0].file, v.rows()[0].line);
        fs::write(&path, format!("{}\n", line("beta", &ed25519(2)))).unwrap();
        assert!(v.remove(&[stale]).is_err());
        assert_eq!(v.rows()[0].hosts, "beta");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn files_from_the_config() {
        let home = Path::new("/home/me");
        let default = home.join(".ssh/known_hosts");
        let config = "Host a\n  UserKnownHostsFile ~/.ssh/kh_a \"/keys/with space\"\n\
                      Host b\n  UserKnownHostsFile=%d/.ssh/known_hosts\n\
                      Host c\n  userknownhostsfile none\n\
                      Host d\n  UserKnownHostsFile ~/.ssh/%h_hosts \\\\server\\share\\kh\n\
                      # UserKnownHostsFile /commented\n";
        assert_eq!(
            files_to_list(&default, config, home),
            vec![
                default.clone(),
                PathBuf::from("/home/me/.ssh/kh_a"),
                PathBuf::from("/keys/with space"),
            ]
        );
    }
}
