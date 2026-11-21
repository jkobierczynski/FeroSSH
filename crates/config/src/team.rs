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
//! The file is a [`crate::bundle::Bundle`] with `sessions.conf` and
//! `ferossh.conf`, and nothing secret: a library that holds a password
//! is refused. A connection may name a credential; each user keeps their
//! own credential of that name.

use russh::keys::PublicKey;
use russh::keys::ssh_key::SshSig;

use crate::bundle::Bundle;
use crate::host_policy::SecurityRule;
use crate::host_theme::ThemeRule;
use crate::saved::{self, SavedHost};
use crate::settings::{Profile, Settings};

/// The SSHSIG namespace a library is signed under, so a signature made
/// for anything else (a git commit, a file) does not pass.
pub const NAMESPACE: &str = "ferossh-library";

/// The folder the connection manager shows the team's connections in.
pub const FOLDER: &str = "Team";

/// The largest library read, in bytes.
pub const MAX_LEN: usize = 8 * 1024 * 1024;

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
}

impl Team {
    /// Whether connection `name` is the team's.
    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.security_rules.is_empty() && self.theme_rules.is_empty()
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
        .collect();
    Ok(Team {
        hosts_text: saved::to_text(&hosts),
        names,
        profiles,
        security_rules: settings.security_rules,
        theme_rules: settings.theme_rules,
    })
}

/// Verifies and reads a library: the file's bytes and its `.sig` text.
pub fn load(bytes: &[u8], sig: &str, trusted: &[PublicKey]) -> Result<Team, Refused> {
    verify(bytes, sig, trusted)?;
    parse(bytes)
}

/// The library to export from `sessions` (`sessions.conf` text) and
/// `settings`: every saved connection with its profile, and the security
/// and theme rules. No password, credential or other setting goes in.
pub fn export(sessions: &str, settings: &Settings) -> Vec<u8> {
    let hosts = saved::parse(sessions);
    let part = Settings {
        profiles: settings
            .profiles
            .iter()
            .filter(|p| hosts.iter().any(|h| h.alias.eq_ignore_ascii_case(&p.host)))
            .cloned()
            .collect(),
        security_rules: settings.security_rules.clone(),
        theme_rules: settings.theme_rules.clone(),
        ..Settings::default()
    };
    let mut bundle = Bundle::default();
    bundle.set_file("sessions.conf", saved::to_text(&hosts));
    bundle.set_file("ferossh.conf", part.to_text());
    bundle.to_bytes().to_vec()
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
            "Profile web\n    Tags prod\n    Credential ops\nProfile elsewhere\n    Tags x\n\
             SecurityRule\n    Tags prod\n    KeysOnly yes\n\
             ThemeRule\n    Tags prod\n    Theme Production\n\
             Credential ops\n    User someone\n",
        );
        export(sessions, &settings)
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
        assert!(team.has("WEB"));
        // No credential travels, only its name.
        assert!(!String::from_utf8_lossy(&bytes).contains("someone"));
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
