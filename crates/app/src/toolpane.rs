//! The tools pane: an icon rail on the window's outer edge, and the open
//! tool beside it, which the terminal slides over.
//!
//! The tool keeps the width it is laid out at; the terminal's near edge
//! decides how much of it shows. Dragging that edge covers the tool
//! partly, or completely so that only the rail is left, or uncovers it
//! again (and past its width, makes it wider). Platform-neutral, so the
//! geometry is tested on any host; the rail's window is in `rail.rs`.

use crate::layout::Rect;

/// Width of the icon rail, in DIPs.
pub const RAIL_DIP: i32 = 40;
/// With less than this much of the tool showing (DIPs), a drag covers it
/// completely.
pub const MIN_SHOWN_DIP: i32 = 48;
/// The narrowest a tool is laid out, in pixels.
pub const MIN_WIDTH: i32 = 80;

/// The rail's buttons, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailItem {
    Connections,
    Sessions,
    Files,
    Forwards,
    Snippets,
    /// At the bottom: the command palette.
    Palette,
    /// At the bottom, last: keep the tool open (pinned) or let the
    /// terminal cover it again after a peek.
    Pin,
}

impl RailItem {
    pub const ALL: [RailItem; 7] = [
        RailItem::Connections,
        RailItem::Sessions,
        RailItem::Files,
        RailItem::Forwards,
        RailItem::Snippets,
        RailItem::Palette,
        RailItem::Pin,
    ];

    /// The tooltip.
    pub fn name(self, pinned: bool) -> &'static str {
        match self {
            RailItem::Connections => "Connection manager (Ctrl+Shift+S)",
            RailItem::Sessions => "Session manager (Ctrl+Shift+L)",
            RailItem::Files => "File transfer (Ctrl+Shift+E)",
            RailItem::Forwards => "Port forwards (Ctrl+Shift+P)",
            RailItem::Snippets => "Scripts (Ctrl+Shift+Space)",
            RailItem::Palette => "Command palette (Ctrl+Shift+A)",
            RailItem::Pin if pinned => "Tools stay open (click: the terminal covers them again)",
            RailItem::Pin => "Tools cover again after use (click: keep them open)",
        }
    }

    /// The icon: a character of Segoe MDL2 Assets, the icon font every
    /// Windows 10 and 11 has (Segoe Fluent Icons keeps the same code
    /// points).
    pub fn glyph(self, pinned: bool) -> char {
        match self {
            RailItem::Connections => '\u{E839}',   // Ethernet
            RailItem::Sessions => '\u{E756}',      // CommandPrompt
            RailItem::Files => '\u{E8B7}',         // Folder
            RailItem::Forwards => '\u{E8AB}',      // Switch
            RailItem::Snippets => '\u{E943}',      // Code
            RailItem::Palette => '\u{E721}',       // Search
            RailItem::Pin if pinned => '\u{E840}', // Pinned
            RailItem::Pin => '\u{E718}',           // Pin
        }
    }

    fn at_bottom(self) -> bool {
        matches!(self, RailItem::Palette | RailItem::Pin)
    }
}

/// Where rail button `item` is, in the rail's own coordinates: square
/// buttons `rail` pixels wide, the tools from the top and the palette
/// and pin from the bottom.
pub fn item_rect(item: RailItem, rail: i32, height: i32) -> Rect {
    let pad = rail / 8;
    let size = rail - 2 * pad;
    let step = size + pad;
    let index = |list: &[RailItem]| list.iter().position(|i| *i == item).unwrap_or(0) as i32;
    if item.at_bottom() {
        // Last in the list is lowest.
        let from_bottom = 1 - index(&[RailItem::Palette, RailItem::Pin]);
        let y = height - pad - size - from_bottom * step;
        Rect::new(pad, y, size, size)
    } else {
        let tools = [
            RailItem::Connections,
            RailItem::Sessions,
            RailItem::Files,
            RailItem::Forwards,
            RailItem::Snippets,
        ];
        Rect::new(pad, pad + index(&tools) * step, size, size)
    }
}

/// The rail button at a point in the rail's own coordinates. Bottom
/// buttons that would overlap the top ones (a very short window) give
/// way to them.
pub fn item_at(x: i32, y: i32, rail: i32, height: i32) -> Option<RailItem> {
    let top_end = item_rect(RailItem::Snippets, rail, height).bottom();
    RailItem::ALL.into_iter().find(|&i| {
        let r = item_rect(i, rail, height);
        r.contains(x, y) && (!i.at_bottom() || r.y >= top_end)
    })
}

/// What the layout needs to know.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub client: (i32, i32),
    /// Rail width in pixels; 0 without a rail.
    pub rail: i32,
    /// The tool's width.
    pub width: i32,
    /// How much of it shows.
    pub shown: i32,
    /// A tool is open.
    pub open: bool,
    pub right: bool,
    /// The edge's width.
    pub gap: i32,
}

/// The parts of the client area the tools pane makes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    pub rail: Option<Rect>,
    /// The part of the open tool that shows. Its grid starts at its left.
    pub shown: Option<Rect>,
    /// The tool's whole width (what its grid is laid out for).
    pub width: i32,
    /// The terminal's near edge, which drags.
    pub edge: Option<Rect>,
    /// What is left for the tab strip and the terminal.
    pub main: Rect,
}

/// The tool's width, as `width` fits the client `w` beside a rail.
pub fn fit_width(width: i32, w: i32, rail: i32) -> i32 {
    width.clamp(MIN_WIDTH, ((w - rail) / 2).max(MIN_WIDTH))
}

/// Splits the client area.
pub fn split(i: &Input) -> Parts {
    let (w, h) = i.client;
    let rail = i.rail.clamp(0, w);
    let mut parts = Parts {
        main: Rect::new(0, 0, w, h),
        ..Parts::default()
    };
    if rail == 0 && !i.open {
        return parts;
    }
    let width = fit_width(i.width, w, rail);
    let shown = if i.open {
        i.shown.clamp(0, width).min(w - rail - i.gap).max(0)
    } else {
        0
    };
    // The edge sits next to the shown part, or next to the rail when the
    // tool is covered; the terminal gets the rest.
    let used = (rail + shown + i.gap).min(w);
    let rest = (w - used).max(0);
    parts.width = width;
    if i.right {
        if rail > 0 {
            parts.rail = Some(Rect::new(w - rail, 0, rail, h));
        }
        if i.open {
            parts.shown = Some(Rect::new(w - rail - shown, 0, shown, h));
        }
        parts.edge = Some(Rect::new(rest, 0, used - rail - shown, h));
        parts.main = Rect::new(0, 0, rest, h);
    } else {
        if rail > 0 {
            parts.rail = Some(Rect::new(0, 0, rail, h));
        }
        if i.open {
            parts.shown = Some(Rect::new(rail, 0, shown, h));
        }
        parts.edge = Some(Rect::new(rail + shown, 0, used - rail - shown, h));
        parts.main = Rect::new(used, 0, rest, h);
    }
    parts
}

/// Where a drag of the terminal's edge to client x leaves the tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dragged {
    pub open: bool,
    pub width: i32,
    pub shown: i32,
}

/// The tool after its edge is dragged to `x`: less than `min_shown`
/// uncovered covers it completely; past its width, it grows.
pub fn drag(x: i32, i: &Input, min_shown: i32) -> Dragged {
    let (w, _) = i.client;
    let rail = i.rail.clamp(0, w);
    let offset = if i.right { w - rail - x } else { x - rail };
    let width = fit_width(i.width, w, rail);
    if offset < min_shown {
        return Dragged {
            open: false,
            width,
            shown: 0,
        };
    }
    if offset > width {
        let width = fit_width(offset, w, rail);
        return Dragged {
            open: true,
            width,
            shown: width,
        };
    }
    Dragged {
        open: true,
        width,
        shown: offset,
    }
}

/// How much of a tool shows when it opens: what showed last, or all of
/// it when that was too little to use.
pub fn reopen(shown: i32, width: i32, min_shown: i32) -> i32 {
    if shown < min_shown || shown > width {
        width
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(open: bool, right: bool, shown: i32) -> Input {
        Input {
            client: (1000, 600),
            rail: 40,
            width: 300,
            shown,
            open,
            right,
            gap: 2,
        }
    }

    #[test]
    fn open_tool_on_the_left_is_partly_covered() {
        let p = split(&input(true, false, 120));
        assert_eq!(p.rail, Some(Rect::new(0, 0, 40, 600)));
        assert_eq!(p.shown, Some(Rect::new(40, 0, 120, 600)));
        assert_eq!(p.width, 300);
        assert_eq!(p.edge, Some(Rect::new(160, 0, 2, 600)));
        assert_eq!(p.main, Rect::new(162, 0, 838, 600));
    }

    #[test]
    fn open_tool_on_the_right_mirrors_it() {
        let p = split(&input(true, true, 120));
        assert_eq!(p.rail, Some(Rect::new(960, 0, 40, 600)));
        assert_eq!(p.shown, Some(Rect::new(840, 0, 120, 600)));
        assert_eq!(p.edge, Some(Rect::new(838, 0, 2, 600)));
        assert_eq!(p.main, Rect::new(0, 0, 838, 600));
    }

    #[test]
    fn covered_tool_leaves_the_rail_and_a_draggable_edge() {
        let p = split(&input(false, false, 120));
        assert_eq!(p.shown, None);
        assert_eq!(p.edge, Some(Rect::new(40, 0, 2, 600)));
        assert_eq!(p.main, Rect::new(42, 0, 958, 600));
    }

    #[test]
    fn no_rail_and_no_tool_is_all_terminal() {
        let mut i = input(false, false, 0);
        i.rail = 0;
        let p = split(&i);
        assert_eq!(
            p,
            Parts {
                main: Rect::new(0, 0, 1000, 600),
                ..Parts::default()
            }
        );
    }

    #[test]
    fn no_rail_but_an_open_tool_works_as_before() {
        let mut i = input(true, false, 300);
        i.rail = 0;
        let p = split(&i);
        assert_eq!(p.rail, None);
        assert_eq!(p.shown, Some(Rect::new(0, 0, 300, 600)));
        assert_eq!(p.main, Rect::new(302, 0, 698, 600));
    }

    #[test]
    fn shown_never_exceeds_the_width_and_width_half_the_window() {
        let mut i = input(true, false, 5000);
        i.width = 5000;
        let p = split(&i);
        assert_eq!(p.width, 480);
        assert_eq!(p.shown.map(|r| r.w), Some(480));
    }

    #[test]
    fn tiny_window_does_not_go_negative() {
        let mut i = input(true, false, 300);
        i.client = (30, 10);
        let p = split(&i);
        assert!(p.main.w >= 0);
        assert!(p.shown.is_some_and(|r| r.w >= 0));
    }

    #[test]
    fn drag_covers_uncovers_and_widens() {
        let i = input(true, false, 300);
        assert_eq!(
            drag(100, &i, 48),
            Dragged {
                open: true,
                width: 300,
                shown: 60
            }
        );
        // Less than the minimum shows: covered.
        assert_eq!(
            drag(70, &i, 48),
            Dragged {
                open: false,
                width: 300,
                shown: 0
            }
        );
        // Past the tool's width: it grows.
        assert_eq!(
            drag(400, &i, 48),
            Dragged {
                open: true,
                width: 360,
                shown: 360
            }
        );
        // But not past half the window.
        assert_eq!(drag(990, &i, 48).width, 480);
    }

    #[test]
    fn drag_on_the_right_counts_from_the_rail() {
        let i = input(true, true, 300);
        // The rail starts at 960: x 860 leaves 100 showing.
        assert_eq!(
            drag(860, &i, 48),
            Dragged {
                open: true,
                width: 300,
                shown: 100
            }
        );
        assert!(!drag(950, &i, 48).open);
    }

    #[test]
    fn reopen_restores_a_usable_cover() {
        assert_eq!(reopen(120, 300, 48), 120);
        assert_eq!(reopen(0, 300, 48), 300);
        assert_eq!(reopen(400, 300, 48), 300);
    }

    #[test]
    fn rail_buttons_hit_test_top_and_bottom() {
        // 40 px rail: 5 px padding, 30 px buttons, 35 px steps.
        assert_eq!(item_at(20, 20, 40, 600), Some(RailItem::Connections));
        assert_eq!(item_at(20, 5 + 35 + 10, 40, 600), Some(RailItem::Sessions));
        assert_eq!(
            item_at(20, 5 + 4 * 35 + 10, 40, 600),
            Some(RailItem::Snippets)
        );
        assert_eq!(item_at(20, 600 - 5 - 10, 40, 600), Some(RailItem::Pin));
        assert_eq!(
            item_at(20, 600 - 5 - 35 - 10, 40, 600),
            Some(RailItem::Palette)
        );
        // Between the groups.
        assert_eq!(item_at(20, 300, 40, 600), None);
        // The padding.
        assert_eq!(item_at(1, 20, 40, 600), None);
    }

    #[test]
    fn short_rail_keeps_the_tool_buttons() {
        // Too short for both groups: the bottom ones give way.
        assert_eq!(item_at(20, 20, 40, 100), Some(RailItem::Connections));
        assert!(item_at(20, 95, 40, 100).is_none_or(|i| !i.at_bottom()));
    }

    #[test]
    fn every_item_has_a_name_and_an_icon() {
        for i in RailItem::ALL {
            for pinned in [false, true] {
                assert!(!i.name(pinned).is_empty());
                assert!(('\u{E000}'..='\u{F8FF}').contains(&i.glyph(pinned)));
            }
        }
    }
}
