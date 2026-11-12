//! Workspaces (Window > Workspaces): the window's tabs and split panes
//! saved as a [`ferossh_config::workspace::Workspace`], and one opened as
//! new tabs.
//!
//! This is only the conversion between the window's split trees and the
//! saved ones; the window decides what each pane opens and opens it. It is
//! platform-neutral and tested everywhere.

use std::collections::HashMap;

use ferossh_config::workspace::{Layout, Open, PaneSpec, TabSpec, Workspace};

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

/// A tmux session a workspace opens: the connection its panes are on,
/// its name, and its tabs (its own panes only, each a tmux window) to
/// make again when the session is gone.
#[derive(Debug, Clone, PartialEq)]
pub struct TmuxPlan {
    pub destination: String,
    pub session: String,
    pub tabs: Vec<TabSpec>,
    pub active: usize,
}

/// The connection and session a pane opens as a tmux window, if it does.
fn tmux_key(p: &PaneSpec) -> Option<(String, String)> {
    match (&p.open, &p.tmux) {
        (Open::Connection(dest), Some(name)) => Some((
            dest.trim().to_owned(),
            ferossh_tmuxcc::command::session_name(name),
        )),
        _ => None,
    }
}

/// `layout` with only the panes `keep` takes (by their index in the
/// tab); a neighbour takes a dropped pane's place. `kept` gets the
/// indexes kept, in order.
fn retain(
    layout: &Layout,
    index: &mut usize,
    keep: &mut impl FnMut(usize, &PaneSpec) -> bool,
    kept: &mut Vec<usize>,
) -> Option<Layout> {
    match layout {
        Layout::Pane(p) => {
            let i = *index;
            *index += 1;
            keep(i, p).then(|| {
                kept.push(i);
                Layout::Pane(p.clone())
            })
        }
        Layout::Split {
            down,
            ratio,
            first,
            second,
        } => match (
            retain(first, index, keep, kept),
            retain(second, index, keep, kept),
        ) {
            (Some(a), Some(b)) => Some(Layout::Split {
                down: *down,
                ratio: *ratio,
                first: Box::new(a),
                second: Box::new(b),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

/// The tabs of `ws` with only the panes `keep` takes (tab, index in tab,
/// pane), each tab's focus following its pane; an emptied tab goes. The
/// active tab, or 0 when it went.
fn retain_tabs(
    ws: &Workspace,
    keep: &mut impl FnMut(usize, usize, &PaneSpec) -> bool,
) -> (Vec<TabSpec>, usize) {
    let mut tabs = Vec::new();
    let mut active = 0;
    for (t, tab) in ws.tabs.iter().enumerate() {
        let mut kept = Vec::new();
        let Some(root) = retain(&tab.root, &mut 0, &mut |i, p| keep(t, i, p), &mut kept) else {
            continue;
        };
        if t == ws.active {
            active = tabs.len();
        }
        let focus = kept.iter().position(|&i| i == tab.focus).unwrap_or(0);
        tabs.push(TabSpec { root, focus });
    }
    (tabs, active)
}

/// Takes the tmux sessions out of `ws`. A session's first pane (its
/// lead) stays and opens the session; its other panes leave the
/// workspace, as tmux shows them once attached. The plans, by session.
pub fn take_tmux(ws: &mut Workspace) -> Vec<TmuxPlan> {
    let mut plans: Vec<TmuxPlan> = Vec::new();
    let mut leads: HashMap<(String, String), (usize, usize)> = HashMap::new();
    for (t, tab) in ws.tabs.iter().enumerate() {
        for (i, p) in tab.root.panes().into_iter().enumerate() {
            if let Some(key) = tmux_key(p) {
                leads.entry(key).or_insert((t, i));
            }
        }
    }
    let mut order: Vec<_> = leads.iter().map(|(k, at)| (*at, k.clone())).collect();
    order.sort();
    for (_, (destination, session)) in order {
        let key = (destination.clone(), session.clone());
        let (tabs, active) = retain_tabs(ws, &mut |_, _, p| tmux_key(p).as_ref() == Some(&key));
        plans.push(TmuxPlan {
            destination,
            session,
            tabs,
            active,
        });
    }
    let (tabs, active) = retain_tabs(ws, &mut |t, i, p| match tmux_key(p) {
        Some(key) => leads.get(&key) == Some(&(t, i)),
        None => true,
    });
    ws.tabs = tabs;
    ws.active = active;
    plans
}

/// `dir` for a POSIX shell: single-quoted, a path relative to the home
/// folder under `"$HOME"`. Empty when there is none or it holds control
/// characters.
fn sh_dir(dir: &str) -> String {
    let dir = dir.trim();
    if dir.is_empty() || dir.chars().any(char::is_control) {
        return String::new();
    }
    if dir.starts_with('/') {
        sh_quote(dir)
    } else if dir == "~" || dir == "." {
        "\"$HOME\"".to_owned()
    } else {
        let rel = dir.strip_prefix("~/").unwrap_or(dir);
        format!("\"$HOME\"/{}", sh_quote(rel))
    }
}

/// Stand-ins for window ids in the saved layout, replaced by shell variables.
const STAND_IN: u32 = 4_000_000_000;

/// What to type before attaching session `plan`: the [`tmux_script`]
/// under `sh`, whatever the login shell, ending in `; ` for the attach.
pub fn tmux_rebuild(plan: &TmuxPlan) -> Option<String> {
    let script = tmux_script(plan)?;
    Some(format!(" sh -c {}; ", sh_quote(&script)))
}

/// `text` single-quoted for a POSIX shell (and fish and tcsh).
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The shell command that makes session `plan` again when it is gone:
/// its windows, each in its directory, and the FeroSSH layout that
/// places them, which tmux integration then shows on attach. `None` when
/// the session has a single pane and nothing to place.
fn tmux_script(plan: &TmuxPlan) -> Option<String> {
    use ferossh_tmuxcc::saved::{Look, Node as SNode, Saved, Tab as STab};
    fn node(l: &Layout, n: &mut u32, specs: &mut Vec<PaneSpec>) -> SNode {
        match l {
            Layout::Pane(p) => {
                specs.push(p.clone());
                let w = STAND_IN + *n;
                *n += 1;
                SNode::Window(w)
            }
            Layout::Split {
                down,
                ratio,
                first,
                second,
            } => {
                let a = node(first, n, specs);
                let b = node(second, n, specs);
                let r = if ratio.is_finite() { *ratio } else { 0.5 };
                SNode::Split {
                    row: !down,
                    ratio: (r.clamp(0.1, 0.9) * 1000.0).round() as u16,
                    first: Box::new(a),
                    second: Box::new(b),
                }
            }
        }
    }
    let mut n = 0;
    let mut specs = Vec::new();
    let mut saved = Saved {
        active: plan.active,
        ..Saved::default()
    };
    for tab in &plan.tabs {
        let first = n;
        let root = node(&tab.root, &mut n, &mut specs);
        let focus = STAND_IN + first + u32::try_from(tab.focus).unwrap_or(0);
        let focus = if focus < STAND_IN + n {
            focus
        } else {
            STAND_IN + first
        };
        saved.tabs.push(STab { root, focus });
    }
    if let [only] = specs.as_slice()
        && only.dir.is_none()
        && only.name.is_none()
    {
        return None;
    }
    for (i, p) in specs.iter().enumerate() {
        let look = Look {
            font: p.font.clone(),
            size: p.font_size,
            theme: p.theme.clone(),
            name: p.name.clone(),
        };
        if look != Look::default() {
            saved
                .looks
                .insert(STAND_IN + u32::try_from(i).unwrap_or(0), look);
        }
    }
    // The text is ASCII letters, digits and `%(),./=@-_`: safe in double
    // quotes once each stand-in (all ten digits) is its variable.
    let mut text = saved.to_text();
    for i in (0..specs.len()).rev() {
        text = text.replace(&format!("@{}", STAND_IN as usize + i), &format!("$w{i}"));
    }
    let s = &plan.session;
    let mut steps = Vec::new();
    for (i, p) in specs.iter().enumerate() {
        let dir = p.dir.as_deref().map(sh_dir).unwrap_or_default();
        let dir = if dir.is_empty() {
            String::new()
        } else {
            format!(" -c {dir}")
        };
        steps.push(if i == 0 {
            format!("w0=$(tmux new-session -d -s {s} -P -F '#{{window_id}}'{dir})")
        } else {
            format!("w{i}=$(tmux new-window -d -t {s}: -P -F '#{{window_id}}'{dir})")
        });
    }
    steps.push(format!(
        "tmux set-option -t {s} {} ferossh",
        ferossh_tmuxcc::saved::MODE_OPTION
    ));
    steps.push(format!(
        "tmux set-option -t {s} {} \"{text}\"",
        ferossh_tmuxcc::saved::OPTION
    ));
    Some(format!(
        "tmux has-session -t ={s} 2>/dev/null || {{ {}; }}",
        steps.join(" && ")
    ))
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

    fn tmux_pane(dest: &str, session: &str, dir: Option<&str>) -> Layout {
        let mut p = plain(Open::Connection(dest.into()));
        p.tmux = Some(session.into());
        p.dir = dir.map(Into::into);
        Layout::Pane(p)
    }

    fn lsplit(down: bool, a: Layout, b: Layout) -> Layout {
        Layout::Split {
            down,
            ratio: 0.6,
            first: Box::new(a),
            second: Box::new(b),
        }
    }

    #[test]
    fn a_tmux_session_opens_from_its_first_pane() {
        let mut ws = Workspace {
            tabs: vec![
                TabSpec {
                    root: lsplit(
                        false,
                        Layout::Pane(plain(Open::Local)),
                        lsplit(
                            true,
                            tmux_pane("web1", "main", Some("/srv")),
                            tmux_pane("web1", "main", None),
                        ),
                    ),
                    focus: 2,
                },
                TabSpec {
                    root: lsplit(
                        false,
                        tmux_pane("web1", "main", Some("logs")),
                        tmux_pane("web1", "other", None),
                    ),
                    focus: 0,
                },
                TabSpec {
                    root: tmux_pane("web1", "main", None),
                    focus: 0,
                },
            ],
            active: 1,
        };
        let plans = take_tmux(&mut ws);
        // Left: the local shell beside main's first pane, and other's
        // pane; the third tab only held main's panes.
        assert_eq!(ws.tabs.len(), 2);
        assert_eq!(
            ws.tabs[0].root,
            lsplit(
                false,
                Layout::Pane(plain(Open::Local)),
                tmux_pane("web1", "main", Some("/srv"))
            )
        );
        assert_eq!(ws.tabs[0].focus, 0);
        assert_eq!(ws.tabs[1].root, tmux_pane("web1", "other", None));
        assert_eq!(ws.active, 1);
        assert_eq!(plans.len(), 2);
        let main = &plans[0];
        assert_eq!(
            (main.destination.as_str(), main.session.as_str()),
            ("web1", "main")
        );
        assert_eq!(main.tabs.len(), 3);
        assert_eq!(main.tabs[0].focus, 1);
        assert_eq!(main.tabs[1].root, tmux_pane("web1", "main", Some("logs")));
        assert_eq!(main.active, 1);
        assert_eq!(plans[1].session, "other");
        assert_eq!(plans[1].tabs.len(), 1);
    }

    #[test]
    fn a_gone_tmux_session_is_made_again() {
        let mut named = plain(Open::Connection("web1".into()));
        named.tmux = Some("main".into());
        named.name = Some("db shell".into());
        named.theme = Some("Nord".into());
        let plan = TmuxPlan {
            destination: "web1".into(),
            session: "main".into(),
            tabs: vec![TabSpec {
                root: lsplit(
                    false,
                    tmux_pane("web1", "main", Some("/tmp/it's here")),
                    lsplit(
                        true,
                        tmux_pane("web1", "main", Some("~/logs")),
                        Layout::Pane(named),
                    ),
                ),
                focus: 2,
            }],
            active: 0,
        };
        assert_eq!(
            tmux_script(&plan).unwrap(),
            r#"tmux has-session -t =main 2>/dev/null || { w0=$(tmux new-session -d -s main -P -F '#{window_id}' -c '/tmp/it'\''s here') && w1=$(tmux new-window -d -t main: -P -F '#{window_id}' -c "$HOME"/'logs') && w2=$(tmux new-window -d -t main: -P -F '#{window_id}') && tmux set-option -t main @ferossh-mode ferossh && tmux set-option -t main @ferossh-layout "ferossh1 active=0 tab=r600($w0,c600($w1,$w2))/$w2 look=$w2,,,Nord,db%20shell"; }"#
        );
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        // One pane with nothing to place: tmux makes it on attach.
        let alone = TmuxPlan {
            tabs: vec![TabSpec {
                root: tmux_pane("web1", "main", None),
                focus: 0,
            }],
            ..plan
        };
        assert!(tmux_rebuild(&alone).is_none());
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
