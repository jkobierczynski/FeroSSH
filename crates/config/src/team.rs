//! A team's shared connection library: a file an administrator exports
//! from FeroSSH (File > Import and export > Export team library), signs
//! with an SSH key and puts on a share everyone can read:
//!
//! ```text
//! ssh-keygen -Y sign -f admin_key -n ferossh-library team.ferossh-library
//! ```
//!
//! FeroSSH reads it only when `team.ferossh-library.sig` beside it is a
//! good signature by one of the keys named in the settings
//! (`TeamLibraryKey`). Its connections, their profiles and its security
//! and theme rules then come first, and the user cannot change them: a
//! local connection of the same name is not used, and local security
//! rules can only add limits to the team's.
//!
//! The file is a [`crate::bundle::Bundle`] with `sessions.conf`,
//! `ferossh.conf` and a `theme-N.conf` per custom theme its rules and
//! profiles name (the themes folder's file, so a PC without that file
//! still has the theme), and nothing secret: a library that holds a password
//! is refused. A connection may name a credential; each user keeps their
//! own credential of that name.

use russh::keys::PublicKey;
use russh::keys::ssh_key::SshSig;

use crate::bundle::Bundle;
use crate::host_policy::SecurityRule;
use crate::host_theme::ThemeRule;
use crate::password::PasswordSource;
use crate::saved::{self, SavedHost};
use crate::settings::{Profile, Settings};

/// The SSHSIG namespace a library is signed under, so a signature made
/// for anything else (a git commit, a file) does not pass.
pub const NAMESPACE: &str = "ferossh-library";

/// The folder the connection manager shows the team's connections in.
pub const FOLDER: &str = "Team";

/// The largest library read, in bytes.
pub const MAX_LEN: usize = 8 * 1024 * 1024;

/// The most custom themes a library brings.
pub const MAX_THEMES: usize = 50;

/// The largest theme file a library brings, in bytes (as the themes
/// folder reads them).
pub const MAX_THEME_LEN: usize = 64 * 1024;

/// The bundle file holding the library's [`Team::serial`].
const VERSION_FILE: &str = "library.conf";

/// The bundle file holding a library's theme `n` (from 1).
fn theme_file(n: usize) -> String {
    format!("theme-{n}.conf")
}

/// What a verified library gives.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Team {
    /// The team's connections, in `sessions.conf` syntax, each in the
    /// [`FOLDER`] folder (theirs below it).
    pub hosts_text: String,
    /// Their names.
    pub names: Vec<String>,
    /// Their profiles (only for the team's own connections).
    pub profiles: Vec<Profile>,
    pub security_rules: Vec<SecurityRule>,
    pub theme_rules: Vec<ThemeRule>,
    /// The custom themes it brings: a theme file's text each, read by
    /// the app as the themes folder's are.
    pub themes: Vec<String>,
    /// When it was exported, seconds since 1970, signed with the rest:
    /// a library older than one already checked is not used, so a share
    /// cannot be turned back to an earlier signed library (one from
    /// before a security rule was added, say). 0 for a library exported
    /// before FeroSSH wrote one.
    pub serial: u64,
    /// Read from a library whose signature was checked (or its checked
    /// copy); false for none at all. A machine policy that requires the
    /// library connects nowhere without one
    /// ([`crate::team_policy::check`]).
    pub verified: bool,
}

impl Team {
    /// Whether connection `name` is the team's.
    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
            && self.security_rules.is_empty()
            && self.theme_rules.is_empty()
            && self.themes.is_empty()
    }
}

/// Why a library was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// No `TeamLibraryKey` is set: nothing could be checked.
    NoKey,
    /// The `.sig` file does not hold an SSH signature.
    BadSignatureFile,
    /// The signature is by none of the keys set.
    UnknownSigner,
    /// The signature does not match the file (changed since it was
    /// signed), or was made for something else.
    BadSignature,
    /// The file is not a library FeroSSH wrote.
    NotALibrary,
    /// The file holds passwords, which a shared library must not.
    HoldsSecrets,
    TooLarge,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoKey => "no team library key is set in Settings, so its signature cannot be checked",
            Self::BadSignatureFile => "its .sig file is not an SSH signature",
            Self::UnknownSigner => "it is signed by a key that is not the team library key",
            Self::BadSignature => {
                "its signature does not match it (it changed after it was signed, or the signature is for something else)"
            }
            Self::NotALibrary => "it is not a team library exported by FeroSSH",
            Self::HoldsSecrets => "it holds saved passwords, which a team library must not",
            Self::TooLarge => "it is larger than 8 MB",
        })
    }
}

/// The keys in `lines` (OpenSSH public key lines) that parse.
pub fn keys(lines: &[String]) -> Vec<PublicKey> {
    lines
        .iter()
        .filter_map(|l| PublicKey::from_openssh(l.trim()).ok())
        .collect()
}

/// Checks `sig` (the armored `.sig` text) over `bytes` against `trusted`.
pub fn verify(bytes: &[u8], sig: &str, trusted: &[PublicKey]) -> Result<(), Refused> {
    if trusted.is_empty() {
        return Err(Refused::NoKey);
    }
    if bytes.len() > MAX_LEN {
        return Err(Refused::TooLarge);
    }
    let sig = SshSig::from_pem(sig.trim()).map_err(|_| Refused::BadSignatureFile)?;
    let Some(key) = trusted.iter().find(|k| k.key_data() == sig.public_key()) else {
        return Err(Refused::UnknownSigner);
    };
    key.verify(NAMESPACE, bytes, &sig)
        .map_err(|_| Refused::BadSignature)
}

/// The fingerprint (`SHA256:...`) of the key that made signature `sig`,
/// for a library [`verify`] accepted: what FeroSSH keeps the newest
/// library date per, whatever path the library is read from.
pub fn signer(sig: &str) -> Option<String> {
    let sig = SshSig::from_pem(sig.trim()).ok()?;
    Some(
        sig.public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string(),
    )
}

/// The fingerprints of the keys in `lines` (the `TeamLibraryKey` lines),
/// as [`signer`] gives them.
pub fn fingerprints(lines: &[String]) -> Vec<String> {
    keys(lines)
        .iter()
        .map(|k| k.fingerprint(russh::keys::HashAlg::Sha256).to_string())
        .collect()
}

/// Reads a library already verified.
pub fn parse(bytes: &[u8]) -> Result<Team, Refused> {
    if bytes.len() > MAX_LEN {
        return Err(Refused::TooLarge);
    }
    let bundle = Bundle::parse(bytes).ok_or(Refused::NotALibrary)?;
    if !bundle.secrets.is_empty() {
        return Err(Refused::HoldsSecrets);
    }
    let mut hosts: Vec<SavedHost> = Vec::new();
    for mut h in saved::parse(bundle.file("sessions.conf").unwrap_or_default()) {
        h.identity_file = portable_key(h.identity_file);
        if hosts.iter().any(|o| o.alias.eq_ignore_ascii_case(&h.alias)) {
            continue;
        }
        h.folder = Some(match h.folder.as_deref().map(|f| f.trim_matches('/')) {
            Some(f) if !f.is_empty() => format!("{FOLDER}/{f}"),
            _ => FOLDER.to_owned(),
        });
        hosts.push(h);
    }
    let names: Vec<String> = hosts.iter().map(|h| h.alias.clone()).collect();
    let settings = Settings::parse(bundle.file("ferossh.conf").unwrap_or_default());
    let profiles = settings
        .profiles
        .into_iter()
        .filter(|p| names.iter().any(|n| n.eq_ignore_ascii_case(&p.host)))
        .map(without_password_command)
        .collect();
    // The team's connections sit in the Team folder now: a rule for
    // folder `Production` is for `Team/Production` as well.
    let security_rules = settings
        .security_rules
        .into_iter()
        .map(|mut r| {
            let inside: Vec<String> = r
                .folders
                .iter()
                .map(|f| format!("{FOLDER}/{}", f.trim_matches('/')))
                .collect();
            r.folders.extend(inside);
            r
        })
        .collect();
    // Numbered from 1 with no gaps, as export writes them; one too big
    // is left out (the PC's own file of that name, if any, is used).
    let themes = (1..=MAX_THEMES)
        .map_while(|n| bundle.file(&theme_file(n)))
        .filter(|t| t.len() <= MAX_THEME_LEN)
        .map(str::to_owned)
        .collect();
    Ok(Team {
        serial: bundle.file(VERSION_FILE).map_or(0, read_serial),
        hosts_text: saved::to_text(&hosts),
        names,
        profiles,
        security_rules,
        theme_rules: settings.theme_rules,
        themes,
        verified: true,
    })
}

/// The `Serial` line of a library's [`VERSION_FILE`]; 0 when there is
/// none that reads.
fn read_serial(text: &str) -> u64 {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("Serial "))
        .find_map(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// A connection's key file as a team library may carry it: only one in
/// the user's own home folder (`~/.ssh/id_team`), which names each
/// person's own file. A full path (`C:\Users\admin\.ssh\id_ed25519`,
/// a share) names the exporting PC's file and its account, and on another
/// PC some other file, so it is left out of an export and ignored in a
/// library; each person's credential or ssh_config gives their key.
fn portable_key(key: Option<String>) -> Option<String> {
    key.filter(|k| {
        let k = k.trim();
        (k.starts_with("~/") || k.starts_with("~\\")) && !k.contains("..")
    })
}

/// `p` without a password manager command, which could hold a password
/// itself (`echo ...`): each user sets their own, or a named credential.
fn without_password_command(mut p: Profile) -> Profile {
    if matches!(p.password, PasswordSource::Command(_)) {
        p.password = PasswordSource::Ask;
    }
    p
}

/// Verifies and reads a library: the file's bytes and its `.sig` text.
pub fn load(bytes: &[u8], sig: &str, trusted: &[PublicKey]) -> Result<Team, Refused> {
    verify(bytes, sig, trusted)?;
    parse(bytes)
}

/// The library to export from `sessions` (`sessions.conf` text) and
/// `settings`: every saved connection with its profile, and the security
/// and theme rules. No password, credential or other setting goes in.
///
/// `themes` are the custom themes on this PC, as (name, theme file
/// text): those a theme rule or an exported profile names go in, at
/// most [`MAX_THEMES`]. `serial` is the export's time, seconds since
/// 1970 (see [`Team::serial`]).
pub fn export(
    sessions: &str,
    settings: &Settings,
    themes: &[(String, String)],
    serial: u64,
) -> Vec<u8> {
    let hosts: Vec<SavedHost> = saved::parse(sessions)
        .into_iter()
        .map(|mut h| {
            h.identity_file = portable_key(h.identity_file);
            h
        })
        .collect();
    let part = Settings {
        profiles: settings
            .profiles
            .iter()
            .filter(|p| hosts.iter().any(|h| h.alias.eq_ignore_ascii_case(&p.host)))
            .cloned()
            .map(without_password_command)
            .collect(),
        security_rules: settings.security_rules.clone(),
        theme_rules: settings.theme_rules.clone(),
        ..Settings::default()
    };
    let mut bundle = Bundle::default();
    bundle.set_file("sessions.conf", saved::to_text(&hosts));
    bundle.set_file("ferossh.conf", part.to_text());
    bundle.set_file(VERSION_FILE, format!("Serial {serial}\n"));
    for (n, (_, text)) in (1..).zip(used_themes(&part, themes)) {
        bundle.set_file(&theme_file(n), text.clone());
    }
    bundle.to_bytes().to_vec()
}

/// The themes of `themes` that `part`'s theme rules or profiles name,
/// at most [`MAX_THEMES`], each once.
pub fn used_themes<'a>(
    part: &Settings,
    themes: &'a [(String, String)],
) -> Vec<&'a (String, String)> {
    let named: Vec<&str> = part
        .theme_rules
        .iter()
        .map(|r| r.theme.as_str())
        .chain(part.profiles.iter().filter_map(|p| p.theme.as_deref()))
        .collect();
    let mut used: Vec<&(String, String)> = Vec::new();
    for t in themes {
        if named
            .iter()
            .any(|n| n.trim().eq_ignore_ascii_case(t.0.trim()))
            && t.1.len() <= MAX_THEME_LEN
            && !used.iter().any(|u| u.0.eq_ignore_ascii_case(&t.0))
            && used.len() < MAX_THEMES
        {
            used.push(t);
        }
    }
    used
}

/// The names of the custom themes the library from `sessions` and
/// `settings` would name but `themes` does not have: a PC without them
/// falls back to its default look for those rules.
pub fn missing_themes(
    sessions: &str,
    settings: &Settings,
    themes: &[(String, String)],
    builtin: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let hosts = saved::parse(sessions);
    let mut out: Vec<String> = Vec::new();
    let named = settings.theme_rules.iter().map(|r| r.theme.as_str()).chain(
        settings
            .profiles
            .iter()
            .filter(|p| hosts.iter().any(|h| h.alias.eq_ignore_ascii_case(&p.host)))
            .filter_map(|p| p.theme.as_deref()),
    );
    for n in named.map(str::trim).filter(|n| !n.is_empty()) {
        if !builtin(n)
            && !themes.iter().any(|t| t.0.trim().eq_ignore_ascii_case(n))
            && !out.iter().any(|o| o.eq_ignore_ascii_case(n))
        {
            out.push(n.to_owned());
        }
    }
    out
}

/// Leaves out of `sessions` (`sessions.conf` text) the connections the
/// team has: the team's own are used instead.
pub fn without_team_hosts(sessions: &str, team: &Team) -> String {
    if team.names.is_empty() {
        return sessions.to_owned();
    }
    let hosts = saved::parse(sessions);
    if !hosts.iter().any(|h| team.has(&h.alias)) {
        return sessions.to_owned();
    }
    let kept: Vec<SavedHost> = hosts.into_iter().filter(|h| !team.has(&h.alias)).collect();
    saved::to_text(&kept)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::bundle::Secret;
    use russh::keys::HashAlg;
    use russh::keys::ssh_key::LineEnding;
    use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData, PrivateKey};

    fn key(seed: u8) -> PrivateKey {
        let pair = Ed25519Keypair::from_seed(&[seed; 32]);
        PrivateKey::new(KeypairData::Ed25519(pair), "admin").unwrap()
    }

    fn sign(k: &PrivateKey, namespace: &str, bytes: &[u8]) -> String {
        SshSig::sign(k, namespace, HashAlg::Sha512, bytes)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap()
    }

    fn library() -> Vec<u8> {
        let sessions = "Host db\n    HostName db.example\n\n# folder: Web\nHost web\n    HostName web.example\n    User admin\n";
        let settings = Settings::parse(
            "Profile web\n    Tags prod\n    Credential ops\n    PasswordCommand echo hunter2\n\
             Profile elsewhere\n    Tags x\n\
             SecurityRule\n    Tags prod\n    KeysOnly yes\n\
             SecurityRule\n    Folders Web\n    LogSession yes\n\
             ThemeRule\n    Tags prod\n    Theme Production\n\
             ThemeRule\n    Tags db\n    Theme Sandstorm\n\
             Credential ops\n    User someone\n",
        );
        let themes = [
            (
                "Sandstorm".to_owned(),
                "Name Sandstorm\nBackground #2E3440\n".to_owned(),
            ),
            ("Unused".to_owned(), "Name Unused\n".to_owned()),
        ];
        export(sessions, &settings, &themes, 1_700_000_000)
    }

    #[test]
    fn a_signed_library_loads_into_the_team_folder() {
        let admin = key(1);
        let bytes = library();
        let sig = sign(&admin, NAMESPACE, &bytes);
        let team = load(&bytes, &sig, &[admin.public_key().clone()]).unwrap();
        assert_eq!(team.names, ["db", "web"]);
        assert!(team.hosts_text.contains("# folder: Team/Web"));
        assert!(team.hosts_text.contains("# folder: Team\n"));
        assert_eq!(team.profiles.len(), 1, "only the team's connections");
        assert_eq!(team.profiles[0].credential.as_deref(), Some("ops"));
        assert!(team.security_rules[0].limits.keys_only);
        assert_eq!(team.theme_rules[0].theme, "Production");
        // The custom theme a rule names travels; one nothing names does not.
        assert_eq!(team.themes, ["Name Sandstorm\nBackground #2E3440\n"]);
        assert!(team.has("WEB"));
        assert_eq!(team.serial, 1_700_000_000);
        // No credential travels, only its name.
        assert!(!String::from_utf8_lossy(&bytes).contains("someone"));
        // Nor a password command, which could hold a password.
        assert!(!String::from_utf8_lossy(&bytes).contains("hunter2"));
        assert_eq!(team.profiles[0].password, PasswordSource::Ask);
        // A folder rule still finds the team's folder of that name.
        assert!(
            team.security_rules[1]
                .folders
                .iter()
                .any(|f| f == "Team/Web")
        );
    }

    #[test]
    fn a_bad_or_foreign_signature_is_refused() {
        let admin = key(1);
        let other = key(2);
        let bytes = library();
        let trusted = [admin.public_key().clone()];
        let good = sign(&admin, NAMESPACE, &bytes);
        assert_eq!(load(&bytes, &good, &[]), Err(Refused::NoKey));
        assert_eq!(
            load(&bytes, &sign(&other, NAMESPACE, &bytes), &trusted),
            Err(Refused::UnknownSigner)
        );
        assert_eq!(
            load(&bytes, &sign(&admin, "git", &bytes), &trusted),
            Err(Refused::BadSignature)
        );
        let mut changed = bytes.clone();
        if let Some(b) = changed.iter_mut().find(|b| **b == b'w') {
            *b = b'v';
        }
        assert_eq!(load(&changed, &good, &trusted), Err(Refused::BadSignature));
        assert_eq!(
            load(&bytes, "not a signature", &trusted),
            Err(Refused::BadSignatureFile)
        );
        let junk = b"hello".to_vec();
        assert_eq!(
            load(&junk, &sign(&admin, NAMESPACE, &junk), &trusted),
            Err(Refused::NotALibrary)
        );
    }

    #[test]
    fn a_library_with_passwords_is_refused() {
        let mut bundle = Bundle::default();
        bundle.set_file("sessions.conf", "Host a\n".into());
        bundle.set_secret(Secret {
            target: "FeroSSH:a".into(),
            user: "u".into(),
            password: zeroize::Zeroizing::new("p".into()),
        });
        assert_eq!(parse(&bundle.to_bytes()), Err(Refused::HoldsSecrets));
    }

    #[test]
    fn themes_named_by_profiles_travel_and_missing_ones_are_listed() {
        let sessions = "Host web\n    HostName web.example\n";
        let settings = Settings::parse(
            "Profile web\n    Theme \"My Blue\"\n\
             Profile other\n    Theme Hidden\n\
             ThemeRule\n    Tags x\n    Theme Gone\n\
             ThemeRule\n    Tags y\n    Theme Nord\n",
        );
        let themes = [
            ("my blue".to_owned(), "Name my blue\n".to_owned()),
            ("Hidden".to_owned(), "Name Hidden\n".to_owned()),
            ("Big".to_owned(), "x".repeat(MAX_THEME_LEN + 1)),
        ];
        let team = parse(&export(sessions, &settings, &themes, 0)).unwrap();
        // Not Hidden: its profile is for a connection not exported.
        assert_eq!(team.themes, ["Name my blue\n"]);
        let builtin = |n: &str| n == "Nord";
        assert_eq!(
            missing_themes(sessions, &settings, &themes, &builtin),
            ["Gone"]
        );
    }

    #[test]
    fn library_themes_stop_at_a_gap_and_skip_big_ones() {
        let mut bundle = Bundle::default();
        bundle.set_file("sessions.conf", "Host a\n".into());
        bundle.set_file("theme-1.conf", "Name A\n".into());
        bundle.set_file("theme-2.conf", "x".repeat(MAX_THEME_LEN + 1));
        bundle.set_file("theme-3.conf", "Name C\n".into());
        bundle.set_file("theme-5.conf", "Name E\n".into());
        let team = parse(&bundle.to_bytes()).unwrap();
        assert_eq!(team.themes, ["Name A\n", "Name C\n"]);
    }

    #[test]
    fn only_home_relative_key_files_travel() {
        let sessions = "Host a\n    IdentityFile C:\\Users\\admin\\.ssh\\id_ed25519\n\n\
                        Host b\n    IdentityFile \\\\server\\keys\\id\n\n\
                        Host c\n    IdentityFile ~/.ssh/id_team\n\n\
                        Host d\n    IdentityFile ~/../admin/.ssh/id\n";
        let bytes = export(sessions, &Settings::default(), &[], 1);
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("admin") && !text.contains("server"));
        assert!(text.contains("~/.ssh/id_team"));
        // A library made by hand with a full path: ignored on reading.
        let mut bundle = Bundle::default();
        bundle.set_file(
            "sessions.conf",
            "Host a\n    IdentityFile C:\\Users\\admin\\.ssh\\id\n".into(),
        );
        let team = parse(&bundle.to_bytes()).unwrap();
        assert!(!team.hosts_text.contains("admin"));
        assert!(team.has("a"));
    }

    #[test]
    fn signer_names_the_signing_key() {
        let admin = key(1);
        let sig = sign(&admin, NAMESPACE, b"x");
        assert_eq!(
            signer(&sig).unwrap(),
            admin.public_key().fingerprint(HashAlg::Sha256).to_string()
        );
        assert_eq!(signer("junk"), None);
        let line = admin.public_key().to_openssh().unwrap();
        assert_eq!(fingerprints(&[line]), [signer(&sig).unwrap()]);
    }

    #[test]
    fn keys_read_openssh_lines() {
        let admin = key(1);
        let line = admin.public_key().to_openssh().unwrap();
        assert_eq!(keys(&[line, "junk".into()]).len(), 1);
    }

    #[test]
    fn local_connections_of_a_team_name_are_left_out() {
        let team = parse(&library()).unwrap();
        let local = "Host web\n    HostName evil.example\n\nHost mine\n    HostName mine.example\n";
        let kept = without_team_hosts(local, &team);
        assert!(!kept.contains("evil"));
        assert!(kept.contains("mine.example"));
    }
}
