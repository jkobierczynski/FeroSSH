//! The team library set by machine policy: an administrator puts the
//! library's path and the keys it must be signed with under
//! `HKLM\Software\Policies\FeroSSH` (Group Policy or Intune, with the ADMX
//! template in `docs/admin/policy`), which a user who is not an
//! administrator of the PC cannot change.
//!
//! While it is set, the user's own `TeamLibrary` and `TeamLibraryKey`
//! settings are not used, wherever the settings file is (a portable
//! `FeroSSH-data` folder included): the library and its rules cannot be
//! turned off or swapped for another. When no good library can be read,
//! not even the checked copy, FeroSSH connects nowhere
//! (`RequireTeamLibrary`, on unless set to 0), and with
//! `OnlyTeamConnections` it connects only to the library's connections,
//! exactly as the library defines them.
//!
//! This only holds on a PC whose users are not its administrators: an
//! administrator can remove the policy, and anyone can run another SSH
//! client unless application control (AppLocker, WDAC) stops them.

use crate::team::Team;

/// The policy's registry key, under `HKEY_LOCAL_MACHINE`.
pub const KEY: &str = r"Software\Policies\FeroSSH";

/// The policy values, as the ADMX template writes them.
pub const LIBRARY: &str = "TeamLibrary";
/// A subkey with one REG_SZ value per key (an ADMX list), or a
/// REG_MULTI_SZ value of this name in [`KEY`].
pub const KEYS: &str = "TrustedKeys";
pub const REQUIRE: &str = "RequireTeamLibrary";
pub const ONLY_TEAM: &str = "OnlyTeamConnections";

/// What the machine policy says about the team library.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamPolicy {
    /// The library's path; `None` when the policy names none, which
    /// leaves no library to read (and so, while [`Self::require_library`],
    /// no connection either).
    pub library: Option<String>,
    /// The keys the library must be signed with, OpenSSH public key lines.
    pub keys: Vec<String>,
    /// Connect nowhere without a good library.
    pub require_library: bool,
    /// Connect only to the library's connections.
    pub only_team_connections: bool,
}

impl TeamPolicy {
    /// The policy from the values read under [`KEY`]; `None` when none of
    /// them is set (no policy: the user's own settings are used).
    ///
    /// `keys` may hold several keys in one entry, a line each.
    pub fn from_values(
        library: Option<String>,
        keys: Vec<String>,
        require: Option<u32>,
        only_team: Option<u32>,
    ) -> Option<Self> {
        let library = library
            .map(|l| l.trim().trim_matches('"').trim().to_owned())
            .filter(|l| !l.is_empty());
        let mut lines: Vec<String> = Vec::new();
        for k in keys.iter().flat_map(|k| k.lines()) {
            let k = k.trim();
            if !k.is_empty() && !lines.iter().any(|l| l == k) {
                lines.push(k.to_owned());
            }
        }
        if library.is_none() && lines.is_empty() && require.is_none() && only_team.is_none() {
            return None;
        }
        Some(Self {
            library,
            keys: lines,
            // On unless the administrator turns it off: a policy whose
            // library cannot be read must not leave FeroSSH open.
            require_library: require != Some(0),
            only_team_connections: only_team.is_some_and(|v| v != 0),
        })
    }
}

/// Why the policy stops a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// No good library could be read, nor a checked copy.
    NoLibrary,
    /// Connection (or jump host) `.0` is not one of the library's.
    NotTeam(String),
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLibrary => f.write_str(
                "Your administrator requires the team library, and FeroSSH could not read a \
                 good copy of it, so it connects nowhere until it can. Check that the share \
                 is reachable, then press Refresh in the connection manager.",
            ),
            Self::NotTeam(name) => write!(
                f,
                "Your administrator allows only the team library's connections, and {name} \
                 is not one of them. Pick a connection from the Team folder."
            ),
        }
    }
}

/// Whether `policy` lets FeroSSH connect to `names` (the connection, then
/// each jump host on the way) while `team` is the library in use (see
/// [`Team::verified`]).
pub fn check(policy: Option<&TeamPolicy>, team: &Team, names: &[&str]) -> Result<(), Blocked> {
    let Some(policy) = policy else {
        return Ok(());
    };
    if !team.verified && (policy.require_library || policy.only_team_connections) {
        return Err(Blocked::NoLibrary);
    }
    if policy.only_team_connections
        && let Some(n) = names.iter().find(|n| !team.has(n))
    {
        return Err(Blocked::NotTeam((*n).to_owned()));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn team() -> Team {
        Team {
            names: vec!["web".into(), "bastion".into()],
            verified: true,
            ..Team::default()
        }
    }

    #[test]
    fn no_values_is_no_policy() {
        assert_eq!(TeamPolicy::from_values(None, vec![], None, None), None);
        assert_eq!(
            TeamPolicy::from_values(Some("  ".into()), vec![" ".into()], None, None),
            None
        );
    }

    #[test]
    fn values_read_with_safe_defaults() {
        let p = TeamPolicy::from_values(
            Some(r#" "\\srv\team\team.ferossh-library" "#.into()),
            vec![
                "ssh-ed25519 AAAA a\r\nssh-ed25519 BBBB b".into(),
                "ssh-ed25519 AAAA a".into(),
            ],
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            p.library.as_deref(),
            Some(r"\\srv\team\team.ferossh-library")
        );
        assert_eq!(p.keys, ["ssh-ed25519 AAAA a", "ssh-ed25519 BBBB b"]);
        assert!(p.require_library && !p.only_team_connections);
        let off = TeamPolicy::from_values(None, vec![], Some(0), Some(1)).unwrap();
        assert!(!off.require_library && off.only_team_connections);
        assert!(off.library.is_none());
    }

    #[test]
    fn no_policy_allows_everything() {
        assert_eq!(check(None, &Team::default(), &["anything"]), Ok(()));
    }

    #[test]
    fn fails_closed_without_a_library() {
        let p = TeamPolicy::from_values(Some("x".into()), vec![], None, None).unwrap();
        assert_eq!(
            check(Some(&p), &Team::default(), &["web"]),
            Err(Blocked::NoLibrary)
        );
        assert_eq!(check(Some(&p), &team(), &["elsewhere"]), Ok(()));
        // Turned off: open without one.
        let open = TeamPolicy::from_values(Some("x".into()), vec![], Some(0), None).unwrap();
        assert_eq!(check(Some(&open), &Team::default(), &["web"]), Ok(()));
        // Team connections only needs the library whatever the other says.
        let only = TeamPolicy::from_values(Some("x".into()), vec![], Some(0), Some(1)).unwrap();
        assert_eq!(
            check(Some(&only), &Team::default(), &["web"]),
            Err(Blocked::NoLibrary)
        );
    }

    #[test]
    fn only_team_connections_checks_every_hop() {
        let p = TeamPolicy::from_values(Some("x".into()), vec![], None, Some(1)).unwrap();
        assert_eq!(check(Some(&p), &team(), &["WEB", "bastion"]), Ok(()));
        assert_eq!(
            check(Some(&p), &team(), &["web", "jump.example"]),
            Err(Blocked::NotTeam("jump.example".into()))
        );
        assert_eq!(
            check(Some(&p), &team(), &["10.0.0.5"]),
            Err(Blocked::NotTeam("10.0.0.5".into()))
        );
    }
}
