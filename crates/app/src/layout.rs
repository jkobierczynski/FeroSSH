//! Tabs and splits: each tab is a tree of panes split side by side or one
//! above the other, with a focused pane.
//!
//! This is only geometry and bookkeeping over pane ids; the window owns
//! the panes themselves. It is platform-neutral and tested everywhere.

/// Names one pane for the window's lifetime.
pub type PaneId = u32;

/// A rectangle in client pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    fn centre(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// How a split arranges its two children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Side by side, with a vertical divider.
    Row,
    /// One above the other, with a horizontal divider.
    Column,
}

/// A direction, for moving focus, resizing and where a new pane goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    fn axis(self) -> Axis {
        match self {
            Dir::Left | Dir::Right => Axis::Row,
            Dir::Up | Dir::Down => Axis::Column,
        }
    }

    /// Whether this direction points to a split's second child.
    fn forward(self) -> bool {
        matches!(self, Dir::Right | Dir::Down)
    }
}

/// Smallest share of a split either side keeps.
const MIN_RATIO: f32 = 0.1;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// The first child's share of the space, 0.1 to 0.9.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A divider between two children of a split, for dragging.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    /// The divider's own strip, as hit area.
    pub rect: Rect,
    pub axis: Axis,
    /// The split's whole area, to turn a mouse position into a ratio.
    pub span: Rect,
    /// Which split: left (`false`) and right (`true`) turns from the root.
    pub path: Vec<bool>,
}

/// Splits `len` pixels by `ratio`, leaving `gap` between the two parts.
fn split_len(len: i32, gap: i32, ratio: f32) -> (i32, i32) {
    let avail = (len - gap).max(0);
    let first = ((avail as f32) * ratio).round() as i32;
    (first, avail - first)
}

fn halves(r: Rect, axis: Axis, ratio: f32, gap: i32) -> (Rect, Rect, Rect) {
    match axis {
        Axis::Row => {
            let (a, b) = split_len(r.w, gap, ratio);
            (
                Rect::new(r.x, r.y, a, r.h),
                Rect::new(r.x + a + gap, r.y, b, r.h),
                Rect::new(r.x + a, r.y, gap, r.h),
            )
        }
        Axis::Column => {
            let (a, b) = split_len(r.h, gap, ratio);
            (
                Rect::new(r.x, r.y, r.w, a),
                Rect::new(r.x, r.y + a + gap, r.w, b),
                Rect::new(r.x, r.y + a, r.w, gap),
            )
        }
    }
}

impl Node {
    /// The pane ids, first to last (left to right, top to bottom).
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(p) => *p == id,
            Node::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Splits pane `target`, putting `new` on side `side` of it with half
    /// the space. `false` if `target` is not in the tree.
    pub fn split(&mut self, target: PaneId, new: PaneId, side: Dir) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let old = Box::new(Node::Leaf(target));
                let fresh = Box::new(Node::Leaf(new));
                let (first, second) = if side.forward() {
                    (old, fresh)
                } else {
                    (fresh, old)
                };
                *self = Node::Split {
                    axis: side.axis(),
                    ratio: 0.5,
                    first,
                    second,
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split(target, new, side) || second.split(target, new, side)
            }
        }
    }

    /// The tree without pane `id`; its sibling takes its place. `None`
    /// when `id` was the only pane.
    #[must_use]
    pub fn remove(self, id: PaneId) -> Option<Node> {
        match self {
            Node::Leaf(p) if p == id => None,
            leaf @ Node::Leaf(_) => Some(leaf),
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.remove(id), second.remove(id)) {
                (Some(a), Some(b)) => Some(Node::Split {
                    axis,
                    ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// Exchanges two panes' places.
    pub fn swap(&mut self, a: PaneId, b: PaneId) {
        match self {
            Node::Leaf(p) if *p == a => *p = b,
            Node::Leaf(p) if *p == b => *p = a,
            Node::Leaf(_) => {}
            Node::Split { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
        }
    }

    /// Where each pane goes in `area`, with `gap` pixels between panes.
    pub fn layout(&self, area: Rect, gap: i32) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.layout_into(area, gap, &mut out);
        out
    }

    fn layout_into(&self, area: Rect, gap: i32, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, area)),
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, b, _) = halves(area, *axis, *ratio, gap);
                first.layout_into(a, gap, out);
                second.layout_into(b, gap, out);
            }
        }
    }

    /// Every divider in `area`.
    pub fn dividers(&self, area: Rect, gap: i32) -> Vec<Divider> {
        let mut out = Vec::new();
        self.dividers_into(area, gap, &mut Vec::new(), &mut out);
        out
    }

    fn dividers_into(&self, area: Rect, gap: i32, path: &mut Vec<bool>, out: &mut Vec<Divider>) {
        if let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        {
            let (a, b, line) = halves(area, *axis, *ratio, gap);
            out.push(Divider {
                rect: line,
                axis: *axis,
                span: area,
                path: path.clone(),
            });
            path.push(false);
            first.dividers_into(a, gap, path, out);
            path.pop();
            path.push(true);
            second.dividers_into(b, gap, path, out);
            path.pop();
        }
    }

    fn at_path(&mut self, path: &[bool]) -> Option<&mut Node> {
        match (path.split_first(), self) {
            (None, node) => Some(node),
            (Some((&turn, rest)), Node::Split { first, second, .. }) => {
                if turn { second } else { first }.at_path(rest)
            }
            (Some(_), Node::Leaf(_)) => None,
        }
    }

    /// Sets the share of the split at `path`.
    pub fn set_ratio(&mut self, path: &[bool], value: f32) {
        if let Some(Node::Split { ratio, .. }) = self.at_path(path) {
            *ratio = value.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        }
    }

    /// Moves the edge of pane `id` that faces `dir` by `step` of the
    /// enclosing split: the nearest split along that axis where the pane
    /// has a neighbour on that side. `false` if there is none.
    pub fn resize(&mut self, id: PaneId, dir: Dir, step: f32) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let in_first = first.contains(id);
                if !in_first && !second.contains(id) {
                    return false;
                }
                // A deeper split gets the first chance.
                let child = if in_first { first } else { second };
                if child.resize(id, dir, step) {
                    return true;
                }
                // The pane's edge on `dir`'s side is this divider when the
                // pane is in the first child and dir points forward, or in
                // the second child and dir points back.
                if *axis != dir.axis() || in_first != dir.forward() {
                    return false;
                }
                let delta = if dir.forward() { step } else { -step };
                *ratio = (*ratio + delta).clamp(MIN_RATIO, 1.0 - MIN_RATIO);
                true
            }
        }
    }
}

/// The pane next to `from` in direction `dir`: of the panes that lie on
/// that side and overlap it across, the nearest, then the one whose middle
/// is closest.
pub fn neighbour(layout: &[(PaneId, Rect)], from: PaneId, dir: Dir) -> Option<PaneId> {
    let (_, r) = layout.iter().find(|(id, _)| *id == from)?;
    let (cx, cy) = r.centre();
    layout
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|(id, o)| {
            let (gap, overlap, off) = match dir {
                Dir::Left => (
                    r.x - o.right(),
                    o.y < r.bottom() && o.bottom() > r.y,
                    cy - o.centre().1,
                ),
                Dir::Right => (
                    o.x - r.right(),
                    o.y < r.bottom() && o.bottom() > r.y,
                    cy - o.centre().1,
                ),
                Dir::Up => (
                    r.y - o.bottom(),
                    o.x < r.right() && o.right() > r.x,
                    cx - o.centre().0,
                ),
                Dir::Down => (
                    o.y - r.bottom(),
                    o.x < r.right() && o.right() > r.x,
                    cx - o.centre().0,
                ),
            };
            (gap >= 0 && overlap).then_some((gap, off.abs(), *id))
        })
        .min()
        .map(|(_, _, id)| id)
}

/// Where a dragged pane lands relative to the pane under the mouse: on a
/// side (split there) or the middle (swap).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    Side(Dir),
    Swap,
}

/// The drop zone for a point in `r`: the outer quarter on each side
/// splits, the middle swaps.
pub fn drop_zone(r: Rect, x: i32, y: i32) -> Drop {
    let fx = (x - r.x) as f32 / r.w.max(1) as f32;
    let fy = (y - r.y) as f32 / r.h.max(1) as f32;
    let edges = [
        (fx, Dir::Left),
        (1.0 - fx, Dir::Right),
        (fy, Dir::Up),
        (1.0 - fy, Dir::Down),
    ];
    match edges
        .iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .filter(|(d, _)| *d < 0.25)
    {
        Some((_, dir)) => Drop::Side(*dir),
        None => Drop::Swap,
    }
}

/// One tab: its split tree and which pane has the keyboard.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub root: Node,
    pub focus: PaneId,
}

/// What closing a pane did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    Pane,
    /// It was the tab's last pane; the tab went too.
    Tab,
    /// It was the window's last pane.
    Window,
}

/// The window's tabs.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_id: PaneId,
}

impl Workspace {
    /// One tab with one pane; returns the workspace and that pane's id.
    pub fn new() -> (Self, PaneId) {
        let ws = Self {
            tabs: vec![Tab {
                root: Node::Leaf(1),
                focus: 1,
            }],
            active: 0,
            next_id: 2,
        };
        (ws, 1)
    }

    fn fresh_id(&mut self) -> PaneId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// The focused pane of the active tab.
    pub fn focused(&self) -> PaneId {
        self.tab().map_or(0, |t| t.focus)
    }

    /// The tab index holding pane `id`.
    pub fn tab_of(&self, id: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| t.root.contains(id))
    }

    /// Every pane in every tab.
    pub fn all_panes(&self) -> Vec<PaneId> {
        self.tabs.iter().flat_map(|t| t.root.panes()).collect()
    }

    /// Opens a tab after the active one and switches to it.
    pub fn new_tab(&mut self) -> PaneId {
        let id = self.fresh_id();
        let at = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(
            at,
            Tab {
                root: Node::Leaf(id),
                focus: id,
            },
        );
        self.active = at;
        id
    }

    /// Splits the focused pane; the new pane takes the focus.
    pub fn split(&mut self, side: Dir) -> Option<PaneId> {
        let id = self.fresh_id();
        let tab = self.tabs.get_mut(self.active)?;
        if !tab.root.split(tab.focus, id, side) {
            return None;
        }
        tab.focus = id;
        Some(id)
    }

    /// Focuses pane `id` and its tab.
    pub fn focus(&mut self, id: PaneId) -> bool {
        let Some(i) = self.tab_of(id) else {
            return false;
        };
        self.active = i;
        if let Some(t) = self.tabs.get_mut(i) {
            t.focus = id;
        }
        true
    }

    /// The next (or previous) tab, wrapping.
    pub fn cycle_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        if n > 1 {
            self.active = if forward {
                (self.active + 1) % n
            } else {
                (self.active + n - 1) % n
            };
        }
    }

    /// Moves tab `from` to position `to`; the moved tab stays active if it
    /// was.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let active_id = self.focused();
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        if let Some(i) = self.tab_of(active_id) {
            self.active = i;
        }
    }

    /// Takes pane `id` out of its tab, dropping the tab if it empties.
    /// The tab's focus moves to its first remaining pane.
    fn detach(&mut self, id: PaneId) -> Option<Closed> {
        let i = self.tab_of(id)?;
        let tab = self.tabs.get_mut(i)?;
        let root = std::mem::replace(&mut tab.root, Node::Leaf(0));
        match root.remove(id) {
            Some(rest) => {
                if tab.focus == id {
                    tab.focus = rest.panes().first().copied().unwrap_or(0);
                }
                tab.root = rest;
                Some(Closed::Pane)
            }
            None => {
                self.tabs.remove(i);
                if self.tabs.is_empty() {
                    return Some(Closed::Window);
                }
                if self.active > i || self.active >= self.tabs.len() {
                    self.active = self.active.saturating_sub(1);
                }
                Some(Closed::Tab)
            }
        }
    }

    /// Closes pane `id`. The focus goes to its neighbour.
    pub fn close(&mut self, id: PaneId) -> Option<Closed> {
        let closed = self.detach(id)?;
        if closed == Closed::Window {
            // Keep the window's state valid until it closes.
            self.tabs.push(Tab {
                root: Node::Leaf(id),
                focus: id,
            });
            self.active = 0;
        }
        Some(closed)
    }

    /// Moves pane `id` next to pane `target` (or swaps them), as a drag
    /// and drop does. Both may be in different tabs.
    pub fn move_pane(&mut self, id: PaneId, target: PaneId, drop: Drop) -> bool {
        if id == target {
            return false;
        }
        match drop {
            Drop::Swap => {
                let (Some(a), Some(b)) = (self.tab_of(id), self.tab_of(target)) else {
                    return false;
                };
                let tabs = if a == b { vec![a] } else { vec![a, b] };
                for t in tabs {
                    if let Some(tab) = self.tabs.get_mut(t) {
                        tab.root.swap(id, target);
                        if tab.focus == id {
                            tab.focus = target;
                        } else if tab.focus == target {
                            tab.focus = id;
                        }
                    }
                }
                self.focus(id)
            }
            Drop::Side(dir) => {
                if self.tab_of(target).is_none() || self.tab_of(id).is_none() {
                    return false;
                }
                if self.detach(id) == Some(Closed::Window) {
                    return false;
                }
                let Some(t) = self.tab_of(target) else {
                    return false;
                };
                let Some(tab) = self.tabs.get_mut(t) else {
                    return false;
                };
                tab.root.split(target, id, dir);
                self.focus(id)
            }
        }
    }

    /// Moves pane `id` into a tab of its own, after its current tab.
    pub fn pane_to_tab(&mut self, id: PaneId) -> bool {
        let Some(i) = self.tab_of(id) else {
            return false;
        };
        if self.tabs.get(i).is_some_and(|t| t.root.panes().len() < 2) {
            return false;
        }
        self.detach(id);
        let at = (i + 1).min(self.tabs.len());
        self.tabs.insert(
            at,
            Tab {
                root: Node::Leaf(id),
                focus: id,
            },
        );
        self.active = at;
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1001, 601);

    #[test]
    fn splits_and_lays_out() {
        let mut root = Node::Leaf(1);
        assert!(root.split(1, 2, Dir::Right));
        assert!(root.split(2, 3, Dir::Down));
        assert!(!root.split(9, 4, Dir::Down));
        assert_eq!(root.panes(), [1, 2, 3]);
        let l = root.layout(AREA, 1);
        assert_eq!(l[0], (1, Rect::new(0, 0, 500, 601)));
        assert_eq!(l[1], (2, Rect::new(501, 0, 500, 300)));
        assert_eq!(l[2], (3, Rect::new(501, 301, 500, 300)));
        let d = root.dividers(AREA, 1);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].rect, Rect::new(500, 0, 1, 601));
        assert_eq!(d[1].path, [true]);
        // Splitting to the left puts the new pane first.
        let mut root = Node::Leaf(1);
        root.split(1, 2, Dir::Left);
        assert_eq!(root.panes(), [2, 1]);
    }

    #[test]
    fn removing_collapses_the_split() {
        let mut root = Node::Leaf(1);
        root.split(1, 2, Dir::Right);
        root.split(2, 3, Dir::Down);
        let root = root.remove(2).unwrap();
        assert_eq!(root.panes(), [1, 3]);
        assert!(matches!(
            root,
            Node::Split {
                axis: Axis::Row,
                ..
            }
        ));
        assert_eq!(Node::Leaf(1).remove(1), None);
    }

    #[test]
    fn neighbours_follow_geometry() {
        // 1 | 2
        //   | 3
        let mut root = Node::Leaf(1);
        root.split(1, 2, Dir::Right);
        root.split(2, 3, Dir::Down);
        let l = root.layout(AREA, 1);
        assert_eq!(neighbour(&l, 1, Dir::Right), Some(2));
        assert_eq!(neighbour(&l, 3, Dir::Left), Some(1));
        assert_eq!(neighbour(&l, 2, Dir::Down), Some(3));
        assert_eq!(neighbour(&l, 3, Dir::Up), Some(2));
        assert_eq!(neighbour(&l, 1, Dir::Left), None);
        assert_eq!(neighbour(&l, 2, Dir::Right), None);
    }

    #[test]
    fn resizes_the_right_edge() {
        let mut root = Node::Leaf(1);
        root.split(1, 2, Dir::Right);
        root.split(2, 3, Dir::Down);
        assert!(root.resize(1, Dir::Right, 0.1));
        assert!(root.resize(3, Dir::Up, 0.1));
        assert!(!root.resize(1, Dir::Left, 0.1));
        let Node::Split { ratio, second, .. } = &root else {
            panic!()
        };
        assert!((ratio - 0.6).abs() < 1e-6);
        let Node::Split { ratio, .. } = second.as_ref() else {
            panic!()
        };
        assert!((ratio - 0.4).abs() < 1e-6);
        root.set_ratio(&[], 5.0);
        let Node::Split { ratio, .. } = &root else {
            panic!()
        };
        assert!((ratio - 0.9).abs() < 1e-6);
    }

    #[test]
    fn drop_zones() {
        let r = Rect::new(0, 0, 100, 100);
        assert_eq!(drop_zone(r, 5, 50), Drop::Side(Dir::Left));
        assert_eq!(drop_zone(r, 95, 40), Drop::Side(Dir::Right));
        assert_eq!(drop_zone(r, 50, 90), Drop::Side(Dir::Down));
        assert_eq!(drop_zone(r, 50, 50), Drop::Swap);
    }

    #[test]
    fn tabs_open_close_and_move() {
        let (mut ws, first) = Workspace::new();
        let second = ws.new_tab();
        let third = ws.new_tab();
        assert_eq!(ws.active, 2);
        ws.cycle_tab(true);
        assert_eq!(ws.focused(), first);
        ws.move_tab(0, 2);
        assert_eq!(ws.active, 2);
        assert_eq!(ws.tabs[0].focus, second);
        assert!(ws.focus(second));
        assert_eq!(ws.close(second), Some(Closed::Tab));
        assert_eq!(ws.focused(), third);
        assert_eq!(ws.close(third), Some(Closed::Tab));
        assert_eq!(ws.close(first), Some(Closed::Window));
        assert_eq!(ws.tabs.len(), 1);
    }

    #[test]
    fn closing_a_split_pane_moves_focus() {
        let (mut ws, first) = Workspace::new();
        let right = ws.split(Dir::Right).unwrap();
        assert_eq!(ws.focused(), right);
        assert_eq!(ws.close(right), Some(Closed::Pane));
        assert_eq!(ws.focused(), first);
        assert_eq!(ws.tabs.len(), 1);
    }

    #[test]
    fn panes_move_between_tabs() {
        let (mut ws, a) = Workspace::new();
        let b = ws.split(Dir::Right).unwrap();
        let c = ws.new_tab();
        // Drag c next to a: its tab goes away.
        assert!(ws.move_pane(c, a, Drop::Side(Dir::Down)));
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.tabs[0].root.panes(), [a, c, b]);
        assert_eq!(ws.focused(), c);
        // Swap keeps the shape.
        assert!(ws.move_pane(a, b, Drop::Swap));
        assert_eq!(ws.tabs[0].root.panes(), [b, c, a]);
        assert!(!ws.move_pane(a, a, Drop::Swap));
        // And out into its own tab.
        assert!(ws.pane_to_tab(c));
        assert_eq!(ws.tabs.len(), 2);
        assert_eq!(ws.focused(), c);
        assert!(!ws.pane_to_tab(c), "already alone");
        assert_eq!(ws.all_panes().len(), 3);
    }
}
