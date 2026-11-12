//! What goes inside FeroSSH's encrypted file ([`crate::sealed`]): the
//! connection files (`sessions.conf`, the `imported-*.conf` files,
//! `ferossh.conf`) and, only when the user asks, saved passwords. Each
//! part carries its length, so no content needs escaping:
//!
//! ```text
//! FeroSSH connections v1
//! file sessions.conf 52
//! <52 bytes>
//! secret 24 5 9
//! <24 bytes of the credential's name><5 of the user><9 of the password>
//! end
//! ```
//!
//! Also here: how an imported file merges into what the user has, by
//! connection name, so nothing of theirs is lost that the file lacks.

use zeroize::Zeroizing;

use crate::Settings;

const MAGIC: &str = "FeroSSH connections v1";

/// A saved password: the name it is kept under (`FeroSSH:user@host:port`
/// and the like), the user, and the password.
pub struct Secret {
    pub target: String,
    pub user: String,
    pub password: Zeroizing<String>,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secret")
            .field("target", &self.target)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Secret {
            target: self.target.clone(),
            user: self.user.clone(),
            password: Zeroizing::new(self.password.as_str().to_owned()),
        }
    }
}

/// Whether `target` is a name FeroSSH keeps a password under: an
/// imported file may not write anything else to the Credential Manager.
pub fn is_secret_target(target: &str) -> bool {
    (target.starts_with("FeroSSH:")
        || target.starts_with("FeroSSH proxy:")
        || target.starts_with("FeroSSH secret:"))
        && target.len() <= 512
        && !target.contains(|c: char| c.is_control())
}

/// Whether `name` may be a file in the bundle: a plain `.conf` name in
/// FeroSSH's folder, never a path.
pub fn is_file_name(name: &str) -> bool {
    name.len() <= 64
        && name.ends_with(".conf")
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The files and passwords in an encrypted file.
#[derive(Debug, Clone, Default)]
pub struct Bundle {
    /// File name and its text, in the order written.
    pub files: Vec<(String, String)>,
    pub secrets: Vec<Secret>,
}

impl Bundle {
    pub fn file(&self, name: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.as_str())
    }

    /// Sets `name` to `text`, or adds it.
    pub fn set_file(&mut self, name: &str, text: String) {
        match self.files.iter_mut().find(|(n, _)| n == name) {
            Some((_, t)) => *t = text,
            None => self.files.push((name.to_owned(), text)),
        }
    }

    pub fn remove_file(&mut self, name: &str) {
        self.files.retain(|(n, _)| n != name);
    }

    pub fn secret(&self, target: &str) -> Option<&Secret> {
        self.secrets.iter().find(|s| s.target == target)
    }

    /// Keeps `secret`, replacing one under the same name.
    pub fn set_secret(&mut self, secret: Secret) {
        self.secrets.retain(|s| s.target != secret.target);
        self.secrets.push(secret);
    }

    pub fn remove_secret(&mut self, target: &str) {
        self.secrets.retain(|s| s.target != target);
    }

    /// The bundle as bytes to seal. Files with names [`is_file_name`]
    /// refuses and secrets under other names are left out.
    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut out = Zeroizing::new(Vec::new());
        out.extend_from_slice(MAGIC.as_bytes());
        out.push(b'\n');
        for (name, text) in self.files.iter().filter(|(n, _)| is_file_name(n)) {
            out.extend_from_slice(format!("file {name} {}\n", text.len()).as_bytes());
            out.extend_from_slice(text.as_bytes());
            out.push(b'\n');
        }
        for s in self.secrets.iter().filter(|s| is_secret_target(&s.target)) {
            out.extend_from_slice(
                format!(
                    "secret {} {} {}\n",
                    s.target.len(),
                    s.user.len(),
                    s.password.len()
                )
                .as_bytes(),
            );
            out.extend_from_slice(s.target.as_bytes());
            out.extend_from_slice(s.user.as_bytes());
            out.extend_from_slice(s.password.as_bytes());
            out.push(b'\n');
        }
        out.extend_from_slice(b"end\n");
        out
    }

    /// Reads what [`Bundle::to_bytes`] wrote; `None` for anything else.
    pub fn parse(bytes: &[u8]) -> Option<Bundle> {
        let (first, mut rest) = line(bytes)?;
        if first != MAGIC.as_bytes() {
            return None;
        }
        let mut bundle = Bundle::default();
        loop {
            let (head, after) = line(rest)?;
            rest = after;
            let head = std::str::from_utf8(head).ok()?;
            let mut words = head.split(' ');
            match words.next()? {
                "end" => {
                    return (words.next().is_none() && rest.is_empty()).then_some(bundle);
                }
                "file" => {
                    let name = words.next()?;
                    let len = number(words.next()?)?;
                    if words.next().is_some() || !is_file_name(name) || bundle.file(name).is_some()
                    {
                        return None;
                    }
                    let (text, after) = take(rest, len)?;
                    rest = after;
                    bundle
                        .files
                        .push((name.to_owned(), String::from_utf8(text.to_vec()).ok()?));
                }
                "secret" => {
                    let mut len = || number(words.next()?);
                    let (t, u, p) = (len()?, len()?, len()?);
                    if words.next().is_some() {
                        return None;
                    }
                    let (target, after) = take_part(rest, t)?;
                    let (user, after) = take_part(after, u)?;
                    let (password, after) = take(after, p)?;
                    rest = after;
                    let target = String::from_utf8(target.to_vec()).ok()?;
                    if !is_secret_target(&target) || bundle.secret(&target).is_some() {
                        return None;
                    }
                    bundle.secrets.push(Secret {
                        target,
                        user: String::from_utf8(user.to_vec()).ok()?,
                        password: Zeroizing::new(String::from_utf8(password.to_vec()).ok()?),
                    });
                }
                _ => return None,
            }
        }
    }
}

/// A length as [`Bundle::to_bytes`] writes it: digits, no sign and no
/// leading zero, so one bundle has one spelling.
fn number(word: &str) -> Option<usize> {
    let plain = !word.is_empty()
        && word.bytes().all(|b| b.is_ascii_digit())
        && (word == "0" || !word.starts_with('0'));
    if plain { word.parse().ok() } else { None }
}

/// The line up to the next newline, and what follows it.
fn line(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let at = bytes.iter().position(|&b| b == b'\n')?;
    let (l, after) = bytes.split_at(at);
    Some((l, after.get(1..)?))
}

/// `len` bytes and the newline after them.
fn take(bytes: &[u8], len: usize) -> Option<(&[u8], &[u8])> {
    let (part, after) = take_part(bytes, len)?;
    let after = after.strip_prefix(b"\n")?;
    Some((part, after))
}

fn take_part(bytes: &[u8], len: usize) -> Option<(&[u8], &[u8])> {
    (len <= bytes.len()).then(|| bytes.split_at(len))
}

/// How an import changed a list of connections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Merged {
    /// In the file, not here: added.
    pub added: usize,
    /// Here under the same name but different: replaced by the file's.
    pub replaced: usize,
    /// The same here and in the file.
    pub same: usize,
}

impl Merged {
    pub fn changed(self) -> bool {
        self.added + self.replaced > 0
    }

    pub fn total(self) -> usize {
        self.added + self.replaced + self.same
    }
}

impl std::ops::AddAssign for Merged {
    fn add_assign(&mut self, o: Merged) {
        self.added += o.added;
        self.replaced += o.replaced;
        self.same += o.same;
    }
}

/// One `Host` block of an ssh_config-syntax file, with the comment lines
/// right above it (`# folder: Work`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostBlock {
    /// The `Host` line's names, lower case, to match on.
    key: String,
    text: String,
}

/// Splits ssh_config text into what comes before the first `Host` line
/// and the `Host` blocks.
fn host_blocks(text: &str) -> (String, Vec<HostBlock>) {
    let mut head = String::new();
    let mut blocks: Vec<HostBlock> = Vec::new();
    // Comments and blank lines seen since the last setting: they go with
    // the next block when a `Host` line follows.
    let mut pending = String::new();
    for line in text.lines() {
        let t = line.trim();
        let is_host = t
            .split_once(|c: char| c.is_whitespace() || c == '=')
            .is_some_and(|(w, _)| w.eq_ignore_ascii_case("host"));
        if is_host {
            let names = t
                .get(4..)
                .unwrap_or_default()
                .trim_start_matches(|c: char| c.is_whitespace() || c == '=');
            let key = names
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            // Comments right above the `Host` line are its own; what is
            // above the last blank line stays with what came before.
            let pending_text = std::mem::take(&mut pending);
            let lines: Vec<&str> = pending_text.lines().collect();
            let own_from = lines
                .iter()
                .rposition(|l| l.trim().is_empty())
                .map_or(0, |i| i + 1);
            let (before, own) = lines.split_at(own_from.min(lines.len()));
            let into = match blocks.last_mut() {
                Some(b) => &mut b.text,
                None => &mut head,
            };
            for l in before {
                into.push_str(l);
                into.push('\n');
            }
            let mut text = String::new();
            for l in own {
                text.push_str(l);
                text.push('\n');
            }
            text.push_str(line);
            text.push('\n');
            blocks.push(HostBlock { key, text });
        } else if t.is_empty() || t.starts_with('#') {
            pending.push_str(line);
            pending.push('\n');
        } else {
            let into = match blocks.last_mut() {
                Some(b) => &mut b.text,
                None => &mut head,
            };
            into.push_str(&std::mem::take(&mut pending));
            into.push_str(line);
            into.push('\n');
        }
    }
    match blocks.last_mut() {
        Some(b) => b.text.push_str(&pending),
        None => head.push_str(&pending),
    }
    (head, blocks)
}

/// Whether two blocks say the same, blank lines and indents aside.
fn same_block(a: &str, b: &str) -> bool {
    let lines = |t: &str| -> Vec<String> {
        t.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    };
    lines(a) == lines(b)
}

/// `incoming`'s `Host` blocks merged into `local`, an ssh_config-syntax
/// file (`sessions.conf`, an `imported-*.conf`): a block under a name
/// `local` has replaces it in place, the others are added at the end, and
/// `local`'s other blocks stay. `local`'s text before its first block is
/// kept (the file's own heading).
pub fn merge_hosts(local: &str, incoming: &str) -> (String, Merged) {
    let (head, mut blocks) = host_blocks(local);
    let (in_head, new) = host_blocks(incoming);
    let mut merged = Merged::default();
    let mut seen: Vec<String> = Vec::new();
    for b in new {
        // A name the file has twice: its first block counts, as
        // ssh_config takes the first match.
        if seen.contains(&b.key) {
            continue;
        }
        seen.push(b.key.clone());
        match blocks.iter_mut().find(|l| l.key == b.key) {
            Some(l) if same_block(&l.text, &b.text) => merged.same += 1,
            Some(l) => {
                *l = b;
                merged.replaced += 1;
            }
            None => {
                blocks.push(b);
                merged.added += 1;
            }
        }
    }
    let mut out = if local.trim().is_empty() {
        in_head
    } else {
        head
    };
    for b in blocks {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        // A blank line between blocks, so comments at the end of one
        // stay with it rather than with the next.
        let last_blank = out.lines().last().is_some_and(|l| l.trim().is_empty());
        if !out.is_empty()
            && !last_blank
            && !b.text.lines().next().is_some_and(|l| l.trim().is_empty())
        {
            out.push('\n');
        }
        out.push_str(&b.text);
    }
    (out, merged)
}

/// The number of `Host` blocks in ssh_config text.
pub fn count_hosts(text: &str) -> usize {
    host_blocks(text).1.len()
}

/// `incoming`'s host profiles (session type, look, login script and the
/// rest of FeroSSH's own per-connection settings) and favourites merged
/// into `local`. The rest of `incoming`'s settings (window, fonts, logs)
/// belong to the PC it came from and are left alone.
pub fn merge_profiles(local: &mut Settings, incoming: &Settings) -> Merged {
    let mut merged = Merged::default();
    for p in &incoming.profiles {
        match local
            .profiles
            .iter_mut()
            .find(|l| l.host.eq_ignore_ascii_case(&p.host))
        {
            Some(l) if l == p => merged.same += 1,
            Some(l) => {
                *l = p.clone();
                merged.replaced += 1;
            }
            None => {
                local.profiles.push(p.clone());
                merged.added += 1;
            }
        }
    }
    for f in &incoming.favourites {
        if !local.favourites.iter().any(|l| l.eq_ignore_ascii_case(f)) {
            local.favourites.push(f.clone());
        }
    }
    merged
}

/// The host profiles after two FeroSSH windows (on two PCs sharing a
/// vault) changed them from the same `base`: a profile `ours` changed
/// (or removed) keeps our version, any other takes `theirs`. In
/// `theirs`'s order, with our new ones after.
pub fn merge_profiles3(
    base: &[crate::Profile],
    ours: &[crate::Profile],
    theirs: &[crate::Profile],
) -> Vec<crate::Profile> {
    let find = |list: &[crate::Profile], host: &str| {
        list.iter()
            .find(|p| p.host.eq_ignore_ascii_case(host))
            .cloned()
    };
    let mut hosts: Vec<String> = Vec::new();
    for p in theirs.iter().chain(ours).chain(base) {
        if !hosts.iter().any(|h| h.eq_ignore_ascii_case(&p.host)) {
            hosts.push(p.host.clone());
        }
    }
    hosts
        .iter()
        .filter_map(|h| {
            let (b, o) = (find(base, h), find(ours, h));
            if o == b { find(theirs, h) } else { o }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn secret(t: &str, p: &str) -> Secret {
        Secret {
            target: t.into(),
            user: "me".into(),
            password: Zeroizing::new(p.into()),
        }
    }

    #[test]
    fn round_trip() {
        let mut b = Bundle::default();
        b.set_file("sessions.conf", "Host web\n    User me\n".into());
        b.set_file("imported-putty.conf", String::new());
        b.set_secret(secret("FeroSSH:me@web:22", "pa\nss word"));
        b.set_secret(secret("FeroSSH secret:otp", ""));
        let bytes = b.to_bytes();
        let back = Bundle::parse(&bytes).unwrap();
        assert_eq!(back.files, b.files);
        assert_eq!(back.secrets.len(), 2);
        assert_eq!(
            back.secret("FeroSSH:me@web:22").unwrap().password.as_str(),
            "pa\nss word"
        );
        assert_eq!(back.to_bytes(), bytes);
    }

    #[test]
    fn set_replaces() {
        let mut b = Bundle::default();
        b.set_file("a.conf", "1".into());
        b.set_file("a.conf", "2".into());
        b.set_secret(secret("FeroSSH:a", "1"));
        b.set_secret(secret("FeroSSH:a", "2"));
        assert_eq!(b.files.len(), 1);
        assert_eq!(b.file("a.conf"), Some("2"));
        assert_eq!(b.secrets.len(), 1);
        b.remove_file("a.conf");
        b.remove_secret("FeroSSH:a");
        assert!(b.files.is_empty() && b.secrets.is_empty());
    }

    #[test]
    fn hostile_content_is_refused() {
        let ok = |s: &str| Bundle::parse(s.as_bytes()).is_some();
        assert!(ok("FeroSSH connections v1\nend\n"));
        assert!(!ok("FeroSSH connections v1\n"));
        assert!(!ok("FeroSSH connections v1\nend\nmore"));
        // A path, not a name.
        assert!(!ok("FeroSSH connections v1\nfile ..\\x.conf 0\n\nend\n"));
        assert!(!ok("FeroSSH connections v1\nfile ../x.conf 0\n\nend\n"));
        assert!(!ok("FeroSSH connections v1\nfile x.exe 0\n\nend\n"));
        // A length past the end.
        assert!(!ok("FeroSSH connections v1\nfile a.conf 99\nabc\nend\n"));
        // A secret under a name that is not FeroSSH's.
        assert!(!ok("FeroSSH connections v1\nsecret 5 0 0\nOther\nend\n"));
        // The same file twice.
        assert!(!ok(
            "FeroSSH connections v1\nfile a.conf 0\n\nfile a.conf 0\n\nend\n"
        ));
        assert!(!ok(
            "FeroSSH connections v1\nfile a.conf 18446744073709551616\n\nend\n"
        ));
    }

    #[test]
    fn merge_adds_replaces_and_keeps() {
        let local = "# Sessions made in FeroSSH\n\n# folder: Work\nHost web\n    User me\n\n# folder: \nHost db\n    Port 5432\n";
        let incoming = "# other heading\n\n# folder: Work\nHost web\n    User root\n\n# folder: \nHost db\n  Port 5432\n\n# folder: Home\nHost nas\n    User admin\n";
        let (out, m) = merge_hosts(local, incoming);
        assert_eq!(
            m,
            Merged {
                added: 1,
                replaced: 1,
                same: 1
            }
        );
        assert!(out.starts_with("# Sessions made in FeroSSH\n"));
        assert!(out.contains("Host web\n    User root\n"));
        assert!(!out.contains("User me"));
        assert!(out.contains("# folder: Home\nHost nas\n"));
        assert_eq!(count_hosts(&out), 3);
        // Merging again changes nothing.
        let (again, m) = merge_hosts(&out, incoming);
        assert!(!m.changed());
        assert_eq!(again, out);
        // Parses as the connection manager reads it.
        let hosts = crate::saved::parse(&out);
        assert_eq!(hosts.len(), 3);
        assert_eq!(hosts[2].folder.as_deref(), Some("Home"));
    }

    #[test]
    fn merge_into_nothing() {
        let incoming = "# heading\n\nHost a\n    Port 1\n";
        let (out, m) = merge_hosts("", incoming);
        assert_eq!(m.added, 1);
        assert_eq!(out, incoming);
    }

    #[test]
    fn host_line_forms() {
        let (_, b) = host_blocks("Host=a b\nhost  C\nHostName x\n");
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].key, "a b");
        assert_eq!(b[1].key, "c");
        assert!(b[1].text.contains("HostName x"));
    }

    #[test]
    fn profiles_three_way() {
        let p = |t: &str| Settings::parse(t).profiles;
        let base = p(
            "Profile a\n    FontSize 10\nProfile b\n    FontSize 10\nProfile c\n    FontSize 10\n",
        );
        // We changed a and removed c; they changed b, added d.
        let ours = p("Profile a\n    FontSize 12\nProfile b\n    FontSize 10\n");
        let theirs = p(
            "Profile a\n    FontSize 10\nProfile b\n    FontSize 14\nProfile c\n    FontSize 10\nProfile d\n    FontSize 9\n",
        );
        let out = merge_profiles3(&base, &ours, &theirs);
        let size = |h: &str| out.iter().find(|x| x.host == h).and_then(|x| x.font_size);
        assert_eq!(out.len(), 3);
        assert_eq!(size("a"), Some(12.0));
        assert_eq!(size("b"), Some(14.0));
        assert_eq!(size("d"), Some(9.0));
        assert!(out.iter().all(|x| x.host != "c"));
    }

    #[test]
    fn profiles_merge_by_host() {
        let mut local =
            Settings::parse("Profile web\n    FontSize 14\n\nProfile db\n    FontSize 9\n");
        let incoming = Settings::parse(
            "Profile WEB\n    FontSize 20\n\nProfile db\n    FontSize 9\n\nProfile nas\n    FontSize 11\n",
        );
        let m = merge_profiles(&mut local, &incoming);
        assert_eq!(
            m,
            Merged {
                added: 1,
                replaced: 1,
                same: 1
            }
        );
        assert_eq!(local.profiles.len(), 3);
    }
}
