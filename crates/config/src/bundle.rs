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

/// What an import changes in one connection, worth reading before saying
/// yes: a changed address, route or host key check, forwarding turned on,
/// and what the import left as it was here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Review {
    pub name: String,
    pub notes: Vec<String>,
}

/// Adds `note` to the review for `name` in `reviews`.
fn review(reviews: &mut Vec<Review>, name: &str, note: String) {
    match reviews
        .iter_mut()
        .find(|r| r.name.eq_ignore_ascii_case(name))
    {
        Some(r) => r.notes.push(note),
        None => reviews.push(Review {
            name: name.to_owned(),
            notes: vec![note],
        }),
    }
}

/// The keywords of a `Host` block whose change an import shows: where the
/// connection goes, how it gets there and how its host key is checked.
const WATCHED: [&str; 11] = [
    "hostname",
    "port",
    "user",
    "proxyjump",
    "proxycommand",
    "forwardagent",
    "forwardx11",
    "forwardx11trusted",
    "userknownhostsfile",
    "globalknownhostsfile",
    "identityfile",
];

/// A block's first value for each keyword in [`WATCHED`] (ssh_config
/// takes the first), lowercase keyword to the rest of the line.
fn watched(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines().skip_while(|l| !is_host_line(l)).skip(1) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((k, v)) = keyword_value(t) else {
            continue;
        };
        if k == "match" {
            break;
        }
        if WATCHED.contains(&k.as_str()) && !out.iter().any(|(x, _)| *x == k) {
            out.push((k, v));
        }
    }
    out
}

fn is_host_line(line: &str) -> bool {
    keyword_value(line.trim()).is_some_and(|(k, _)| k == "host")
}

/// A config line's keyword (lowercase) and the rest, `=` or spaces
/// between.
fn keyword_value(line: &str) -> Option<(String, String)> {
    let end = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let (k, rest) = line.split_at(end);
    let v = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    Some((k.to_ascii_lowercase(), v.trim().to_owned()))
}

/// Whether a `StrictHostKeyChecking` value trusts a new key without
/// asking.
fn trusts_any_key(value: &str) -> bool {
    let v = value
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(v.as_str(), "no" | "off" | "accept-new")
}

/// `incoming`'s `Host` blocks for an import into `local`
/// ([`merge_hosts`] after this), made safe and with what changes said:
///
/// - a `StrictHostKeyChecking` that trusts new keys without asking is
///   left out, unless `local`'s block of the name has that very line;
/// - a block replacing one of ours keeps our folder (the comments right
///   above its `Host` line);
/// - for a block replacing one of ours, each changed address, route,
///   forwarding, key file or known_hosts file is named.
pub fn import_hosts(local: &str, incoming: &str) -> (String, Vec<Review>) {
    let (_, ours) = host_blocks(local);
    let (in_head, theirs) = host_blocks(incoming);
    let mut reviews = Vec::new();
    let mut out = in_head;
    for b in theirs {
        let name = b.key.clone();
        let mine = ours.iter().find(|o| o.key == b.key);
        let my_lines: Vec<String> = mine
            .map(|m| m.text.lines().map(|l| l.trim().to_owned()).collect())
            .unwrap_or_default();
        let mut text = String::new();
        let mut in_host = false;
        for line in b.text.lines() {
            if is_host_line(line) {
                in_host = true;
            }
            if in_host
                && let Some((k, v)) = keyword_value(line.trim())
                && k == "stricthostkeychecking"
                && trusts_any_key(&v)
                && !my_lines.iter().any(|l| l == line.trim())
            {
                review(
                    &mut reviews,
                    &name,
                    format!(
                        "left out \"StrictHostKeyChecking {v}\": new host keys are asked about"
                    ),
                );
                continue;
            }
            text.push_str(line);
            text.push('\n');
        }
        if let Some(m) = mine {
            // Our folder (and comments) above the Host line stay.
            let head = |t: &str| -> String {
                t.lines()
                    .take_while(|l| !is_host_line(l))
                    .map(|l| format!("{l}\n"))
                    .collect()
            };
            let (my_head, their_head) = (head(&m.text), head(&text));
            if my_head != their_head {
                let body: String = text
                    .lines()
                    .skip_while(|l| !is_host_line(l))
                    .map(|l| format!("{l}\n"))
                    .collect();
                text = format!("{my_head}{body}");
            }
            let (before, after) = (watched(&m.text), watched(&text));
            for k in WATCHED {
                let old = before.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str());
                let new = after.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str());
                if old != new {
                    let shown = |v: Option<&str>| {
                        v.map_or_else(|| "(not set)".to_owned(), |v| format!("\"{v}\""))
                    };
                    review(
                        &mut reviews,
                        &name,
                        format!("{} {} -> {}", display_keyword(k), shown(old), shown(new)),
                    );
                }
            }
        }
        out.push_str(&text);
    }
    (out, reviews)
}

/// `keyword` as ssh_config documents it.
fn display_keyword(keyword: &str) -> &'static str {
    match keyword {
        "hostname" => "HostName",
        "port" => "Port",
        "user" => "User",
        "proxyjump" => "ProxyJump",
        "proxycommand" => "ProxyCommand",
        "forwardagent" => "ForwardAgent",
        "forwardx11" => "ForwardX11",
        "forwardx11trusted" => "ForwardX11Trusted",
        "userknownhostsfile" => "UserKnownHostsFile",
        "globalknownhostsfile" => "GlobalKnownHostsFile",
        "identityfile" => "IdentityFile",
        _ => "",
    }
}

/// [`merge_profiles`] for an import: a connection's own tags here stay
/// (the file's are added), and so does a Production or Staging theme, so
/// an import never takes a connection out of a security or theme rule's
/// reach. What else a replaced profile changes that matters is named.
pub fn import_profiles(local: &mut Settings, incoming: &Settings) -> (Merged, Vec<Review>) {
    let before = local.profiles.clone();
    let merged = merge_profiles(local, incoming);
    let mut reviews = Vec::new();
    for p in &mut local.profiles {
        let Some(old) = before.iter().find(|b| b.host.eq_ignore_ascii_case(&p.host)) else {
            continue;
        };
        if old == p {
            continue;
        }
        let dropped: Vec<String> = old
            .tags
            .iter()
            .filter(|t| !p.tags.iter().any(|n| n.eq_ignore_ascii_case(t)))
            .cloned()
            .collect();
        if !dropped.is_empty() {
            p.tags.extend(dropped.iter().cloned());
            review(
                &mut reviews,
                &p.host,
                format!("kept your tags {}", dropped.join(" ")),
            );
        }
        let warning = |t: &Option<String>| {
            t.as_deref().is_some_and(|t| {
                t.eq_ignore_ascii_case("Production") || t.eq_ignore_ascii_case("Staging")
            })
        };
        if warning(&old.theme) && p.theme != old.theme {
            p.theme.clone_from(&old.theme);
            review(
                &mut reviews,
                &p.host,
                format!(
                    "kept your {} theme",
                    old.theme.as_deref().unwrap_or_default()
                ),
            );
        }
        if p.winrm_ntlm && !old.winrm_ntlm {
            review(&mut reviews, &p.host, "allows NTLM over plain HTTP".into());
        }
        let checks: [(&str, bool); 5] = [
            ("its password source", p.password != old.password),
            ("its named credential", p.credential != old.credential),
            ("its Via SSH connection", p.via != old.via),
            ("its login script", p.login_script != old.login_script),
            ("its remote app", p.remote_app != old.remote_app),
        ];
        for (what, changed) in checks {
            if changed {
                review(&mut reviews, &p.host, format!("changes {what}"));
            }
        }
    }
    (merged, reviews)
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
    fn an_import_names_what_it_changes_and_keeps_safety() {
        let local = "# folder: Prod\nHost db1\n    HostName db1.example.com\n    User ops\n\n\
                     Host lab\n    HostName lab\n    StrictHostKeyChecking no\n";
        let incoming = "# folder: Misc\nHost db1\n    HostName 10.6.6.6\n    User ops\n    \
                        StrictHostKeyChecking accept-new\n    ForwardAgent yes\n\n\
                        Host lab\n    HostName lab\n    StrictHostKeyChecking no\n\n\
                        Host new\n    StrictHostKeyChecking=off\n";
        let (text, reviews) = import_hosts(local, incoming);
        // The host key check that trusts any key is left out, except
        // where ours already had that very line.
        assert!(!text.contains("accept-new"));
        assert!(!text.contains("=off"));
        assert!(text.contains("Host lab\n    HostName lab\n    StrictHostKeyChecking no\n"));
        // Our folder stays.
        assert!(text.contains("# folder: Prod\nHost db1\n"));
        assert!(!text.contains("Misc"));
        let db1 = reviews.iter().find(|r| r.name == "db1").unwrap();
        assert!(db1.notes.iter().any(|n| n.starts_with("left out")));
        assert!(
            db1.notes
                .contains(&"HostName \"db1.example.com\" -> \"10.6.6.6\"".to_owned())
        );
        assert!(
            db1.notes
                .contains(&"ForwardAgent (not set) -> \"yes\"".to_owned())
        );
        assert!(!reviews.iter().any(|r| r.name == "lab"));
        assert!(reviews.iter().any(|r| r.name == "new"));
        // And it merges as before.
        let (merged, m) = merge_hosts(local, &text);
        assert_eq!((m.added, m.replaced, m.same), (1, 1, 1));
        assert!(merged.contains("HostName 10.6.6.6"));
    }

    #[test]
    fn an_import_keeps_our_tags_and_warning_theme() {
        let mine = crate::Profile {
            host: "db1".into(),
            tags: vec!["prod".into()],
            theme: Some("Production".into()),
            ..crate::Profile::default()
        };
        let mut local = Settings {
            profiles: vec![mine],
            ..Settings::default()
        };
        let incoming = Settings {
            profiles: vec![crate::Profile {
                host: "DB1".into(),
                tags: vec!["web".into()],
                theme: Some("Nord".into()),
                winrm_ntlm: true,
                ..crate::Profile::default()
            }],
            ..Settings::default()
        };
        let (m, reviews) = import_profiles(&mut local, &incoming);
        assert_eq!(m.replaced, 1);
        let p = &local.profiles[0];
        assert_eq!(p.tags, ["web", "prod"]);
        assert_eq!(p.theme.as_deref(), Some("Production"));
        assert_eq!(reviews.len(), 1);
        assert_eq!(
            reviews[0].notes,
            [
                "kept your tags prod",
                "kept your Production theme",
                "allows NTLM over plain HTTP"
            ]
        );
    }
}
