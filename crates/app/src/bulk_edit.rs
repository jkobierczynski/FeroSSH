//! Change one field on many connections at once, from the connection
//! manager's right-click menu on a selection. Kept free of Win32 so it can
//! be tested.
//!
//! Login fields (user, key file, jump hosts, proxy) live in `sessions.conf`,
//! so only connections saved in FeroSSH change; hosts from `~/.ssh/config`
//! or an import are left as they are. The other fields live in each host's
//! profile in `ferossh.conf`, so every host can change. A field a
//! connection's type does not use (a key file on a serial line) is left
//! alone too. Every host left alone is named, with why.

use ferossh_config::saved::{self, SavedHost};
use ferossh_config::settings::{self, SessionKind, Settings};
use ferossh_config::{PasswordSource, ProxyServer};

/// The fields that can change on many connections at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditField {
    User,
    Key,
    Jump,
    Proxy,
    Password,
    Credential,
    Theme,
    Font,
    FontSize,
    Tmux,
    StartDir,
}

/// What a `tmux` value of `on` attaches to.
const DEFAULT_TMUX: &str = "main";

impl EditField {
    /// In menu order.
    pub const ALL: [Self; 11] = [
        Self::User,
        Self::Key,
        Self::Jump,
        Self::Proxy,
        Self::Password,
        Self::Credential,
        Self::Theme,
        Self::Font,
        Self::FontSize,
        Self::Tmux,
        Self::StartDir,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Key => "Key file",
            Self::Jump => "Jump hosts",
            Self::Proxy => "Proxy",
            Self::Password => "Password source",
            Self::Credential => "Credential",
            Self::Theme => "Theme",
            Self::Font => "Font",
            Self::FontSize => "Font size",
            Self::Tmux => "tmux session",
            Self::StartDir => "Start directory",
        }
    }

    /// What to type, and what an empty box does.
    pub fn hint(self) -> &'static str {
        match self {
            Self::User => "The user to log in as. Empty: your Windows user name.",
            Self::Key => {
                "The private key file, e.g. C:\\Users\\me\\.ssh\\id_ed25519. Empty: \
                          the agent and the usual key files."
            }
            Self::Jump => {
                "Jump hosts as on a ProxyJump line: a connection name or \
                           user@host:port, several separated by commas. Empty: straight there."
            }
            Self::Proxy => {
                "socks5://[user@]host[:port] or http://[user@]host[:port]. Empty: \
                            no proxy."
            }
            Self::Password => {
                "ask, windows (the Windows Credential Manager), or a password \
                               manager's command; %n stands for the connection's name. Empty: \
                               ask."
            }
            Self::Credential => {
                "A named credential: its user, key file and password source win \
                                 over each connection's own. Empty: their own login."
            }
            Self::Theme => "A theme's name, as in View > Theme. Empty: the theme rules decide.",
            Self::Font => "A font's name, e.g. Cascadia Mono. Empty: the default font.",
            Self::FontSize => "In points, 4 to 96. Empty: the default size.",
            Self::Tmux => {
                "The tmux session to attach after login: on for main, or a name. \
                           Empty or off: no tmux."
            }
            Self::StartDir => {
                "The server directory new shells start in. Empty: the home \
                               directory."
            }
        }
    }

    /// Kept in `sessions.conf`, so only saved connections change.
    fn saved_only(self) -> bool {
        matches!(self, Self::User | Self::Key | Self::Jump | Self::Proxy)
    }

    /// Whether a connection of type `kind` uses the field.
    fn fits(self, kind: SessionKind) -> bool {
        use SessionKind as K;
        let login = matches!(kind, K::Ssh | K::Mosh | K::Sftp | K::Tunnel);
        match self {
            Self::User => login || kind == K::WinRm,
            Self::Key | Self::Jump | Self::Proxy => login,
            Self::Password | Self::Credential => login || kind == K::WinRm,
            Self::Tmux => kind == K::Ssh,
            Self::StartDir => matches!(kind, K::Ssh | K::Mosh),
            Self::Theme | Self::Font | Self::FontSize => !matches!(
                kind,
                K::Rdp | K::Vnc | K::Xvfb | K::RemoteApp | K::Sftp | K::Tunnel
            ),
        }
    }
}

/// One selected host and its connection type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub kind: SessionKind,
}

/// The field's value on host `name`, as typed in the box; empty when
/// unset.
pub fn current(field: EditField, name: &str, saved: &[SavedHost], settings: &Settings) -> String {
    let host = saved.iter().find(|h| h.alias.eq_ignore_ascii_case(name));
    let p = settings.profile(name);
    let text = |v: Option<&String>| v.cloned().unwrap_or_default();
    match field {
        EditField::User => text(host.and_then(|h| h.user.as_ref())),
        EditField::Key => text(host.and_then(|h| h.identity_file.as_ref())),
        EditField::Jump => text(host.and_then(|h| h.proxy_jump.as_ref())),
        EditField::Proxy => text(host.and_then(|h| h.proxy_server.as_ref())),
        EditField::Password => match p.map(|p| &p.password) {
            None | Some(PasswordSource::Ask) => "ask".into(),
            Some(PasswordSource::Windows) => "windows".into(),
            Some(PasswordSource::Command(c)) => c.clone(),
        },
        EditField::Credential => text(p.and_then(|p| p.credential.as_ref())),
        EditField::Theme => text(p.and_then(|p| p.theme.as_ref())),
        EditField::Font => text(p.and_then(|p| p.font.as_ref())),
        EditField::FontSize => p
            .and_then(|p| p.font_size)
            .map(|s| s.to_string())
            .unwrap_or_default(),
        EditField::Tmux => p
            .and_then(|p| p.tmux.clone())
            .unwrap_or_else(|| "off".into()),
        EditField::StartDir => text(p.and_then(|p| p.start_dir.as_ref())),
    }
}

/// Why a host is left alone.
fn left_alone(field: EditField, t: &Target, saved: &[SavedHost]) -> Option<&'static str> {
    if !field.fits(t.kind) {
        return Some("their connection type has no such field");
    }
    if field.saved_only() && !saved.iter().any(|h| h.alias.eq_ignore_ascii_case(&t.name)) {
        return Some("they come from ~/.ssh/config or an import, which FeroSSH does not change");
    }
    None
}

/// The value the hosts the field applies to share, to start the box
/// with; `None` when they hold several values (or none applies).
pub fn shared(
    field: EditField,
    targets: &[Target],
    saved: &[SavedHost],
    settings: &Settings,
) -> Option<String> {
    let mut values = targets
        .iter()
        .filter(|t| left_alone(field, t, saved).is_none())
        .map(|t| current(field, &t.name, saved, settings));
    let first = values.next()?;
    values.all(|v| v == first).then_some(first)
}

/// One host's change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    /// A WinRM connection: a password source there needs a user.
    pub winrm: bool,
    pub from: String,
    pub to: String,
}

/// What a change of one field on many connections will do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub field: EditField,
    /// The value as it will be kept; empty clears the field.
    pub value: String,
    pub changes: Vec<Change>,
    /// Hosts left alone, and why.
    pub skipped: Vec<(String, &'static str)>,
}

fn plain(text: &str, what: &str) -> Result<(), String> {
    if text.contains('"') || text.chars().any(char::is_control) {
        return Err(format!("{what} cannot hold \" or control characters."));
    }
    Ok(())
}

/// Checks `typed` for `field` and gives the value as kept.
fn normalise(
    field: EditField,
    typed: &str,
    themes: &[String],
    settings: &Settings,
) -> Result<String, String> {
    let v = typed.trim();
    match field {
        EditField::User => {
            if v.contains(char::is_whitespace) || v.chars().any(char::is_control) {
                return Err("The user name has a space in it.".into());
            }
            Ok(v.into())
        }
        EditField::Key => plain(v, "The key file").map(|()| v.into()),
        EditField::Font => {
            if v.chars().count() > 64 {
                return Err("The font name is too long.".into());
            }
            plain(v, "The font name").map(|()| v.into())
        }
        EditField::Jump => {
            if !v.is_empty() && !saved::valid_proxy_jump(v) {
                return Err(
                    "Jump host: a connection name or user@host:port; several go \
                            separated by commas."
                        .into(),
                );
            }
            Ok(v.into())
        }
        EditField::Proxy => {
            if v.is_empty() {
                return Ok(String::new());
            }
            match ProxyServer::parse(v) {
                Some(p) => Ok(p.to_string()),
                None if v.split("://").nth(1).is_some_and(|r| {
                    r.split_once('@')
                        .is_some_and(|(login, _)| login.contains(':'))
                }) =>
                {
                    Err(
                        "Proxy: no password in the address; FeroSSH asks for it and keeps \
                         it in the Windows Credential Manager."
                            .into(),
                    )
                }
                None => {
                    Err("Proxy: socks5://[user@]host[:port] or http://[user@]host[:port].".into())
                }
            }
        }
        EditField::Password => {
            // Quotes group a command's arguments, as in the form.
            if v.chars().any(char::is_control) {
                return Err("The password command cannot hold control characters.".to_owned());
            }
            Ok(if v.is_empty() || v.eq_ignore_ascii_case("ask") {
                "ask".into()
            } else if v.eq_ignore_ascii_case("windows") {
                "windows".into()
            } else {
                v.into()
            })
        }
        EditField::Credential => {
            if v.is_empty() {
                return Ok(String::new());
            }
            settings
                .credential(v)
                .map(|c| c.name.clone())
                .ok_or_else(|| format!("There is no credential called {v}."))
        }
        EditField::Theme => {
            if v.is_empty() {
                return Ok(String::new());
            }
            themes
                .iter()
                .find(|t| t.eq_ignore_ascii_case(v))
                .cloned()
                .ok_or_else(|| format!("There is no theme called {v}."))
        }
        EditField::FontSize => {
            if v.is_empty() {
                return Ok(String::new());
            }
            match v.parse::<f32>() {
                Ok(s) if (4.0..=96.0).contains(&s) => Ok(s.to_string()),
                Ok(_) => Err("The font size goes from 4 to 96.".into()),
                Err(_) => Err("The font size is a number, like 11 or 10.5.".into()),
            }
        }
        EditField::Tmux => {
            if settings::tmux_off(v) {
                Ok("off".into())
            } else if settings::tmux_on(v) {
                Ok(DEFAULT_TMUX.into())
            } else if v
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                Ok(v.into())
            } else {
                Err("tmux session names use letters, digits, . _ and - only.".into())
            }
        }
        EditField::StartDir => {
            if v.is_empty() {
                return Ok(String::new());
            }
            settings::start_dir(v)
                .ok_or_else(|| "The start directory cannot hold \" or control characters.".into())
        }
    }
}

/// Works out what setting `field` to `typed` on `targets` changes,
/// without changing anything. An error names what is wrong with `typed`.
pub fn plan(
    field: EditField,
    typed: &str,
    targets: &[Target],
    saved: &[SavedHost],
    settings: &Settings,
    themes: &[String],
) -> Result<Plan, String> {
    let value = normalise(field, typed, themes, settings)?;
    let mut changes = Vec::new();
    let mut skipped = Vec::new();
    for t in targets {
        if let Some(why) = left_alone(field, t, saved) {
            skipped.push((t.name.clone(), why));
            continue;
        }
        let host = saved.iter().find(|h| h.alias.eq_ignore_ascii_case(&t.name));
        let why = match field {
            EditField::User if value.contains('@') && t.kind != SessionKind::WinRm => {
                Some("only WinRM users can hold @")
            }
            EditField::Jump
                if value
                    .split(',')
                    .any(|j| j.trim().eq_ignore_ascii_case(&t.name)) =>
            {
                Some("a connection cannot be its own jump host")
            }
            EditField::Jump
                if !value.is_empty() && host.is_some_and(|h| h.proxy_server.is_some()) =>
            {
                Some("they go through a proxy; clear it first")
            }
            EditField::Proxy
                if !value.is_empty() && host.is_some_and(|h| h.proxy_jump.is_some()) =>
            {
                Some("they go through jump hosts; clear them first")
            }
            EditField::Password | EditField::User
                if t.kind == SessionKind::WinRm
                    && winrm_without_user(field, &value, host, settings, &t.name) =>
            {
                Some("WinRM needs a user with that password source")
            }
            _ => None,
        };
        if let Some(why) = why {
            skipped.push((t.name.clone(), why));
            continue;
        }
        let from = current(field, &t.name, saved, settings);
        if from != value {
            changes.push(Change {
                name: t.name.clone(),
                winrm: t.kind == SessionKind::WinRm,
                from,
                to: value.clone(),
            });
        }
    }
    Ok(Plan {
        field,
        value,
        changes,
        skipped,
    })
}

/// WinRM with no user logs in as yourself with no password, so a
/// password source needs a user there, as in the form: whether setting
/// `field` to `value` on `name` would leave one without.
fn winrm_without_user(
    field: EditField,
    value: &str,
    host: Option<&SavedHost>,
    settings: &Settings,
    name: &str,
) -> bool {
    match field {
        EditField::Password => {
            value != "ask" && host.is_none_or(|h| h.user.as_deref().is_none_or(str::is_empty))
        }
        EditField::User => {
            value.is_empty() && settings.password_source(name) != PasswordSource::Ask
        }
        _ => false,
    }
}

/// The planned hosts that may no longer change, with `saved` read again
/// and `settings` as they are now: one that now has a proxy (for jump
/// hosts) or jump hosts (for a proxy), as a connection never holds both,
/// or a WinRM one that would be left with a password source and no
/// user. [`apply`] leaves them alone.
pub fn conflicts(plan: &Plan, saved: &[SavedHost], settings: &Settings) -> Vec<String> {
    plan.changes
        .iter()
        .filter(|c| conflicting(plan, c, saved, settings))
        .map(|c| c.name.clone())
        .collect()
}

fn conflicting(plan: &Plan, c: &Change, saved: &[SavedHost], settings: &Settings) -> bool {
    let host = saved.iter().find(|h| h.alias.eq_ignore_ascii_case(&c.name));
    if c.winrm && winrm_without_user(plan.field, &plan.value, host, settings, &c.name) {
        return true;
    }
    !plan.value.is_empty()
        && host.is_some_and(|h| match plan.field {
            EditField::Jump => h.proxy_server.is_some(),
            EditField::Proxy => h.proxy_jump.is_some(),
            _ => false,
        })
}

/// Makes the changes in `plan`. Returns whether `sessions.conf` (the
/// saved connections) changed; the profiles change in `settings`. Hosts
/// in [`conflicts`] are left alone.
pub fn apply(plan: &Plan, saved: &mut [SavedHost], settings: &mut Settings) -> bool {
    let value = Some(plan.value.clone()).filter(|v| !v.is_empty());
    let skip = conflicts(plan, saved, settings);
    let mut saved_changed = false;
    for c in &plan.changes {
        if skip.contains(&c.name) {
            continue;
        }
        if plan.field.saved_only() {
            let Some(h) = saved
                .iter_mut()
                .find(|h| h.alias.eq_ignore_ascii_case(&c.name))
            else {
                continue;
            };
            let slot = match plan.field {
                EditField::User => &mut h.user,
                EditField::Key => &mut h.identity_file,
                EditField::Jump => &mut h.proxy_jump,
                _ => &mut h.proxy_server,
            };
            *slot = value.clone();
            saved_changed = true;
            continue;
        }
        let p = settings.profile_mut(&c.name);
        match plan.field {
            EditField::Password => {
                p.password = match plan.value.as_str() {
                    "ask" => PasswordSource::Ask,
                    "windows" => PasswordSource::Windows,
                    cmd => PasswordSource::Command(cmd.to_owned()),
                };
            }
            EditField::Credential => p.credential = value.clone(),
            EditField::Theme => p.theme = value.clone(),
            EditField::Font => p.font = value.clone(),
            EditField::FontSize => p.font_size = value.as_deref().and_then(|s| s.parse().ok()),
            EditField::Tmux => p.tmux = value.clone().filter(|v| v != "off"),
            EditField::StartDir => p.start_dir = value.clone(),
            _ => {}
        }
    }
    saved_changed
}

/// How a value reads in the preview.
fn shown(field: EditField, value: &str) -> String {
    if value.is_empty() {
        return match field {
            EditField::Theme => "(by theme rules)".into(),
            EditField::Credential => "(their own login)".into(),
            _ => "(empty)".into(),
        };
    }
    value.to_owned()
}

/// The preview shown before saving: what changes on which hosts, and
/// which are left alone.
pub fn preview(plan: &Plan) -> String {
    const SHOWN: usize = 12;
    let label = plan.field.label();
    let n = plan.changes.len();
    let mut out = format!(
        "Set {label} to {} on {n} connection{}:\n",
        shown(plan.field, &plan.value),
        if n == 1 { "" } else { "s" }
    );
    for c in plan.changes.iter().take(SHOWN) {
        out.push_str(&format!(
            "\n    {}: {} \u{2192} {}",
            c.name,
            shown(plan.field, &c.from),
            shown(plan.field, &c.to)
        ));
    }
    if n > SHOWN {
        out.push_str(&format!("\n    and {} more", n - SHOWN));
    }
    let mut reasons: Vec<&'static str> = Vec::new();
    for (_, why) in &plan.skipped {
        if !reasons.contains(why) {
            reasons.push(why);
        }
    }
    for why in reasons {
        let names: Vec<String> = plan
            .skipped
            .iter()
            .filter(|(_, w)| *w == why)
            .map(|(n, _)| n.clone())
            .collect();
        out.push_str(&format!(
            "\n\nLeft alone, because {why}: {}.",
            crate::bulk::name_list(&names)
        ));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn target(name: &str, kind: SessionKind) -> Target {
        Target {
            name: name.into(),
            kind,
        }
    }

    fn saved_host(alias: &str, user: Option<&str>) -> SavedHost {
        SavedHost {
            alias: alias.into(),
            user: user.map(str::to_owned),
            ..SavedHost::default()
        }
    }

    fn themes() -> Vec<String> {
        vec!["Nord".into(), "Production".into()]
    }

    #[test]
    fn a_login_field_changes_saved_connections_only() {
        let mut saved = vec![
            saved_host("web", Some("admin")),
            saved_host("db", Some("admin")),
        ];
        let mut settings = Settings::default();
        let targets = [
            target("web", SessionKind::Ssh),
            target("db", SessionKind::Ssh),
            target("github", SessionKind::Ssh),
            target("com1", SessionKind::Serial),
        ];
        assert_eq!(
            shared(EditField::User, &targets, &saved, &settings),
            Some("admin".into())
        );
        let plan = plan(
            EditField::User,
            " ops ",
            &targets,
            &saved,
            &settings,
            &themes(),
        )
        .unwrap();
        assert_eq!(plan.value, "ops");
        assert_eq!(plan.changes.len(), 2);
        assert_eq!(plan.skipped.len(), 2);
        let text = preview(&plan);
        assert!(text.contains("web: admin \u{2192} ops"), "{text}");
        assert!(text.contains("~/.ssh/config or an import, which FeroSSH does not change: github"));
        assert!(text.contains("no such field: com1"));
        assert!(apply(&plan, &mut saved, &mut settings));
        assert_eq!(saved[1].user.as_deref(), Some("ops"));
        // Emptying the box clears it.
        let plan2 = super::plan(EditField::User, "", &targets, &saved, &settings, &[]).unwrap();
        apply(&plan2, &mut saved, &mut settings);
        assert_eq!(saved[0].user, None);
        assert!(super::plan(EditField::User, "a b", &targets, &saved, &settings, &[]).is_err());
    }

    #[test]
    fn several_values_and_nothing_to_change() {
        let saved = vec![saved_host("a", Some("x")), saved_host("b", Some("y"))];
        let settings = Settings::default();
        let t = [target("a", SessionKind::Ssh), target("b", SessionKind::Ssh)];
        assert_eq!(shared(EditField::User, &t, &saved, &settings), None);
        let p = plan(EditField::User, "x", &t, &saved, &settings, &[]).unwrap();
        assert_eq!(p.changes.len(), 1, "a already has it");
    }

    #[test]
    fn profile_fields_change_every_host() {
        let saved = Vec::new();
        let mut settings = Settings::parse("Profile web\n    Theme Nord\n");
        let t = [
            target("web", SessionKind::Ssh),
            target("pi", SessionKind::Ssh),
        ];
        let p = plan(
            EditField::Theme,
            "production",
            &t,
            &saved,
            &settings,
            &themes(),
        )
        .unwrap();
        assert_eq!(p.value, "Production");
        assert_eq!(p.changes.len(), 2);
        apply(&p, &mut saved.clone(), &mut settings);
        assert_eq!(
            settings.profile("pi").unwrap().theme.as_deref(),
            Some("Production")
        );
        assert!(plan(EditField::Theme, "nope", &t, &saved, &settings, &themes()).is_err());

        let p = plan(EditField::FontSize, "10.5", &t, &saved, &settings, &[]).unwrap();
        apply(&p, &mut [], &mut settings);
        assert_eq!(settings.profile("web").unwrap().font_size, Some(10.5));
        assert!(plan(EditField::FontSize, "200", &t, &saved, &settings, &[]).is_err());

        let p = plan(EditField::Tmux, "on", &t, &saved, &settings, &[]).unwrap();
        apply(&p, &mut [], &mut settings);
        assert_eq!(
            settings.profile("pi").unwrap().tmux.as_deref(),
            Some("main")
        );
        let p = plan(EditField::Tmux, "off", &t, &saved, &settings, &[]).unwrap();
        apply(&p, &mut [], &mut settings);
        assert_eq!(settings.profile("pi").unwrap().tmux, None);

        let p = plan(EditField::Password, "Windows", &t, &saved, &settings, &[]).unwrap();
        apply(&p, &mut [], &mut settings);
        assert_eq!(
            settings.profile("web").unwrap().password,
            PasswordSource::Windows
        );
        assert_eq!(
            shared(EditField::Password, &t, &saved, &settings),
            Some("windows".into())
        );

        // The settings file keeps what was set.
        let again = Settings::parse(&settings.to_text());
        assert_eq!(
            again.profile("pi").unwrap().theme.as_deref(),
            Some("Production")
        );
    }

    #[test]
    fn a_credential_is_named_for_many() {
        let mut settings = Settings::parse("Credential ops-admin\n    User ops\n");
        let t = [
            target("web", SessionKind::Ssh),
            target("com1", SessionKind::Serial),
        ];
        let p = plan(EditField::Credential, "OPS-ADMIN", &t, &[], &settings, &[]).unwrap();
        assert_eq!(p.value, "ops-admin");
        assert_eq!((p.changes.len(), p.skipped.len()), (1, 1));
        apply(&p, &mut [], &mut settings);
        assert_eq!(settings.credential_of("web").unwrap().name, "ops-admin");
        assert!(plan(EditField::Credential, "nope", &t, &[], &settings, &[]).is_err());
        let p = plan(EditField::Credential, "", &t, &[], &settings, &[]).unwrap();
        assert!(preview(&p).contains("(their own login)"));
    }

    #[test]
    fn jump_hosts_and_proxies_are_checked_per_host() {
        let mut saved = vec![saved_host("bastion", None), saved_host("web", None)];
        saved[1].proxy_server = Some("socks5://proxy:1080".into());
        let settings = Settings::default();
        let t = [
            target("bastion", SessionKind::Ssh),
            target("web", SessionKind::Ssh),
        ];
        let p = plan(EditField::Jump, "bastion", &t, &saved, &settings, &[]).unwrap();
        assert!(p.changes.is_empty());
        assert_eq!(p.skipped.len(), 2, "itself, and a proxy already");
        assert!(plan(EditField::Jump, "bad host!", &t, &saved, &settings, &[]).is_err());
        assert!(
            plan(
                EditField::Proxy,
                "socks5://me:pw@p",
                &t,
                &saved,
                &settings,
                &[]
            )
            .unwrap_err()
            .contains("no password")
        );
    }

    #[test]
    fn a_proxy_added_since_the_preview_is_left_alone() {
        let saved = vec![saved_host("web", None), saved_host("db", None)];
        let t = [
            target("web", SessionKind::Ssh),
            target("db", SessionKind::Ssh),
        ];
        let p = plan(
            EditField::Jump,
            "bastion",
            &t,
            &saved,
            &Settings::default(),
            &[],
        )
        .unwrap();
        assert_eq!(p.changes.len(), 2);
        // sessions.conf, read again, now gives web a proxy.
        let mut fresh = saved.clone();
        fresh[0].proxy_server = Some("socks5://proxy:1080".into());
        assert_eq!(conflicts(&p, &fresh, &Settings::default()), ["web"]);
        let mut settings = Settings::default();
        assert!(apply(&p, &mut fresh, &mut settings));
        assert_eq!(fresh[0].proxy_jump, None);
        assert_eq!(fresh[1].proxy_jump.as_deref(), Some("bastion"));
    }

    #[test]
    fn password_commands_keep_quotes_and_winrm_needs_a_user() {
        let saved = vec![saved_host("win", None), saved_host("named", Some("admin"))];
        let t = [
            target("win", SessionKind::WinRm),
            target("named", SessionKind::WinRm),
        ];
        let settings = Settings::default();
        let cmd = r#"op read "op://Vault/Item/password""#;
        let p = plan(EditField::Password, cmd, &t, &saved, &settings, &[]).unwrap();
        assert_eq!(p.value, cmd);
        assert_eq!(p.changes.len(), 1);
        assert_eq!(p.changes[0].name, "named");
        assert_eq!(p.skipped.len(), 1, "win has no user");
        assert!(plan(EditField::Password, "a\u{7}", &t, &saved, &settings, &[]).is_err());
        // A stored Windows password needs one too.
        let p = plan(
            EditField::Password,
            "windows",
            &t[..1],
            &saved,
            &settings,
            &[],
        )
        .unwrap();
        assert!(p.changes.is_empty());
        let mut settings = Settings::default();
        settings.profile_mut("named").password = PasswordSource::Windows;
        let p = plan(EditField::User, "", &t[1..], &saved, &settings, &[]).unwrap();
        assert!(p.changes.is_empty());
        assert_eq!(p.skipped.len(), 1, "a password source needs a user");
    }

    #[test]
    fn a_winrm_user_cleared_since_the_preview_is_left_alone() {
        let saved = vec![saved_host("win", Some("admin"))];
        let t = [target("win", SessionKind::WinRm)];
        let p = plan(
            EditField::Password,
            "windows",
            &t,
            &saved,
            &Settings::default(),
            &[],
        )
        .unwrap();
        assert_eq!(p.changes.len(), 1);
        let mut fresh = saved.clone();
        fresh[0].user = None;
        let mut settings = Settings::default();
        assert_eq!(conflicts(&p, &fresh, &settings), ["win"]);
        apply(&p, &mut fresh, &mut settings);
        assert!(
            settings
                .profile("win")
                .is_none_or(|p| p.password == PasswordSource::Ask)
        );
    }
}
