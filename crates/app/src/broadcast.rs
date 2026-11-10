//! Broadcast input: what is typed in the focused pane also goes to other
//! panes, to run the same commands on several hosts at once.
//!
//! Window > Broadcast input sends to the panes of the focused tab or of
//! every tab. A pane can be left out (Window > Leave this pane out). Only
//! panes that pass typing straight to their host take part: a pane at a
//! password prompt, the connect prompt or a finished session never gets
//! another pane's keys, and nothing typed into a prompt is broadcast, so a
//! password never reaches the other shells.

/// Which panes receive what is typed in the focused one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Off,
    /// The other panes of the focused tab.
    Tab,
    /// The other panes of every tab.
    All,
}

impl Scope {
    /// The scope after the menu item or shortcut for `wanted` was used:
    /// it turns that scope on, or off again when it was already on.
    pub fn toggle(self, wanted: Scope) -> Scope {
        if self == wanted { Scope::Off } else { wanted }
    }
}

/// The panes that also get what is typed in `from`: in `scope`, not left
/// out, and passing typing to their host (`ready`). `from` must be ready
/// itself, or nothing is broadcast. `tab` holds the focused tab's panes,
/// `all` every pane, both in display order.
pub fn targets<P: Copy + PartialEq>(
    scope: Scope,
    from: P,
    tab: &[P],
    all: &[P],
    left_out: impl Fn(P) -> bool,
    ready: impl Fn(P) -> bool,
) -> Vec<P> {
    let pool = match scope {
        Scope::Off => return Vec::new(),
        Scope::Tab => tab,
        Scope::All => all,
    };
    if !ready(from) {
        return Vec::new();
    }
    pool.iter()
        .copied()
        .filter(|&p| p != from && !left_out(p) && ready(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_switches_between_scopes_and_off() {
        assert_eq!(Scope::Off.toggle(Scope::Tab), Scope::Tab);
        assert_eq!(Scope::Tab.toggle(Scope::Tab), Scope::Off);
        assert_eq!(Scope::Tab.toggle(Scope::All), Scope::All);
        assert_eq!(Scope::All.toggle(Scope::All), Scope::Off);
    }

    #[test]
    fn targets_follow_scope_exclusions_and_readiness() {
        let tab = [1, 2, 3];
        let all = [1, 2, 3, 4, 5];
        let none = |_| false;
        let yes = |_| true;
        assert!(targets(Scope::Off, 1, &tab, &all, none, yes).is_empty());
        assert_eq!(targets(Scope::Tab, 1, &tab, &all, none, yes), [2, 3]);
        assert_eq!(targets(Scope::All, 1, &tab, &all, none, yes), [2, 3, 4, 5]);
        // Left out, and at a prompt.
        let got = targets(Scope::All, 1, &tab, &all, |p| p == 3, |p| p != 5);
        assert_eq!(got, [2, 4]);
    }

    #[test]
    fn nothing_typed_into_a_prompt_is_broadcast() {
        let got = targets(Scope::All, 1, &[1, 2], &[1, 2], |_| false, |p| p != 1);
        assert!(got.is_empty());
    }
}
