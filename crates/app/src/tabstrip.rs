//! The tab strip: one row of cells above the panes, shown only when the
//! window has two or more tabs.
//!
//! Each tab shows a state dot (green: logged in, amber: connecting, grey:
//! closed or local), its title, a forwarding badge and a close mark; a `+`
//! at the end opens a new tab. It is drawn into a [`Grid`] like a terminal row, so it needs
//! nothing from the renderer. On Windows its colours follow the system menu
//! bar, with the active tab in the window colour.

use ferossh_render::{Cell, Color, Flags, Grid, Rgb, Theme};
use unicode_width::UnicodeWidthChar;

/// The longest title a tab shows, in cells.
const MAX_TITLE: usize = 24;
/// Short titles are padded to this many cells while there is room, so
/// tabs are easy to hit.
const MIN_TITLE: usize = 10;

/// A tab's connection state, as its dot shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Connected,
    Connecting,
    /// Closed, local, or asking for a host.
    Idle,
}

/// What a tab's connection forwards, as its badge shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Forwarding {
    #[default]
    None,
    /// Local or dynamic forwards only: `⇄`, dimmed.
    Outbound,
    /// Remote forwards or X11, which let the server reach into this
    /// side: `⇄`, in amber.
    Inbound,
}

impl Forwarding {
    /// The badge after a title, for the window title too.
    pub fn badge(self) -> &'static str {
        match self {
            Forwarding::None => "",
            Forwarding::Outbound | Forwarding::Inbound => " ⇄",
        }
    }
}

/// What the strip shows for one tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLabel {
    pub title: String,
    pub health: Health,
    /// Output or a bell arrived while the tab was in the background.
    pub bell: bool,
    pub forwarding: Forwarding,
    /// The connection uses the opt-in legacy algorithms: `⚠`, in amber.
    pub legacy: bool,
    /// The connection is not encrypted (Telnet): `⚠ telnet`, in red.
    pub unencrypted: bool,
}

/// The legacy-algorithms badge, before the forwarding one.
pub const LEGACY_BADGE: &str = " ⚠";
/// The badge of an unencrypted (Telnet) connection, in its place.
pub const UNENCRYPTED_BADGE: &str = " ⚠ telnet";

const AMBER: Rgb = Rgb::new(0xe5, 0xb5, 0x3b);
const RED: Rgb = Rgb::new(0xe0, 0x5c, 0x5c);

/// What a click on the strip hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    Close(usize),
    NewTab,
}

/// The cells each tab covers, for clicks and drags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StripLayout {
    /// Per tab: first cell, close-mark cell, one past the last cell.
    tabs: Vec<(usize, usize, usize)>,
    new_tab: Option<usize>,
}

impl StripLayout {
    pub fn hit(&self, col: usize) -> Option<Hit> {
        for (i, &(start, close, end)) in self.tabs.iter().enumerate() {
            if col == close {
                return Some(Hit::Close(i));
            }
            if (start..end).contains(&col) {
                return Some(Hit::Tab(i));
            }
        }
        (self.new_tab == Some(col)).then_some(Hit::NewTab)
    }

    /// The cells tab `i` covers: first, one past the last.
    pub fn span(&self, i: usize) -> Option<(usize, usize)> {
        self.tabs.get(i).map(|&(start, _, end)| (start, end))
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// The tab a dragged tab would land on at `col`: the nearest one.
    pub fn slot(&self, col: usize) -> usize {
        self.tabs
            .iter()
            .position(|&(start, _, end)| col < end || col < start)
            .unwrap_or(self.tabs.len().saturating_sub(1))
    }
}

pub fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) * (1.0 - amount) + f32::from(y) * amount).round() as u8;
    Rgb::new(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// Relative luminance, as WCAG defines it.
fn luminance(c: Rgb) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// The WCAG contrast ratio of two colours, from 1 to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Secondary text contrast the side pane keeps on both its backgrounds,
/// where the theme's own text allows: WCAG AA for normal text.
pub const DIM_CONTRAST: f32 = 4.5;

/// The strip's (and side pane's) colours.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub strip: Rgb,
    pub active: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
    /// The line over the active tab.
    pub accent: Rgb,
}

impl Palette {
    /// From a pane theme.
    pub fn of(theme: &Theme) -> Self {
        let strip = mix(theme.background, theme.foreground, 0.08);
        let active = theme.background;
        // Grey, but no fainter than reads well on either background: from
        // a 40% fade toward the background, step back to the foreground.
        let mut fade = 0.40;
        let mut dim = mix(theme.foreground, theme.background, fade);
        while fade > 0.0
            && (contrast(dim, strip) < DIM_CONTRAST || contrast(dim, active) < DIM_CONTRAST)
        {
            fade = (fade - 0.02_f32).max(0.0);
            dim = mix(theme.foreground, theme.background, fade);
        }
        Self {
            strip,
            active,
            text: theme.foreground,
            dim,
            accent: theme.cursor,
        }
    }

    /// From the system's menu bar colours: tabs on `menu`, the active one
    /// in `window` with `text`; `gray` for the rest.
    pub fn menu_bar(menu: Rgb, window: Rgb, text: Rgb, gray: Rgb, accent: Rgb) -> Self {
        // A window colour equal to the menu's (some high contrast themes)
        // would hide the active tab.
        let active = if window == menu {
            mix(menu, text, 0.12)
        } else {
            window
        };
        Self {
            strip: menu,
            active,
            text,
            dim: mix(gray, text, 0.35),
            accent,
        }
    }
}

/// Cuts `text` to `max` cells, ending in `…` when cut.
pub fn fit(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    let total: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
    for c in text.chars().filter(|c| !c.is_control()) {
        let w = c.width().unwrap_or(0);
        if total > max && width + w + 1 > max {
            out.push('…');
            return out;
        }
        if width + w > max {
            break;
        }
        width += w;
        out.push(c);
    }
    out
}

/// A row being filled cell by cell.
struct Row {
    cells: Vec<Cell>,
}

impl Row {
    fn put(&mut self, text: &str, fg: Rgb, bg: Rgb, flags: Flags) {
        for c in text.chars() {
            let w = c.width().unwrap_or(0);
            if w == 0 {
                continue;
            }
            let mut f = flags;
            if w == 2 {
                f.insert(Flags::WIDE);
            }
            self.cells
                .push(Cell::new(c, Color::Rgb(fg), Color::Rgb(bg), f));
            if w == 2 {
                self.cells.push(Cell::new(
                    ' ',
                    Color::Rgb(fg),
                    Color::Rgb(bg),
                    Flags::WIDE_SPACER,
                ));
            }
        }
    }
}

/// Draws the strip into `grid`'s first row and returns where each tab is.
/// Tabs share the width; titles shrink when space runs out.
pub fn draw(grid: &mut Grid, tabs: &[TabLabel], active: usize, palette: &Palette) -> StripLayout {
    let width = grid.cols();
    // "  ● " + title + "  ×  " per tab, and " + " at the end.
    let fixed = 9;
    let room = width.saturating_sub(3) / tabs.len().max(1);
    let title_max = room.saturating_sub(fixed).clamp(1, MAX_TITLE);
    let mut row = Row { cells: Vec::new() };
    let mut layout = StripLayout::default();
    for (i, tab) in tabs.iter().enumerate() {
        let bg = if i == active {
            palette.active
        } else {
            palette.strip
        };
        let fg = if i == active {
            palette.text
        } else {
            palette.dim
        };
        let start = row.cells.len();
        let dot = match tab.health {
            Health::Connected => Rgb::new(0x4c, 0xc3, 0x6b),
            Health::Connecting => Rgb::new(0xe5, 0xb5, 0x3b),
            Health::Idle => palette.dim,
        };
        row.put("  ", fg, bg, Flags::empty());
        row.put(if tab.bell { "◆" } else { "●" }, dot, bg, Flags::empty());
        row.put(" ", fg, bg, Flags::empty());
        let flags = if i == active {
            Flags::BOLD
        } else {
            Flags::empty()
        };
        let badge = tab.forwarding.badge();
        let (legacy, warn) = if tab.unencrypted {
            (UNENCRYPTED_BADGE, RED)
        } else if tab.legacy {
            (LEGACY_BADGE, AMBER)
        } else {
            ("", AMBER)
        };
        let title = fit(
            &tab.title,
            title_max
                .saturating_sub(badge.chars().count() + legacy.chars().count())
                .max(1),
        );
        let mut shown: usize = title.chars().map(|c| c.width().unwrap_or(0)).sum();
        row.put(&title, fg, bg, flags);
        if !legacy.is_empty() {
            row.put(legacy, warn, bg, Flags::empty());
            shown += legacy.chars().count();
        }
        if !badge.is_empty() {
            let colour = match tab.forwarding {
                Forwarding::Inbound => Rgb::new(0xe5, 0xb5, 0x3b),
                _ => palette.dim,
            };
            row.put(badge, colour, bg, Flags::empty());
            shown += badge.chars().count();
        }
        let pad = MIN_TITLE.min(title_max).saturating_sub(shown);
        row.put(&" ".repeat(pad + 2), fg, bg, Flags::empty());
        let close = row.cells.len();
        row.put("×", palette.dim, bg, Flags::empty());
        row.put("  ", fg, bg, Flags::empty());
        layout.tabs.push((start, close, row.cells.len()));
    }
    row.put(" ", palette.dim, palette.strip, Flags::empty());
    layout.new_tab = Some(row.cells.len());
    row.put("+", palette.dim, palette.strip, Flags::empty());
    let pad = Cell::new(
        ' ',
        Color::Rgb(palette.dim),
        Color::Rgb(palette.strip),
        Flags::empty(),
    );
    row.cells.resize(width.max(row.cells.len()), pad);
    row.cells.truncate(width);
    if layout.new_tab.is_some_and(|c| c >= width) {
        layout.new_tab = None;
    }
    grid.set_row(0, row.cells);
    layout
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn label(title: &str) -> TabLabel {
        TabLabel {
            title: title.into(),
            health: Health::Connected,
            bell: false,
            forwarding: Forwarding::None,
            legacy: false,
            unencrypted: false,
        }
    }

    fn text(grid: &Grid) -> String {
        grid.row(0)
            .unwrap()
            .iter()
            .filter(|c| !c.flags.contains(Flags::WIDE_SPACER))
            .map(|c| c.ch)
            .collect()
    }

    #[test]
    fn dim_text_reads_on_every_theme() {
        for theme in Theme::builtin() {
            let p = Palette::of(theme);
            let floor = DIM_CONTRAST.min(contrast(p.text, p.strip));
            for bg in [p.strip, p.active] {
                let c = contrast(p.dim, bg);
                assert!(c >= floor - 0.01, "{}: {c:.2}", theme.name);
            }
        }
    }

    #[test]
    fn fits_titles() {
        assert_eq!(fit("short", 10), "short");
        assert_eq!(fit("a long title", 6), "a lon…");
        assert_eq!(fit("日本語の", 5), "日本…");
        assert_eq!(fit("x\x1b[31m", 9), "x[31m");
    }

    #[test]
    fn draws_and_hits() {
        let mut grid = Grid::new(60, 1);
        let p = Palette::of(&Theme::default());
        let l = draw(&mut grid, &[label("web"), label("db")], 1, &p);
        let t = text(&grid);
        // Short titles are padded, so tabs keep a useful width.
        assert!(
            t.starts_with("  ● web         ×    ● db          ×   + "),
            "{t:?}"
        );
        assert_eq!(l.span(0), Some((0, 19)));
        assert_eq!(l.hit(0), Some(Hit::Tab(0)));
        assert_eq!(l.hit(16), Some(Hit::Close(0)));
        assert_eq!(l.hit(19), Some(Hit::Tab(1)));
        assert_eq!(l.hit(35), Some(Hit::Close(1)));
        assert_eq!(l.hit(39), Some(Hit::NewTab));
        assert_eq!(l.hit(50), None);
        assert_eq!(l.slot(0), 0);
        assert_eq!(l.slot(25), 1);
        assert_eq!(l.slot(55), 1);
        // The active tab is bold on the pane's background.
        let cell = grid.cell(23, 0).unwrap();
        assert!(cell.flags.contains(Flags::BOLD));
        assert_eq!(cell.bg, Color::Rgb(Theme::default().background));
    }

    #[test]
    fn badges_show_forwarding() {
        let mut grid = Grid::new(80, 1);
        let p = Palette::of(&Theme::default());
        let mut fwd = label("db");
        fwd.forwarding = Forwarding::Inbound;
        let l = draw(&mut grid, &[fwd, label("web")], 0, &p);
        let t = text(&grid);
        assert!(t.starts_with("  ● db ⇄        ×    ● web"), "{t:?}");
        assert_eq!(l.hit(16), Some(Hit::Close(0)));
        let badge = grid.cell(7, 0).unwrap();
        assert_eq!(badge.ch, '⇄');
        assert_eq!(badge.fg, Color::Rgb(Rgb::new(0xe5, 0xb5, 0x3b)));
    }

    #[test]
    fn badge_shows_legacy_algorithms() {
        let mut grid = Grid::new(80, 1);
        let p = Palette::of(&Theme::default());
        let mut old = label("switch");
        old.legacy = true;
        old.forwarding = Forwarding::Outbound;
        draw(&mut grid, &[old], 0, &p);
        let t = text(&grid);
        assert!(t.starts_with("  ● switch ⚠ ⇄"), "{t:?}");
        let badge = grid.cell(11, 0).unwrap();
        assert_eq!(badge.ch, '⚠');
        assert_eq!(badge.fg, Color::Rgb(Rgb::new(0xe5, 0xb5, 0x3b)));
    }

    #[test]
    fn badge_shows_unencrypted_telnet() {
        let mut grid = Grid::new(80, 1);
        let p = Palette::of(&Theme::default());
        let mut plain = label("router");
        plain.unencrypted = true;
        draw(&mut grid, &[plain], 0, &p);
        let t = text(&grid);
        assert!(t.starts_with("  ● router ⚠ telnet"), "{t:?}");
        let badge = grid.cell(11, 0).unwrap();
        assert_eq!(badge.ch, '⚠');
        assert_eq!(badge.fg, Color::Rgb(RED));
    }

    #[test]
    fn many_tabs_shrink() {
        let mut grid = Grid::new(40, 1);
        let p = Palette::of(&Theme::default());
        let tabs: Vec<TabLabel> = (0..4).map(|i| label(&format!("server-{i}-long"))).collect();
        let l = draw(&mut grid, &tabs, 0, &p);
        assert_eq!(l.hit(0), Some(Hit::Tab(0)));
        assert!(text(&grid).contains('…'));
        assert!(l.tabs.iter().all(|&(_, _, end)| end <= 40));
    }

    #[test]
    fn menu_bar_palette_keeps_the_active_tab_visible() {
        let grey = Rgb::new(0xf0, 0xf0, 0xf0);
        let black = Rgb::new(0, 0, 0);
        let p = Palette::menu_bar(grey, Rgb::new(255, 255, 255), black, grey, black);
        assert_eq!(p.strip, grey);
        assert_eq!(p.active, Rgb::new(255, 255, 255));
        let p = Palette::menu_bar(grey, grey, black, grey, black);
        assert_ne!(p.active, p.strip);
    }
}
