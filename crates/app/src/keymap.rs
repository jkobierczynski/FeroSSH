//! Settings > Key bindings: which keys run which menu command.
//!
//! Every action is a menu command id. The window gives its defaults (the
//! shortcuts FeroSSH always had, as [`KeyDefault`]s); the settings file keeps
//! only the actions whose keys were changed ([`Keymap::changed`]). One key
//! runs at most one action: giving a key to an action takes it from the
//! action that had it. Some actions only work in some places, as before:
//! the tab keys with more than one tab, the pane keys with more than one
//! pane; elsewhere their keys go to the terminal ([`When`]).
//!
//! [`check`] says what a key would get in the way of: typing (refused),
//! or keys programs in the terminal use, such as Ctrl+C or a tmux prefix
//! (a warning). Platform-neutral, so it is tested everywhere.

use std::collections::BTreeMap;

use ferossh_config::hotkey::Hotkey;

/// Where an action's keys work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Always,
    /// With more than one tab.
    Tabs,
    /// With more than one pane in the tab.
    Split,
}

/// An action's default keys, as the settings file spells them.
#[derive(Debug, Clone, Copy)]
pub struct KeyDefault {
    pub id: u32,
    pub when: When,
    pub keys: &'static [&'static str],
}

/// Parses a key spelled in a [`KeyDefault`]; a typo there is a bug, caught
/// by the window's tests.
fn key(text: &str) -> Option<Hotkey> {
    Hotkey::parse_key(text).ok()
}

/// The key for a key press: no Win, which never reaches the window.
pub const fn chord(vk: u16, ctrl: bool, alt: bool, shift: bool) -> Hotkey {
    Hotkey {
        ctrl,
        alt,
        shift,
        win: false,
        vk,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    /// The defaults: keys and where they work.
    defaults: BTreeMap<u32, (When, Vec<Hotkey>)>,
    /// The keys each action has now; actions with none are left out.
    keys: BTreeMap<u32, Vec<Hotkey>>,
}

impl Keymap {
    /// The defaults with `changed` (the settings' key bindings) applied:
    /// a changed action has the keys listed. A key listed for two actions
    /// stays with the first changed one.
    pub fn new(defaults: &[KeyDefault], changed: &BTreeMap<u32, Vec<Hotkey>>) -> Self {
        let defaults: BTreeMap<u32, (When, Vec<Hotkey>)> = defaults
            .iter()
            .map(|d| {
                (
                    d.id,
                    (d.when, d.keys.iter().filter_map(|k| key(k)).collect()),
                )
            })
            .collect();
        let mut map = Self {
            keys: defaults
                .iter()
                .filter(|(_, (_, k))| !k.is_empty())
                .map(|(id, (_, k))| (*id, k.clone()))
                .collect(),
            defaults,
        };
        for (id, keys) in changed {
            let mut keys = keys.clone();
            keys.retain(|k| !changed.iter().any(|(o, ks)| o < id && ks.contains(k)));
            map.set(*id, keys);
        }
        map
    }

    /// The action `k` runs, given how many tabs and panes there are.
    pub fn lookup(&self, k: Hotkey, tabs: bool, split: bool) -> Option<u32> {
        let id = self.owner(k)?;
        match self.when(id) {
            When::Always => Some(id),
            When::Tabs => tabs.then_some(id),
            When::Split => split.then_some(id),
        }
    }

    /// The action that has key `k`, wherever it works.
    pub fn owner(&self, k: Hotkey) -> Option<u32> {
        self.keys
            .iter()
            .find(|(_, ks)| ks.contains(&k))
            .map(|(id, _)| *id)
    }

    pub fn when(&self, id: u32) -> When {
        self.defaults.get(&id).map_or(When::Always, |d| d.0)
    }

    /// The keys `id` has now, the main one first.
    pub fn keys(&self, id: u32) -> &[Hotkey] {
        self.keys.get(&id).map_or(&[], Vec::as_slice)
    }

    pub fn default_keys(&self, id: u32) -> &[Hotkey] {
        self.defaults.get(&id).map_or(&[], |d| d.1.as_slice())
    }

    pub fn is_default(&self, id: u32) -> bool {
        self.keys(id) == self.default_keys(id)
    }

    /// Gives `id` exactly `keys`, taking each from the action that had it.
    pub fn set(&mut self, id: u32, mut keys: Vec<Hotkey>) {
        keys.dedup();
        for (other, ks) in &mut self.keys {
            if *other != id {
                ks.retain(|k| !keys.contains(k));
            }
        }
        self.keys.insert(id, keys);
        self.keys.retain(|_, ks| !ks.is_empty());
    }

    /// Adds `k` to `id`'s keys, after the ones it has.
    pub fn add(&mut self, id: u32, k: Hotkey) {
        let mut keys = self.keys(id).to_vec();
        if !keys.contains(&k) {
            keys.push(k);
        }
        self.set(id, keys);
    }

    /// `id`'s default keys that other actions have now: what a reset
    /// would take from them, as (key, action).
    pub fn reset_takes(&self, id: u32) -> Vec<(Hotkey, u32)> {
        self.default_keys(id)
            .iter()
            .filter_map(|k| self.owner(*k).filter(|o| *o != id).map(|o| (*k, o)))
            .collect()
    }

    /// Back to `id`'s default keys (taken from any action that has them).
    pub fn reset(&mut self, id: u32) {
        let keys = self.default_keys(id).to_vec();
        self.set(id, keys);
    }

    /// Every action back to its default keys.
    pub fn reset_all(&mut self) {
        let defaults = std::mem::take(&mut self.defaults);
        *self = Self {
            keys: defaults
                .iter()
                .filter(|(_, (_, k))| !k.is_empty())
                .map(|(id, (_, k))| (*id, k.clone()))
                .collect(),
            defaults,
        };
    }

    /// The actions whose keys differ from their defaults, with the keys
    /// they have: what the settings file keeps.
    pub fn changed(&self) -> BTreeMap<u32, Vec<Hotkey>> {
        self.defaults
            .keys()
            .chain(self.keys.keys())
            .filter(|id| !self.is_default(**id))
            .map(|id| (*id, self.keys(*id).to_vec()))
            .collect()
    }

    /// Whether any action's keys differ from its defaults.
    pub fn any_changed(&self) -> bool {
        !self.changed().is_empty()
    }
}

/// Whether a key pressed in a tool pane's controls (a search box, a
/// list) goes to the main window as a shortcut, rather than to the
/// control. Ctrl+Shift keys and function keys always do; Ctrl with a key
/// an edit box uses for itself (Ctrl+C, Ctrl+V, Ctrl+A, Ctrl+arrows...)
/// and Alt with a letter (a button's underlined letter) stay with it.
pub fn reaches_main_from_tools(k: Hotkey) -> bool {
    let function = (0x70..=0x87).contains(&k.vk);
    if function || (k.ctrl && k.shift) {
        return true;
    }
    // Ctrl+letters edit boxes and lists use: select all, copy, paste,
    // cut, undo, redo; and the keys that move the caret or delete.
    const EDIT: [u16; 6] = [
        b'A' as u16,
        b'C' as u16,
        b'V' as u16,
        b'X' as u16,
        b'Z' as u16,
        b'Y' as u16,
    ];
    let moves = matches!(k.vk, 0x08 | 0x09 | 0x0D | 0x1B | 0x20..=0x28 | 0x2D | 0x2E);
    if k.ctrl && !k.alt {
        return !(EDIT.contains(&k.vk) || moves);
    }
    if k.alt && !k.ctrl {
        // Alt+letter presses a button; Alt+arrows and the like do not.
        return !k.types_text();
    }
    false
}

/// What a key would mean for an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Fine,
    /// It can be used, but the user should know this first.
    Warn(String),
    /// It would stop typing working: not taken.
    Refuse(String),
}

/// What giving key `k` to an action would get in the way of. `label` is
/// the key as people read it; `tmux_prefix` the prefix of the tmux
/// sessions open now, if FeroSSH knows one (Ctrl+B otherwise).
pub fn check(k: Hotkey, label: &str, tmux_prefix: Option<Hotkey>) -> Check {
    let editing = matches!(k.vk, 0x08 | 0x09 | 0x0D | 0x1B);
    if k.win {
        return Check::Refuse(format!(
            "{label} goes to Windows, not to FeroSSH. Pick a key without Win."
        ));
    }
    if !k.ctrl && !k.alt && (k.types_text() || editing) {
        return Check::Refuse(format!(
            "{label} types in the terminal and in every box. Add Ctrl or Alt."
        ));
    }
    if k.ctrl && k.alt && k.types_text() && k.vk != 0x20 {
        return Check::Refuse(format!(
            "{label} is AltGr with that key on many keyboards, so it would stop you typing \
             its character. Pick another key."
        ));
    }
    if k.vk == 0x1B {
        return Check::Refuse(format!("{label} closes boxes and menus. Pick another key."));
    }
    let prefix = tmux_prefix.unwrap_or(chord(u16::from(b'B'), true, false, false));
    if k == prefix {
        return Check::Warn(format!(
            "{label} is the tmux prefix. With this binding tmux in the terminal never gets \
             it, and FeroSSH's tmux keybindings (Session menu) stop working."
        ));
    }
    if (0x70..0x7C).contains(&k.vk) && k.ctrl && k.shift && !k.alt {
        return Check::Warn(format!(
            "Scripts can have Ctrl+Shift+F1 to F12 as hotkeys. A script with {label} as its \
             hotkey runs instead of this action on the hosts it is for."
        ));
    }
    if k.ctrl && k.shift {
        return Check::Fine;
    }
    if k.ctrl && !k.alt && k.types_text() {
        return Check::Warn(format!(
            "Programs in the terminal use {label} (Ctrl+C stops a program, Ctrl+R searches \
             the shell's history, Ctrl+A and Ctrl+B are common tmux and screen prefixes). \
             With this binding they never get it."
        ));
    }
    if k.alt && !k.ctrl && k.types_text() {
        return Check::Warn(format!(
            "Shells and editors in the terminal use {label} (Alt+B and Alt+F move by word), \
             and Alt with a letter opens the menu that has it underlined. With this binding \
             they never get it."
        ));
    }
    Check::Warn(format!(
        "Programs in the terminal may use {label} (editors, pagers and tmux often do). With \
         this binding they never get it; keys with Ctrl+Shift are the safest."
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const COPY: u32 = 500;
    const PASTE: u32 = 501;
    const NEXT_TAB: u32 = 702;
    const PALETTE: u32 = 213;
    const DEFAULTS: [KeyDefault; 4] = [
        KeyDefault {
            id: COPY,
            when: When::Always,
            keys: &["Ctrl+Shift+C", "Ctrl+Insert"],
        },
        KeyDefault {
            id: PASTE,
            when: When::Always,
            keys: &["Ctrl+Shift+V", "Shift+Insert"],
        },
        KeyDefault {
            id: NEXT_TAB,
            when: When::Tabs,
            keys: &["Ctrl+Tab"],
        },
        KeyDefault {
            id: PALETTE,
            when: When::Always,
            keys: &["Ctrl+Shift+A"],
        },
    ];

    fn k(t: &str) -> Hotkey {
        Hotkey::parse_key(t).unwrap()
    }

    fn map() -> Keymap {
        Keymap::new(&DEFAULTS, &BTreeMap::new())
    }

    #[test]
    fn defaults_look_up_and_respect_where_they_work() {
        let m = map();
        assert_eq!(m.lookup(k("Ctrl+Shift+C"), false, false), Some(COPY));
        assert_eq!(m.lookup(k("Ctrl+Insert"), false, false), Some(COPY));
        assert_eq!(m.lookup(k("Ctrl+C"), true, true), None);
        // Ctrl+Tab only with tabs; without, it goes to the terminal.
        assert_eq!(m.lookup(k("Ctrl+Tab"), false, false), None);
        assert_eq!(m.lookup(k("Ctrl+Tab"), true, false), Some(NEXT_TAB));
        assert!(m.is_default(COPY));
        assert!(!m.any_changed());
        assert!(m.changed().is_empty());
    }

    #[test]
    fn a_key_moves_from_the_action_that_had_it() {
        let mut m = map();
        m.set(PALETTE, vec![k("Ctrl+Shift+C")]);
        assert_eq!(m.owner(k("Ctrl+Shift+C")), Some(PALETTE));
        assert_eq!(m.keys(COPY), &[k("Ctrl+Insert")]);
        assert_eq!(m.lookup(k("Ctrl+Shift+A"), false, false), None);
        // Both changed now: the settings keep both.
        let changed = m.changed();
        assert_eq!(changed.get(&PALETTE), Some(&vec![k("Ctrl+Shift+C")]));
        assert_eq!(changed.get(&COPY), Some(&vec![k("Ctrl+Insert")]));
        // And a window built from them has the same keys.
        assert_eq!(Keymap::new(&DEFAULTS, &changed), m);
    }

    #[test]
    fn reset_one_and_all() {
        let mut m = map();
        m.set(PALETTE, vec![k("Ctrl+Shift+C")]);
        m.add(PASTE, k("F7"));
        assert_eq!(
            m.keys(PASTE),
            &[k("Ctrl+Shift+V"), k("Shift+Insert"), k("F7")]
        );
        // Copy's default key is the palette's now: a reset takes it back.
        assert_eq!(m.reset_takes(COPY), vec![(k("Ctrl+Shift+C"), PALETTE)]);
        m.reset(COPY);
        assert!(m.is_default(COPY));
        assert_eq!(m.keys(PALETTE), &[] as &[Hotkey]);
        assert_eq!(m.changed().get(&PALETTE), Some(&vec![]));
        m.reset_all();
        assert_eq!(m, map());
    }

    #[test]
    fn an_action_without_keys_stays_without() {
        let mut changed = BTreeMap::new();
        changed.insert(COPY, vec![]);
        let m = Keymap::new(&DEFAULTS, &changed);
        assert_eq!(m.lookup(k("Ctrl+Shift+C"), false, false), None);
        assert_eq!(m.changed(), changed);
    }

    #[test]
    fn a_key_saved_twice_stays_with_the_first() {
        let mut changed = BTreeMap::new();
        changed.insert(PALETTE, vec![k("F7")]);
        changed.insert(NEXT_TAB, vec![k("F7"), k("F8")]);
        let m = Keymap::new(&DEFAULTS, &changed);
        assert_eq!(m.owner(k("F7")), Some(PALETTE));
        assert_eq!(m.keys(NEXT_TAB), &[k("F8")]);
    }

    #[test]
    fn checks_refuse_typing_and_warn_about_terminal_keys() {
        let c = |t: &str| check(k(t), t, None);
        assert!(matches!(c("A"), Check::Refuse(_)));
        assert!(matches!(c("Shift+A"), Check::Refuse(_)));
        assert!(matches!(c("Enter"), Check::Refuse(_)));
        assert!(matches!(c("Ctrl+Alt+Q"), Check::Refuse(_)));
        assert!(matches!(c("Win+A"), Check::Refuse(_)));
        assert_eq!(c("Ctrl+Shift+K"), Check::Fine);
        let warn = |c: Check| match c {
            Check::Warn(why) => why,
            other => format!("not a warning: {other:?}"),
        };
        assert!(warn(c("Ctrl+B")).contains("is the tmux prefix"));
        // Another prefix: Ctrl+B is then an ordinary Ctrl key.
        let other = warn(check(k("Ctrl+B"), "Ctrl+B", Some(k("Ctrl+A"))));
        assert!(other.starts_with("Programs in the terminal"), "{other}");
        assert!(matches!(c("Ctrl+K"), Check::Warn(_)));
        assert!(matches!(c("Alt+K"), Check::Warn(_)));
        assert!(matches!(c("F5"), Check::Warn(_)));
        assert!(warn(c("Ctrl+Shift+F3")).starts_with("Scripts"));
    }

    #[test]
    fn tool_controls_keep_their_editing_keys() {
        let r = |t: &str| reaches_main_from_tools(k(t));
        assert!(r("Ctrl+Shift+A"));
        assert!(r("F11"));
        assert!(r("Ctrl+K"));
        assert!(r("Alt+Left"));
        assert!(!r("Ctrl+C"));
        assert!(!r("Ctrl+Left"));
        assert!(!r("Ctrl+Insert"));
        assert!(!r("Alt+O"));
        assert!(!r("Shift+Insert"));
    }
}
