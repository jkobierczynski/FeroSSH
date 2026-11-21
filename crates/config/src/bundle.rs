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
    /// Named credentials added or replaced (profiles only).
    pub credentials: usize,
}

impl Merged {
    pub fn changed(self) -> bool {
        self.added + self.replaced + self.credentials > 0
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
        self.credentials += o.credentials;
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
    // The named credentials the profiles point to come along.
    for c in &incoming.credentials {
        match local
            .credentials
            .iter_mut()
            .find(|l| l.name.eq_ignore_ascii_case(&c.name))
        {
            Some(l) if l == c => {}
            Some(l) => {
                *l = c.clone();
                merged.credentials += 1;
            }
            None => {
                local.credentials.push(c.clone());
                merged.credentials += 1;
            }
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

/// The named credentials after two PCs changed them from the same
/// `base`, as [`merge_profiles3`] does for profiles: one `ours` changed,
/// added or removed keeps our version; any other takes `theirs` (a change
/// or a removal there included). Names match in any case.
pub fn merge_credentials3(
    base: &[crate::settings::Credential],
    ours: &[crate::settings::Credential],
    theirs: &[crate::settings::Credential],
) -> Vec<crate::settings::Credential> {
    let find = |list: &[crate::settings::Credential], name: &str| {
        list.iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .cloned()
    };
    let mut names: Vec<String> = Vec::new();
    for c in theirs.iter().chain(ours).chain(base) {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&c.name)) {
            names.push(c.name.clone());
        }
    }
    names
        .iter()
        .filter_map(|n| {
            let (b, o) = (find(base, n), find(ours, n));
            if o == b { find(theirs, n) } else { o }
        })
        .collect()
}

/// Host profiles and credentials after two PCs changed them from the same
/// `base` ([`merge_profiles3`], [`merge_credentials3`]), with references
/// kept whole: when one side renamed a credential (its profiles moved
/// from the old name to the new one, and the old name is gone there) and
/// the other changed something else in a profile that still names the
/// old one, that profile follows the rename.
pub fn merge_logins3(
    base: &crate::Settings,
    ours: &crate::Settings,
    theirs: &crate::Settings,
) -> (Vec<crate::Profile>, Vec<crate::settings::Credential>) {
    let mut profiles = merge_profiles3(&base.profiles, &ours.profiles, &theirs.profiles);
    let credentials = merge_credentials3(&base.credentials, &ours.credentials, &theirs.credentials);
    let has = |list: &[crate::settings::Credential], n: &str| {
        list.iter().any(|c| c.name.eq_ignore_ascii_case(n))
    };
    // Old name -> new name, as either side renamed.
    let mut renames: Vec<(String, String)> = Vec::new();
    for side in [ours, theirs] {
        for b in &base.profiles {
            let Some(old) = &b.credential else {
                continue;
            };
            let new = side
                .profiles
                .iter()
                .find(|p| p.host.eq_ignore_ascii_case(&b.host))
                .and_then(|p| p.credential.clone());
            if let Some(new) = new
                && !new.eq_ignore_ascii_case(old)
                && !has(&side.credentials, old)
                && has(&side.credentials, &new)
                // A rename makes a new name: one already in base was
                // there to pick, not what the old one became.
                && !has(&base.credentials, &new)
                && !renames.iter().any(|(o, _)| o.eq_ignore_ascii_case(old))
            {
                renames.push((old.clone(), new));
            }
        }
    }
    for p in &mut profiles {
        let Some(name) = &p.credential else {
            continue;
        };
        if has(&credentials, name) {
            continue;
        }
        // The credential is gone from the merge. First the field merges
        // on its own, as the profile does (a side that left it as in base
        // takes the other's): a side that took it off or picked another
        // is followed. Only a name still missing then follows a rename,
        // and one with none falls back to the connection's own login,
        // never a credential neither side gave it.
        let field = |list: &[crate::Profile]| {
            list.iter()
                .find(|q| q.host.eq_ignore_ascii_case(&p.host))
                .and_then(|q| q.credential.clone())
        };
        let (b, o, t) = (
            field(&base.profiles),
            field(&ours.profiles),
            field(&theirs.profiles),
        );
        let merged = if o == b { t } else { o };
        p.credential = match merged {
            Some(n) if has(&credentials, &n) => Some(n),
            Some(n) => renames
                .iter()
                .find(|(old, new)| old.eq_ignore_ascii_case(&n) && has(&credentials, new))
                .map(|(_, new)| new.clone()),
            None => None,
        };
    }
    (profiles, credentials)
}

/// A list without names (security rules, theme rules, saved searches)
/// after two PCs or windows changed it from the same `base`: an entry one
/// side removed (or changed, which is a removal and an addition) goes, an
/// entry either side added stays. So when both sides changed the same
/// rule, both versions stay: for security rules the stricter result, as
/// a rule can only tighten. In `theirs`'s order, with our new ones after.
pub fn merge_set3<T: PartialEq + Clone>(base: &[T], ours: &[T], theirs: &[T]) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    let mut keep = |x: &T| {
        if !out.contains(x) {
            out.push(x.clone());
        }
    };
    for x in theirs {
        // Taken out on our side since base.
        if base.contains(x) && !ours.contains(x) {
            continue;
        }
        keep(x);
    }
    for x in ours {
        // Taken out on their side since base.
        if base.contains(x) && !theirs.contains(x) {
            continue;
        }
        keep(x);
    }
    out
}

/// What two windows on one PC, or two PCs sharing a vault, keep in common
/// in `ferossh.conf`, merged from `base` (what `ours` started from) and
/// `theirs` (the file as it is now) into `ours`: host profiles and
/// credentials ([`merge_logins3`]), security rules, theme rules, saved
/// searches and login-script secrets. The rest (window, fonts, recent
/// connections) belongs to the window that saves.
pub fn merge_shared3(base: &Settings, ours: &mut Settings, theirs: &Settings) {
    let (profiles, credentials) = merge_logins3(base, ours, theirs);
    ours.profiles = profiles;
    ours.credentials = credentials;
    ours.security_rules = merge_set3(
        &base.security_rules,
        &ours.security_rules,
        &theirs.security_rules,
    );
    ours.theme_rules = merge_set3(&base.theme_rules, &ours.theme_rules, &theirs.theme_rules);
    ours.saved_searches = merge_set3(
        &base.saved_searches,
        &ours.saved_searches,
        &theirs.saved_searches,
    );
    ours.secrets = merge_secrets3(&base.secrets, &ours.secrets, &theirs.secrets);
}

/// Login-script secrets by name, as [`merge_credentials3`] does for
/// credentials: ours where we changed, added or removed one, else theirs.
fn merge_secrets3(
    base: &[crate::login::SecretSource],
    ours: &[crate::login::SecretSource],
    theirs: &[crate::login::SecretSource],
) -> Vec<crate::login::SecretSource> {
    let find = |list: &[crate::login::SecretSource], name: &str| {
        list.iter()
            .find(|x| x.name.eq_ignore_ascii_case(name))
            .cloned()
    };
    let mut names: Vec<String> = Vec::new();
    for x in theirs.iter().chain(ours).chain(base) {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&x.name)) {
            names.push(x.name.clone());
        }
    }
    names
        .iter()
        .filter_map(|n| {
            let (b, o) = (find(base, n), find(ours, n));
            if o == b { find(theirs, n) } else { o }
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
                same: 1,
                credentials: 0
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
                same: 1,
                credentials: 0
            }
        );
        assert_eq!(local.profiles.len(), 3);
    }

    #[test]
    fn a_profile_follows_a_credential_renamed_on_the_other_pc() {
        let base = Settings::parse(
            "Credential ops\n    User o\nProfile web\n    Credential ops\n    FontSize 10\n",
        );
        // Here: only the font size changed.
        let ours = Settings::parse(
            "Credential ops\n    User o\nProfile web\n    Credential ops\n    FontSize 12\n",
        );
        // There: ops renamed to prod, with its connection.
        let theirs = Settings::parse(
            "Credential prod\n    User o\nProfile web\n    Credential prod\n    FontSize 10\n",
        );
        // Both changed the profile: as always, the side merging keeps its
        // own, but never a name the credentials no longer hold.
        for (a, b, size) in [(&ours, &theirs, 12.0), (&theirs, &ours, 10.0)] {
            let (profiles, credentials) = merge_logins3(&base, a, b);
            assert_eq!(credentials.len(), 1);
            assert_eq!(credentials[0].name, "prod");
            let web = profiles.iter().find(|p| p.host == "web").unwrap();
            assert_eq!(web.credential.as_deref(), Some("prod"));
            assert_eq!(web.font_size, Some(size));
        }
    }

    #[test]
    fn a_merge_never_hands_a_profile_a_credential_neither_side_gave_it() {
        let base = Settings::parse(
            "Credential ops\n    User o\nCredential lab\n    User l\nProfile web\n    \
             Credential ops\nProfile db\n    Credential ops\n    FontSize 10\n",
        );
        // Here: only db's font size changed.
        let ours = Settings::parse(
            "Credential ops\n    User o\nCredential lab\n    User l\nProfile web\n    \
             Credential ops\nProfile db\n    Credential ops\n    FontSize 12\n",
        );
        // There: web moved to lab (already there), db's credential taken
        // off, and ops removed. Not a rename of ops to lab.
        let theirs = Settings::parse(
            "Credential lab\n    User l\nProfile web\n    Credential lab\nProfile db\n    \
             FontSize 10\n",
        );
        let (profiles, credentials) = merge_logins3(&base, &ours, &theirs);
        assert_eq!(credentials.len(), 1);
        let get = |h: &str| profiles.iter().find(|p| p.host == h).unwrap();
        assert_eq!(get("web").credential.as_deref(), Some("lab"));
        // db: credential as they left it (none), font size as we did.
        assert_eq!(get("db").credential, None);
        assert_eq!(get("db").font_size, Some(12.0));
    }

    #[test]
    fn a_new_credential_on_the_other_pc_is_not_a_rename_for_a_profile_taken_off() {
        let base = Settings::parse(
            "Credential ops\n    User o\nProfile web\n    Credential ops\nProfile db\n    \
             Credential ops\n    FontSize 10\n",
        );
        // Here: only db's font size changed.
        let ours = Settings::parse(
            "Credential ops\n    User o\nProfile web\n    Credential ops\nProfile db\n    \
             Credential ops\n    FontSize 12\n",
        );
        // There: ops removed, a new lab made for web, db taken off.
        let theirs = Settings::parse(
            "Credential lab\n    User l\nProfile web\n    Credential lab\nProfile db\n    \
             FontSize 10\n",
        );
        let (profiles, _) = merge_logins3(&base, &ours, &theirs);
        let get = |h: &str| profiles.iter().find(|p| p.host == h).unwrap();
        assert_eq!(get("web").credential.as_deref(), Some("lab"));
        assert_eq!(get("db").credential, None);
        assert_eq!(get("db").font_size, Some(12.0));
        // A profile only we made, naming ops: follows the rename to lab.
        let ours = Settings::parse(
            "Credential ops\n    User o\nProfile web\n    Credential ops\nProfile db\n    \
             Credential ops\n    FontSize 10\nProfile new\n    Credential ops\n",
        );
        let theirs = Settings::parse(
            "Credential lab\n    User o\nProfile web\n    Credential lab\nProfile db\n    \
             Credential lab\n    FontSize 10\n",
        );
        let (profiles, _) = merge_logins3(&base, &ours, &theirs);
        let get = |h: &str| profiles.iter().find(|p| p.host == h).unwrap();
        assert_eq!(get("new").credential.as_deref(), Some("lab"));
        assert_eq!(get("db").credential.as_deref(), Some("lab"));
    }

    #[test]
    fn credentials_merge_three_ways() {
        let parse = |t: &str| Settings::parse(t).credentials;
        let base = parse(
            "Credential ops\n    User old\nCredential gone\n    User x\nCredential mine\n    User a\n",
        );
        // Ours: changed mine, removed nothing; theirs: changed ops, removed gone, added lab.
        let ours = parse(
            "Credential ops\n    User old\nCredential gone\n    User x\nCredential mine\n    User b\nCredential local\n    User l\n",
        );
        let theirs = parse(
            "Credential OPS\n    User new\nCredential mine\n    User a\nCredential lab\n    User pi\n",
        );
        let out = merge_credentials3(&base, &ours, &theirs);
        let user = |n: &str| {
            out.iter()
                .find(|c| c.name.eq_ignore_ascii_case(n))
                .and_then(|c| c.user.clone())
        };
        assert_eq!(user("ops").as_deref(), Some("new"));
        assert_eq!(user("gone"), None);
        assert_eq!(user("mine").as_deref(), Some("b"));
        assert_eq!(user("lab").as_deref(), Some("pi"));
        assert_eq!(user("local").as_deref(), Some("l"));
        // A credential we removed stays removed.
        let ours = parse("Credential ops\n    User old\n");
        let theirs = parse("Credential ops\n    User old\nCredential mine\n    User a\n");
        let base = parse("Credential ops\n    User old\nCredential mine\n    User a\n");
        assert_eq!(merge_credentials3(&base, &ours, &theirs).len(), 1);
    }

    #[test]
    fn credentials_come_with_the_profiles() {
        let mut local = Settings::parse("Credential ops\n    User old\n");
        let incoming = Settings::parse(
            "Credential OPS\n    User new\nCredential lab\n    User pi\nProfile web\n    Credential lab\n",
        );
        let m = merge_profiles(&mut local, &incoming);
        assert_eq!(m.credentials, 2);
        assert!(m.changed());
        assert_eq!(local.credentials.len(), 2);
        assert_eq!(
            local.credential("ops").unwrap().user.as_deref(),
            Some("new")
        );
        assert_eq!(local.credential_of("web").unwrap().name, "lab");
        // Only a credential that changed counts.
        let only = Settings::parse("Credential lab\n    User pi\n");
        let m = merge_profiles(&mut local, &only);
        assert!(!m.changed());
        let changed = Settings::parse("Credential lab\n    User root\n");
        assert!(merge_profiles(&mut local, &changed).changed());
    }

    #[test]
    fn rules_from_both_sides_survive_a_save() {
        let rule = |tag: &str| {
            let mut r = crate::host_policy::SecurityRule::default();
            r.tags.push(tag.into());
            r.limits.keys_only = true;
            r
        };
        let base = Settings {
            security_rules: vec![rule("old"), rule("gone")],
            saved_searches: vec!["tag:a".into()],
            ..Settings::default()
        };
        // The other PC added a rule and removed one.
        let mut theirs = base.clone();
        theirs.security_rules = vec![rule("old"), rule("prod")];
        // This window, which never saw that, changed its font and added
        // a rule of its own.
        let mut ours = base.clone();
        ours.recent = vec!["web".into()];
        ours.security_rules.push(rule("lab"));
        ours.saved_searches.push("tag:b".into());
        merge_shared3(&base, &mut ours, &theirs);
        let tags: Vec<&str> = ours
            .security_rules
            .iter()
            .map(|r| r.tags[0].as_str())
            .collect();
        assert_eq!(tags, ["old", "prod", "lab"]);
        assert_eq!(ours.saved_searches, ["tag:a", "tag:b"]);
        assert_eq!(ours.recent, ["web"]);
    }

    #[test]
    fn a_rule_removed_or_changed_on_one_side_stays_so() {
        let base = vec![1, 2, 3];
        // We removed 2; they changed 3 into 4.
        assert_eq!(merge_set3(&base, &[1, 3], &[1, 2, 4]), [1, 4]);
        // Both changed 1, differently: both stay (only tighter).
        assert_eq!(merge_set3(&[1], &[5], &[6]), [6, 5]);
        // Nothing to merge from.
        assert_eq!(merge_set3::<u8>(&[], &[], &[7]), [7]);
    }

    #[test]
    fn secrets_merge_by_name() {
        let s = |n: &str, c: &str| crate::login::SecretSource {
            name: n.into(),
            command: c.into(),
        };
        let base = vec![s("a", "1"), s("b", "1")];
        let ours = vec![s("a", "2"), s("b", "1")];
        let theirs = vec![s("a", "1"), s("b", "3"), s("c", "1")];
        assert_eq!(
            merge_secrets3(&base, &ours, &theirs),
            [s("a", "2"), s("b", "3"), s("c", "1")]
        );
    }
}
