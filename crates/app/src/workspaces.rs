//! Workspaces (Window > Workspaces): the window's tabs and split panes
//! saved as a [`ferossh_config::workspace::Workspace`], and one opened as
//! new tabs.
//!
//! This is only the conversion between the window's split trees and the
//! saved ones; the window decides what each pane opens and opens it. It is
//! platform-neutral and tested everywhere.

use std::collections::HashMap;

use ferossh_config::workspace::{Layout, PaneSpec, TabSpec, Workspace};

use crate::layout::{Axis, Node, PaneId, Tab};

/// The tabs as a workspace. `spec` says what each pane opens; a pane it
/// gives `None` for is left out, its neighbour taking its place, and a
/// tab left with no pane goes. `None` when no pane is left at all.
pub fn capture(
    tabs: &[Tab],
    active: usize,
    spec: &mut impl FnMut(PaneId) -> Option<PaneSpec>,
) -> Option<Workspace> {
    let mut out = Workspace::default();
    for (i, tab) in tabs.iter().enumerate() {
        let mut kept = Vec::new();
        let Some(root) = layout_of(&tab.root, spec, &mut kept) else {
            continue;
        };
        if i == active {
            out.active = out.tabs.len();
        }
        let focus = kept.iter().position(|&id| id == tab.focus).unwrap_or(0);
        out.tabs.push(TabSpec { root, focus });
    }
    (!out.tabs.is_empty()).then_some(out)
}

fn layout_of(
    node: &Node,
    spec: &mut impl FnMut(PaneId) -> Option<PaneSpec>,
    kept: &mut Vec<PaneId>,
) -> Option<Layout> {
    match node {
        Node::Leaf(id) => {
            let p = spec(*id)?;
            kept.push(*id);
            Some(Layout::Pane(p))
        }
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => match (layout_of(first, spec, kept), layout_of(second, spec, kept)) {
            (Some(a), Some(b)) => Some(Layout::Split {
                down: *axis == Axis::Column,
                ratio: *ratio,
                first: Box::new(a),
                second: Box::new(b),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

/// A saved tab's split tree as the window's, with a new pane id from
/// `alloc` for each pane; the panes, in order, with what they open go to
/// `panes`.
pub fn node_of(
    layout: &Layout,
    alloc: &mut impl FnMut() -> PaneId,
    panes: &mut Vec<(PaneId, PaneSpec)>,
) -> Node {
    match layout {
        Layout::Pane(p) => {
            let id = alloc();
            panes.push((id, p.clone()));
            Node::Leaf(id)
        }
        Layout::Split {
            down,
            ratio,
            first,
            second,
        } => {
            let first = node_of(first, alloc, panes);
            let second = node_of(second, alloc, panes);
            Node::Split {
                axis: if *down { Axis::Column } else { Axis::Row },
                ratio: if ratio.is_finite() {
                    ratio.clamp(0.1, 0.9)
                } else {
                    0.5
                },
                first: Box::new(first),
                second: Box::new(second),
            }
        }
    }
}

/// Whether panes opening `destination` should share one login: an SSH
/// connection by name or as `user@host[:port]`. Serial ports, `scheme://`
/// addresses and the connection kinds that log in on their own (`kind`
/// is the saved connection's, if any) do not.
pub fn shares_login(destination: &str, ssh_kind: Option<bool>) -> bool {
    let d = destination.trim();
    ssh_kind.unwrap_or(true)
        && !d.is_empty()
        && !d.contains("://")
        && !d.contains(char::is_whitespace)
        && ferossh_config::serial::parse_target(d).is_none()
}

/// An `sftp://` address for `destination` with `folder`, written so
/// [`ferossh_config::ssh_config::sftp_url`] reads the same folder back:
/// an absolute one as it is, one relative to the home folder under `/~/`.
pub fn sftp_address(destination: &str, folder: Option<&str>) -> String {
    let folder = folder
        .filter(|f| !f.is_empty())
        .map(|f| {
            let f = f.replace('%', "%25");
            if f.starts_with('/') {
                f
            } else {
                format!("/~/{f}")
            }
        })
        .unwrap_or_default();
    format!("sftp://{destination}{folder}")
}

/// Panes waiting for another pane on the same host to log in, so they
/// open on its connection instead of logging in again.
#[derive(Debug, Default)]
pub struct Waiting {
    /// Waiting pane: the pane it waits for, the destination and the rest
    /// of what it opens.
    by_pane: HashMap<PaneId, (PaneId, String, PaneSpec)>,
}

impl Waiting {
    pub fn add(&mut self, pane: PaneId, lead: PaneId, destination: String, spec: PaneSpec) {
        self.by_pane.insert(pane, (lead, destination, spec));
    }

    pub fn is_empty(&self) -> bool {
        self.by_pane.is_empty()
    }

    /// What pane `pane` waits to open, if it waits.
    pub fn pending(&self, pane: PaneId) -> Option<PaneSpec> {
        self.by_pane.get(&pane).map(|(_, dest, spec)| {
            let mut spec = spec.clone();
            spec.open = ferossh_config::workspace::Open::Connection(dest.clone());
            spec
        })
    }

    /// Forgets a pane that closed, as a waiting pane.
    pub fn forget(&mut self, pane: PaneId) {
        self.by_pane.remove(&pane);
    }

    /// The panes that waited for `lead`, by pane id, with what they open;
    /// they wait no more.
    pub fn take(&mut self, lead: PaneId) -> Vec<(PaneId, String, PaneSpec)> {
        let (go, stay): (Vec<_>, Vec<_>) = std::mem::take(&mut self.by_pane)
            .into_iter()
            .partition(|(_, (l, _, _))| *l == lead);
        self.by_pane = stay.into_iter().collect();
        let mut out: Vec<(PaneId, String, PaneSpec)> = go
            .into_iter()
            .map(|(pane, (_, dest, spec))| (pane, dest, spec))
            .collect();
        out.sort_by_key(|w| w.0);
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_config::workspace::Open;

    fn plain(open: Open) -> PaneSpec {
        PaneSpec::new(open)
    }

    fn split(axis: Axis, a: Node, b: Node) -> Node {
        Node::Split {
            axis,
            ratio: 0.3,
            first: Box::new(a),
            second: Box::new(b),
        }
    }

    #[test]
    fn tabs_round_trip_through_a_workspace() {
        let tabs = vec![
            Tab {
                root: split(
                    Axis::Row,
                    Node::Leaf(1),
                    split(Axis::Column, Node::Leaf(2), Node::Leaf(3)),
                ),
                focus: 3,
            },
            Tab {
                root: Node::Leaf(4),
                focus: 4,
            },
        ];
        let ws = capture(&tabs, 1, &mut |id| {
            Some(plain(Open::Connection(format!("host{id}"))))
        })
        .unwrap();
        assert_eq!(ws.active, 1);
        assert_eq!(ws.tabs[0].focus, 2);
        let mut next = 10;
        let mut panes = Vec::new();
        let node = node_of(
            &ws.tabs[0].root,
            &mut || {
                next += 1;
                next
            },
            &mut panes,
        );
        assert_eq!(
            node,
            split(
                Axis::Row,
                Node::Leaf(11),
                split(Axis::Column, Node::Leaf(12), Node::Leaf(13)),
            )
        );
        let opens: Vec<_> = panes.iter().map(|(_, p)| p.open.clone()).collect();
        assert_eq!(
            opens,
            ["host1", "host2", "host3"].map(|h| Open::Connection(h.into()))
        );
    }

    #[test]
    fn panes_left_out_close_up() {
        let tabs = vec![
            Tab {
                root: split(Axis::Row, Node::Leaf(1), Node::Leaf(2)),
                focus: 2,
            },
            Tab {
                root: Node::Leaf(3),
                focus: 3,
            },
        ];
        // Pane 2 (a desktop, say) and the whole second tab are left out.
        let ws = capture(&tabs, 1, &mut |id| (id == 1).then(|| plain(Open::Local))).unwrap();
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.active, 0);
        assert_eq!(ws.tabs[0].focus, 0);
        assert_eq!(ws.tabs[0].root, Layout::Pane(plain(Open::Local)));
        assert!(capture(&tabs, 0, &mut |_| None).is_none());
    }

    #[test]
    fn only_ssh_destinations_share_a_login() {
        assert!(shares_login("web1", None));
        assert!(shares_login("root@10.0.0.5:2222", Some(true)));
        assert!(!shares_login("web1", Some(false)));
        assert!(!shares_login("telnet://sw1", None));
        assert!(!shares_login("COM3:115200", None));
        assert!(!shares_login("telnet sw1 23", None));
        assert!(!shares_login(" ", None));
    }

    #[test]
    fn sftp_addresses_keep_their_folder() {
        use ferossh_config::ssh_config::sftp_url;
        for (dest, folder) in [
            ("web1", Some("/var/log")),
            ("me@web1:2222", Some("docs/a b")),
            ("web1", Some("/data/100%")),
            ("web1", None),
        ] {
            let address = sftp_address(dest, folder);
            let (back, f) = sftp_url(&address).unwrap();
            assert_eq!(back, dest, "{address}");
            assert_eq!(f.as_deref(), folder, "{address}");
        }
    }

    #[test]
    fn waiting_panes_go_with_their_lead() {
        let web = || plain(Open::Connection("web1".into()));
        let mut w = Waiting::default();
        let mut tmp = web();
        tmp.dir = Some("/tmp".into());
        w.add(5, 1, "web1".into(), tmp.clone());
        w.add(4, 1, "web1".into(), web());
        w.add(6, 2, "db1".into(), web());
        w.add(7, 2, "db1".into(), web());
        w.forget(6);
        assert_eq!(w.pending(5).unwrap().dir.as_deref(), Some("/tmp"));
        assert!(w.pending(6).is_none());
        assert_eq!(
            w.take(1),
            vec![(4, "web1".into(), web()), (5, "web1".into(), tmp)]
        );
        assert!(!w.is_empty());
        assert_eq!(w.take(2).len(), 1);
        assert!(w.is_empty());
    }
}
