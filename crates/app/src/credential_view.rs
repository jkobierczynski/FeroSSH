//! Tools > Credential manager: the named credentials and the passwords
//! FeroSSH keeps (in the Windows Credential Manager or a master password
//! vault), a row each with what uses it, searched and sorted.
//! Platform-neutral, so it is tested on any host; the native window is in
//! `credential_panel.rs`. No row ever holds a password.

use ferossh_config::Settings;

use crate::credentials::password_text;

/// What a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A named credential (`Credential ops` in `ferossh.conf`).
    Credential,
    /// A login's saved password (`FeroSSH:user@host:port`).
    Login,
    /// A proxy's saved password (`FeroSSH proxy:user@host:port`).
    Proxy,
    /// A named secret a script asks for (`FeroSSH secret:name`).
    Secret,
}

impl Kind {
    pub fn title(self) -> &'static str {
        match self {
            Kind::Credential => "Credential",
            Kind::Login => "Saved password",
            Kind::Proxy => "Proxy password",
            Kind::Secret => "Script secret",
        }
    }
}

/// Where a saved password is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub windows: bool,
    pub vault: bool,
}

impl Place {
    pub fn text(self) -> &'static str {
        match (self.windows, self.vault) {
            (true, true) => "Windows and vault",
            (true, false) => "Windows",
            (false, true) => "Vault",
            (false, false) => "",
        }
    }
}

/// One row: a credential or a saved password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: Kind,
    /// The credential's name, or the saved password's Credential Manager
    /// name (`FeroSSH:bob@web:22`), which deleting it needs.
    pub id: String,
    /// What the list shows as its name: the credential's name, the
    /// login (`bob@web:22`) or the secret's name.
    pub name: String,
    pub user: String,
    /// A credential's key file.
    pub key: String,
    /// A credential's password source (`ask`, `windows` or a command), or
    /// where a saved password is.
    pub password: String,
    /// The connections that use it, as named in the connection manager.
    pub used_by: Vec<String>,
}

/// The list's columns, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Kind,
    User,
    Password,
    Key,
    UsedBy,
}

impl Column {
    pub const ALL: [Column; 6] = [
        Column::Name,
        Column::Kind,
        Column::User,
        Column::Password,
        Column::Key,
        Column::UsedBy,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Kind => "Kind",
            Column::User => "User",
            Column::Password => "Password",
            Column::Key => "Key file",
            Column::UsedBy => "Used by",
        }
    }

    /// Width in DIPs.
    pub fn width(self) -> i32 {
        match self {
            Column::Name => 190,
            Column::Kind => 110,
            Column::User => 110,
            Column::Password => 150,
            Column::Key => 170,
            Column::UsedBy => 220,
        }
    }
}

/// A row per named credential, with the connections that pick it.
pub fn credential_rows(settings: &Settings) -> Vec<Row> {
    settings
        .credentials
        .iter()
        .map(|c| Row {
            kind: Kind::Credential,
            id: c.name.clone(),
            name: c.name.clone(),
            user: c.user.clone().unwrap_or_default(),
            key: c.key.clone().unwrap_or_default(),
            password: password_text(&c.password),
            used_by: settings.credential_users(&c.name),
        })
        .collect()
}

/// A row for the saved password under Credential Manager name `target`;
/// `None` for a name that is not FeroSSH's. `used_by` lists the
/// connections whose login keeps its password there.
pub fn password_row(target: &str, user: &str, place: Place, used_by: Vec<String>) -> Option<Row> {
    let (kind, name) = if let Some(login) = target.strip_prefix("FeroSSH:") {
        (Kind::Login, login)
    } else if let Some(login) = target.strip_prefix("FeroSSH proxy:") {
        (Kind::Proxy, login)
    } else {
        (Kind::Secret, target.strip_prefix("FeroSSH secret:")?)
    };
    Some(Row {
        kind,
        id: target.to_owned(),
        name: clean(name),
        user: clean(user),
        key: String::new(),
        password: place.text().to_owned(),
        used_by,
    })
}

/// A name from the Credential Manager, which anything on the PC can
/// write: no control or bidirectional characters in the list.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                        | '\u{61c}'
                )
        })
        .take(200)
        .collect()
}

/// What a cell shows.
pub fn text(row: &Row, column: Column) -> String {
    match column {
        Column::Name => row.name.clone(),
        Column::Kind => row.kind.title().to_owned(),
        Column::User => row.user.clone(),
        Column::Password => row.password.clone(),
        Column::Key => row.key.clone(),
        Column::UsedBy => {
            if row.used_by.is_empty() {
                match row.kind {
                    // A script names its secrets; FeroSSH cannot tell which.
                    Kind::Secret => "Scripts".to_owned(),
                    // None listed goes through it; a typed one still may.
                    Kind::Proxy => "Logins through this proxy".to_owned(),
                    _ => "Nothing".to_owned(),
                }
            } else {
                crate::bulk::name_list(&row.used_by)
            }
        }
    }
}

/// The rows whose cells hold every word of `query` (any case); a word
/// starting with `-` keeps the rows that do not hold it.
pub fn filter<'a>(rows: &'a [Row], query: &str) -> Vec<&'a Row> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    rows.iter()
        .filter(|r| {
            let all = format!(
                "{} {} {} {} {} {}",
                r.name,
                r.kind.title(),
                r.user,
                r.password,
                r.key,
                // Every name, not the shortened list.
                if r.used_by.is_empty() {
                    text(r, Column::UsedBy)
                } else {
                    r.used_by.join(" ")
                }
            )
            .to_lowercase();
            words.iter().all(|w| match w.strip_prefix('-') {
                Some(not) if !not.is_empty() => !all.contains(not),
                _ => all.contains(w.as_str()),
            })
        })
        .collect()
}

/// Sorts by `column` (credentials before saved passwords on a tie), then
/// by name.
pub fn sort(rows: &mut [&Row], column: Column, descending: bool) {
    rows.sort_by(|a, b| {
        let o = match column {
            Column::UsedBy => a.used_by.len().cmp(&b.used_by.len()),
            Column::Kind => (a.kind as u8).cmp(&(b.kind as u8)),
            c => text(a, c).to_lowercase().cmp(&text(b, c).to_lowercase()),
        };
        let o = if descending { o.reverse() } else { o };
        o.then_with(|| (a.kind as u8).cmp(&(b.kind as u8)))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// The line under the list.
pub fn summary(shown: &[&Row], total: usize) -> String {
    let credentials = shown.iter().filter(|r| r.kind == Kind::Credential).count();
    let passwords = shown.len() - credentials;
    let unused = shown
        .iter()
        .filter(|r| r.used_by.is_empty() && matches!(r.kind, Kind::Credential | Kind::Login))
        .count();
    let mut s = format!(
        "{credentials} credential{}, {passwords} saved password{}",
        if credentials == 1 { "" } else { "s" },
        if passwords == 1 { "" } else { "s" },
    );
    if shown.len() < total {
        s.push_str(&format!(" shown of {total}"));
    }
    if unused > 0 {
        s.push_str(&format!(
            ". {unused} used by nothing: search for nothing to see {}.",
            if unused == 1 { "it" } else { "them" }
        ));
    } else {
        s.push('.');
    }
    s
}

/// What deleting `row` asks first.
pub fn delete_question(row: &Row) -> String {
    let users = crate::bulk::name_list(&row.used_by);
    match row.kind {
        Kind::Credential if row.used_by.is_empty() => {
            format!("Delete credential {}? No connection uses it.", row.name)
        }
        Kind::Credential => format!(
            "Delete credential {}? It is still used by {users}, which go back to their own \
             login.",
            row.name
        ),
        Kind::Secret => format!(
            "Forget the script secret {}? A script that asks for it asks you again.",
            row.name
        ),
        Kind::Proxy if row.used_by.is_empty() => format!(
            "Forget the saved proxy password for {}? Connections through that proxy ask for \
             it again.",
            row.name
        ),
        Kind::Proxy => format!(
            "Forget the saved proxy password for {}? It is still used by {users}, which ask \
             for it again through that proxy.",
            row.name
        ),
        _ if row.used_by.is_empty() => format!(
            "Forget the saved password for {}? No connection uses it.",
            row.name
        ),
        _ => format!(
            "Forget the saved password for {}? It is still used by {users}, which ask for it \
             at their next login.",
            row.name
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::parse(
            "Credential ops\n    User admin\n    PasswordSource windows\nCredential spare\n    \
             IdentityFile ~/.ssh/spare\nProfile web\n    Credential ops\nProfile db\n    Credential OPS\n",
        )
    }

    fn rows() -> Vec<Row> {
        let mut rows = credential_rows(&settings());
        rows.push(
            password_row(
                "FeroSSH:admin@web:22",
                "admin",
                Place {
                    windows: true,
                    vault: false,
                },
                vec!["web".into()],
            )
            .unwrap(),
        );
        rows.push(
            password_row(
                "FeroSSH:old@gone:22",
                "old",
                Place {
                    windows: false,
                    vault: true,
                },
                vec![],
            )
            .unwrap(),
        );
        rows.push(password_row("FeroSSH secret:otp", "", Place::default(), vec![]).unwrap());
        rows
    }

    #[test]
    fn credentials_list_what_picks_them() {
        let rows = credential_rows(&settings());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].used_by, ["web", "db"]);
        assert_eq!(rows[0].password, "windows");
        assert_eq!(text(&rows[0], Column::UsedBy), "web, db");
        assert!(rows[1].used_by.is_empty());
        assert_eq!(text(&rows[1], Column::UsedBy), "Nothing");
        assert_eq!(rows[1].key, "~/.ssh/spare");
    }

    #[test]
    fn saved_passwords_read_their_names() {
        let rows = rows();
        let login = &rows[2];
        assert_eq!(
            (login.kind, login.name.as_str()),
            (Kind::Login, "admin@web:22")
        );
        assert_eq!(login.password, "Windows");
        assert_eq!(rows[3].password, "Vault");
        assert_eq!(rows[4].kind, Kind::Secret);
        assert_eq!(text(&rows[4], Column::UsedBy), "Scripts");
        let proxy = password_row(
            "FeroSSH proxy:me@socks:1080",
            "me",
            Place::default(),
            vec![],
        );
        let proxy = proxy.unwrap();
        assert_eq!(proxy.kind, Kind::Proxy);
        assert!(delete_question(&proxy).contains("through that proxy"));
        let used = password_row(
            "FeroSSH proxy:me@socks:1080",
            "me",
            Place::default(),
            vec!["web".into(), "db".into()],
        )
        .unwrap();
        assert_eq!(text(&used, Column::UsedBy), "web, db");
        assert!(delete_question(&used).contains("still used by web, db"));
        assert!(password_row("Other:thing", "", Place::default(), vec![]).is_none());
        // What another program wrote there shows without its tricks.
        let odd = password_row("FeroSSH:a\u{202e}b\x1b@h:22", "", Place::default(), vec![]);
        assert_eq!(odd.unwrap().name, "ab@h:22");
    }

    #[test]
    fn search_takes_every_word_and_leaves_out_minus_words() {
        let rows = rows();
        let names =
            |q: &str| -> Vec<String> { filter(&rows, q).iter().map(|r| r.name.clone()).collect() };
        assert_eq!(names("web"), ["ops", "admin@web:22"]);
        assert_eq!(names("ADMIN credential"), ["ops"]);
        assert_eq!(names("nothing"), ["spare", "old@gone:22"]);
        assert_eq!(names("saved -vault"), ["admin@web:22"]);
        assert_eq!(names("").len(), 5);
    }

    #[test]
    fn sorting_and_the_summary() {
        let rows = rows();
        let mut shown: Vec<&Row> = rows.iter().collect();
        sort(&mut shown, Column::UsedBy, true);
        assert_eq!(shown[0].name, "ops");
        sort(&mut shown, Column::Name, false);
        assert_eq!(shown[0].name, "admin@web:22");
        assert_eq!(
            summary(&shown, rows.len()),
            "2 credentials, 3 saved passwords. 2 used by nothing: search for nothing to see them."
        );
        let some = filter(&rows, "ops");
        assert_eq!(
            summary(&some, rows.len()),
            "1 credential, 0 saved passwords shown of 5."
        );
    }

    #[test]
    fn deleting_warns_while_in_use() {
        let rows = rows();
        assert!(delete_question(&rows[0]).contains("still used by web, db"));
        assert!(delete_question(&rows[1]).contains("No connection uses it"));
        assert!(delete_question(&rows[2]).contains("ask for it at their next login"));
        assert!(delete_question(&rows[3]).contains("No connection uses it"));
        assert!(delete_question(&rows[4]).contains("script"));
    }
}
