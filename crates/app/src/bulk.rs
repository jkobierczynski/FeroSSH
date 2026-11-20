//! What the connection manager does to several connections at once:
//! the choices its right-click menu offers for a selection, and the
//! changes those choices make. Kept free of Win32 so it can be tested.
//!
//! Tags live in each host's profile, so every host can be tagged; a tag
//! that comes from ssh_config's `Tag` cannot be removed here. Folders and
//! deletes only change connections saved in FeroSSH (`sessions.conf`):
//! hosts from `~/.ssh/config` or an import are left as they are, and
//! counted as skipped.

use ferossh_config::Settings;
use ferossh_config::saved::SavedHost;
use ferossh_config::settings::{MAX_TAGS, same_tag, valid_tag};

use crate::sessions::HostItem;

/// What the right-click menu does to the selected connections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BulkAction {
    /// Connect to each, a tab each.
    OpenAll(Vec<String>),
    AddTag(Vec<String>, String),
    RemoveTag(Vec<String>, String),
    /// Into this folder (`None`: no folder); saved connections only.
    MoveTo(Vec<String>, Option<String>),
    /// One field set on many, as previewed.
    Edit(crate::bulk_edit::Plan),
}

/// The most entries a submenu lists.
pub const MAX_CHOICES: usize = 40;

/// Tags to offer for a selection: `add` holds the tags used anywhere that
/// not every selected host has yet, `remove` the connection-own tags any
/// selected host has. Both sorted, case-insensitively.
pub fn tag_choices(items: &[HostItem], settings: &Settings, picked: &[String]) -> TagChoices {
    let is_picked = |name: &str| picked.iter().any(|p| p.eq_ignore_ascii_case(name));
    let mut all: Vec<String> = Vec::new();
    for h in items {
        for t in &h.tags {
            push_once(&mut all, t);
        }
    }
    let chosen: Vec<&HostItem> = items.iter().filter(|h| is_picked(&h.name)).collect();
    let mut add: Vec<String> = all
        .into_iter()
        .filter(|t| !chosen.iter().all(|h| h.tags.iter().any(|x| same_tag(x, t))))
        .collect();
    let mut remove: Vec<String> = Vec::new();
    for h in &chosen {
        for t in settings
            .profile(&h.name)
            .map(|p| p.tags.as_slice())
            .unwrap_or_default()
        {
            push_once(&mut remove, t);
        }
    }
    for list in [&mut add, &mut remove] {
        list.sort_by_key(|t| t.to_lowercase());
        list.truncate(MAX_CHOICES);
    }
    TagChoices { add, remove }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TagChoices {
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

fn push_once(list: &mut Vec<String>, item: &str) {
    if !list.iter().any(|x| same_tag(x, item)) {
        list.push(item.to_owned());
    }
}

/// Gives every host in `names` tag `tag`; the number changed. A host at
/// [`MAX_TAGS`] already is left out.
pub fn add_tag(settings: &mut Settings, names: &[String], tag: &str) -> usize {
    if !valid_tag(tag) {
        return 0;
    }
    let mut n = 0;
    for name in names {
        let p = settings.profile_mut(name);
        if p.tags.len() < MAX_TAGS && !p.tags.iter().any(|t| same_tag(t, tag)) {
            p.tags.push(tag.to_owned());
            n += 1;
        }
    }
    n
}

/// Takes tag `tag` from every host in `names` that has it as its own;
/// the number changed.
pub fn remove_tag(settings: &mut Settings, names: &[String], tag: &str) -> usize {
    let mut n = 0;
    for p in settings
        .profiles
        .iter_mut()
        .filter(|p| names.iter().any(|x| x.eq_ignore_ascii_case(&p.host)))
    {
        let before = p.tags.len();
        p.tags.retain(|t| !same_tag(t, tag));
        n += usize::from(p.tags.len() != before);
    }
    n
}

/// The folders saved connections are in, sorted, for Move to folder.
pub fn folder_choices(saved: &[SavedHost]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in saved.iter().filter_map(|h| h.folder.as_deref()) {
        push_once(&mut out, f);
    }
    out.sort_by_key(|f| f.to_lowercase());
    out.truncate(MAX_CHOICES);
    out
}

/// What a change to saved connections did: how many changed, and the
/// names it could not change because FeroSSH does not own them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub changed: usize,
    pub skipped: Vec<String>,
}

/// Splits `names` into the saved connections and the rest.
pub fn saved_only(saved: &[SavedHost], names: &[String]) -> (Vec<String>, Vec<String>) {
    names
        .iter()
        .cloned()
        .partition(|n| saved.iter().any(|h| h.alias.eq_ignore_ascii_case(n)))
}

/// Moves the saved connections in `names` into `folder` (`None`: no
/// folder).
pub fn move_to_folder(saved: &mut [SavedHost], names: &[String], folder: Option<&str>) -> Outcome {
    let (mine, skipped) = saved_only(saved, names);
    let mut changed = 0;
    for h in saved
        .iter_mut()
        .filter(|h| mine.iter().any(|n| n.eq_ignore_ascii_case(&h.alias)))
    {
        if h.folder.as_deref() != folder {
            h.folder = folder.map(str::to_owned);
            changed += 1;
        }
    }
    Outcome { changed, skipped }
}

/// Ctrl+click and Shift+click in the connection tree. `order` is the
/// hosts as listed (a host may be listed twice, in Favourites too),
/// `marked` the selection so far, `anchor` where the last plain or Ctrl
/// click was, `current` the tree's own selection and `clicked` where the
/// click is. Returns the new selection and anchor. A plain click clears
/// the selection; Ctrl adds or takes out one host (keeping the host the
/// tree had selected, as Explorer does); Shift selects the run from the
/// anchor, added to the rest with Ctrl too.
pub fn click_marks(
    order: &[String],
    marked: &[String],
    anchor: Option<usize>,
    current: Option<usize>,
    clicked: usize,
    ctrl: bool,
    shift: bool,
) -> (Vec<String>, Option<usize>) {
    let Some(name) = order.get(clicked) else {
        return (marked.to_vec(), anchor);
    };
    let has = |list: &[String], n: &str| list.iter().any(|x| x.eq_ignore_ascii_case(n));
    if shift {
        let from = anchor.or(current).unwrap_or(clicked).min(order.len() - 1);
        let (a, b) = (from.min(clicked), from.max(clicked));
        let mut out = if ctrl { marked.to_vec() } else { Vec::new() };
        for n in order.get(a..=b).unwrap_or_default() {
            if !has(&out, n) {
                out.push(n.clone());
            }
        }
        return (out, Some(from));
    }
    if !ctrl {
        return (Vec::new(), Some(clicked));
    }
    let mut out = marked.to_vec();
    if out.is_empty()
        && let Some(c) = current.and_then(|c| order.get(c))
        && !c.eq_ignore_ascii_case(name)
    {
        out.push(c.clone());
    }
    if has(&out, name) {
        out.retain(|x| !x.eq_ignore_ascii_case(name));
    } else {
        out.push(name.clone());
    }
    (out, Some(clicked))
}

/// A short list of names for a dialog: the first few, then how many more.
pub fn name_list(names: &[String]) -> String {
    const SHOWN: usize = 12;
    let mut out: Vec<String> = names.iter().take(SHOWN).cloned().collect();
    if names.len() > SHOWN {
        out.push(format!("and {} more", names.len() - SHOWN));
    }
    out.join(", ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_config::settings::SessionKind;

    fn item(name: &str, tags: &[&str]) -> HostItem {
        HostItem {
            name: name.into(),
            folder: None,
            detail: String::new(),
            kind: SessionKind::Ssh,
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            user: None,
            hostname: None,
            port: None,
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn tags_offered_added_and_removed() {
        let mut settings = Settings::parse("Profile web\n    Tags prod web\n");
        // db's prod comes from ssh_config's Tag: offered, but not to remove.
        let items = [
            item("web", &["prod", "web"]),
            item("db", &["prod"]),
            item("pi", &["Home"]),
        ];
        let picked = names(&["web", "db"]);
        let c = tag_choices(&items, &settings, &picked);
        assert_eq!(c.add, ["Home", "web"], "prod: both have it");
        assert_eq!(c.remove, ["prod", "web"]);

        assert_eq!(add_tag(&mut settings, &picked, "eu"), 2);
        assert_eq!(add_tag(&mut settings, &picked, "EU"), 0, "once");
        assert_eq!(add_tag(&mut settings, &picked, "bad tag"), 0);
        assert_eq!(settings.profile("db").unwrap().tags, ["eu"]);
        assert_eq!(remove_tag(&mut settings, &picked, "Eu"), 2);
        assert_eq!(remove_tag(&mut settings, &picked, "prod"), 1);
        assert_eq!(settings.profile("web").unwrap().tags, ["web"]);
    }

    #[test]
    fn folders_change_saved_connections_only() {
        let mut saved = vec![
            SavedHost {
                alias: "web".into(),
                folder: Some("Work".into()),
                ..SavedHost::default()
            },
            SavedHost {
                alias: "db".into(),
                ..SavedHost::default()
            },
        ];
        assert_eq!(folder_choices(&saved), ["Work"]);
        let out = move_to_folder(&mut saved, &names(&["WEB", "db", "github"]), Some("Lab"));
        assert_eq!(out.changed, 2);
        assert_eq!(out.skipped, ["github"]);
        assert_eq!(saved[0].folder.as_deref(), Some("Lab"));
        let out = move_to_folder(&mut saved, &names(&["db"]), None);
        assert_eq!(out.changed, 1);
        assert_eq!(saved[1].folder, None);
    }

    #[test]
    fn ctrl_and_shift_clicks() {
        let order = names(&["pi", "web", "db1", "db2", "pi"]);
        // Ctrl+click keeps the host the tree had selected.
        let (m, a) = click_marks(&order, &[], None, Some(1), 2, true, false);
        assert_eq!((m.clone(), a), (names(&["web", "db1"]), Some(2)));
        // Ctrl+click again takes it out.
        let (m2, _) = click_marks(&order, &m, a, Some(2), 2, true, false);
        assert_eq!(m2, ["web"]);
        // Shift selects the run from the anchor, either way round.
        let (m, a) = click_marks(&order, &m, Some(3), None, 1, false, true);
        assert_eq!((m.clone(), a), (names(&["web", "db1", "db2"]), Some(3)));
        // A host listed twice counts once.
        let (m, _) = click_marks(&order, &[], Some(0), None, 4, false, true);
        assert_eq!(m, ["pi", "web", "db1", "db2"]);
        // Ctrl+Shift adds the run to what was there.
        let (m, _) = click_marks(&order, &names(&["pi"]), Some(2), None, 3, true, true);
        assert_eq!(m, ["pi", "db1", "db2"]);
        // A plain click clears.
        assert_eq!(
            click_marks(&order, &m, Some(2), None, 1, false, false),
            (Vec::new(), Some(1))
        );
        // Out of range changes nothing.
        assert_eq!(click_marks(&order, &m, Some(2), None, 9, true, false).0, m);
    }

    #[test]
    fn long_name_lists_are_cut() {
        let many: Vec<String> = (0..15).map(|i| format!("h{i}")).collect();
        assert!(name_list(&many).ends_with("h11, and 3 more"));
        assert_eq!(name_list(&names(&["a", "b"])), "a, b");
    }
}
