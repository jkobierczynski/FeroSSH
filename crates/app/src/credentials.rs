//! Named credentials: a user, a key file and a password source that many
//! connections share (`Credential ops-admin` in `ferossh.conf`). What the
//! connection manager's Credentials window does to them, kept free of
//! Win32 so it can be tested.

use ferossh_config::PasswordSource;
use ferossh_config::settings::{
    Credential, Settings, valid_credential_name, valid_key_path, valid_login_user,
};

/// A password source as typed in one box: `ask`, `windows`, or a
/// password manager's command.
pub fn password_text(source: &PasswordSource) -> String {
    match source {
        PasswordSource::Ask => "ask".into(),
        PasswordSource::Windows => "windows".into(),
        PasswordSource::Command(c) => c.clone(),
    }
}

/// The source `text` (as [`password_text`] writes it) stands for.
pub fn password_from_text(text: &str) -> PasswordSource {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("ask") {
        PasswordSource::Ask
    } else if t.eq_ignore_ascii_case("windows") {
        PasswordSource::Windows
    } else {
        PasswordSource::Command(t.to_owned())
    }
}

/// A credential as typed in the window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Typed {
    pub name: String,
    pub user: String,
    pub key: String,
    /// The password source as typed (see [`password_text`]), never a
    /// password.
    pub source: String,
}

impl Typed {
    pub fn of(c: &Credential) -> Self {
        Self {
            name: c.name.clone(),
            user: c.user.clone().unwrap_or_default(),
            key: c.key.clone().unwrap_or_default(),
            source: password_text(&c.password),
        }
    }
}

/// Saves `typed` as a new credential (`original` `None`) or over the one
/// called `original`; a new name moves the connections that use it along.
/// Returns the saved name, or what is wrong.
pub fn save(
    settings: &mut Settings,
    original: Option<&str>,
    typed: &Typed,
) -> Result<String, String> {
    let name = typed.name.trim();
    if !valid_credential_name(name) {
        return Err("Name: letters, digits, . _ and - (like ops-admin), up to 64.".into());
    }
    if settings.credentials.iter().any(|c| {
        c.name.eq_ignore_ascii_case(name)
            && !original.is_some_and(|o| o.eq_ignore_ascii_case(&c.name))
    }) {
        return Err(format!("There is a credential called {name} already."));
    }
    let user = typed.user.trim();
    if !user.is_empty() && !valid_login_user(user) {
        return Err("The user name has a space or quote in it.".into());
    }
    let key = typed.key.trim().trim_matches('"').trim();
    if !key.is_empty() && !valid_key_path(key) {
        return Err("The key file's path cannot hold \" or control characters.".into());
    }
    if typed.source.chars().any(char::is_control) {
        return Err("The password command cannot hold control characters.".into());
    }
    let password = password_from_text(&typed.source);
    if user.is_empty() && key.is_empty() && password == PasswordSource::Ask {
        return Err("Fill in a user, a key file or a password source.".into());
    }
    let credential = Credential {
        name: name.to_owned(),
        user: (!user.is_empty()).then(|| user.to_owned()),
        key: (!key.is_empty()).then(|| key.to_owned()),
        password,
    };
    match original.and_then(|o| {
        settings
            .credentials
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(o))
    }) {
        Some(i) => {
            let old = settings.credentials.get(i).map(|c| c.name.clone());
            if let Some(old) = old.as_deref().filter(|o| !o.eq_ignore_ascii_case(name))
                && settings.team.profiles.iter().any(|p| {
                    p.credential
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case(old))
                })
            {
                return Err(format!(
                    "The team library's connections use credential {old} by that name, so it \
                     cannot be renamed."
                ));
            }
            if let Some(slot) = settings.credentials.get_mut(i) {
                *slot = credential;
            }
            if let Some(old) = old.filter(|o| o != name) {
                for p in &mut settings.profiles {
                    if p.credential
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case(&old))
                    {
                        p.credential = Some(name.to_owned());
                    }
                }
            }
        }
        None => settings.credentials.push(credential),
    }
    Ok(name.to_owned())
}

/// Removes credential `name`; the connections that used it go back to
/// their own login. Returns them.
pub fn remove(settings: &mut Settings, name: &str) -> Vec<String> {
    settings
        .credentials
        .retain(|c| !c.name.eq_ignore_ascii_case(name));
    let mut users = Vec::new();
    for p in &mut settings.profiles {
        if p.credential
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(name))
        {
            p.credential = None;
            users.push(p.host.clone());
        }
    }
    users
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn typed(name: &str, user: &str, key: &str, source: &str) -> Typed {
        Typed {
            name: name.into(),
            user: user.into(),
            key: key.into(),
            source: source.into(),
        }
    }

    #[test]
    fn credentials_are_saved_renamed_and_removed() {
        let mut s =
            Settings::parse("Profile web\n    Credential old\nProfile db\n    Credential old\n");
        let made = save(&mut s, None, &typed("old", "ops", "", "Windows")).unwrap();
        assert_eq!(made, "old");
        assert_eq!(s.password_source("web"), PasswordSource::Windows);
        assert!(
            save(&mut s, None, &typed("OLD", "x", "", ""))
                .unwrap_err()
                .contains("already")
        );
        assert!(save(&mut s, None, &typed("bad name", "x", "", "")).is_err());
        assert!(save(&mut s, None, &typed("new", "a b", "", "")).is_err());
        assert!(
            save(&mut s, None, &typed("new", "", "", "ask"))
                .unwrap_err()
                .contains("Fill in")
        );
        assert!(save(&mut s, None, &typed("new", "", "C:\\k\u{7}", "")).is_err());

        // A new name takes its connections along.
        save(
            &mut s,
            Some("old"),
            &typed("ops-admin", "ops", " \"C:\\Keys\\ops\" ", "op read x"),
        )
        .unwrap();
        assert_eq!(s.credential_users("ops-admin"), ["web", "db"]);
        let c = s.credential("ops-admin").unwrap();
        assert_eq!(c.key.as_deref(), Some("C:\\Keys\\ops"));
        assert_eq!(Typed::of(c).source, "op read x");

        assert_eq!(remove(&mut s, "OPS-ADMIN"), ["web", "db"]);
        assert!(s.credentials.is_empty());
        assert_eq!(s.profile("web").unwrap().credential, None);
    }

    #[test]
    fn password_sources_as_text() {
        for source in [
            PasswordSource::Ask,
            PasswordSource::Windows,
            PasswordSource::Command("pass show %n".into()),
        ] {
            assert_eq!(password_from_text(&password_text(&source)), source);
        }
        assert_eq!(password_from_text(" "), PasswordSource::Ask);
    }
}
