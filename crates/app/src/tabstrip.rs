//! The tab strip: one row of cells above the panes, shown only when the
//! window has two or more tabs.
//!
//! Each tab shows a state dot (green: logged in, amber: connecting, grey:
//! closed or local), its title and a close mark; a `+` at the end opens a
//! new tab. It is drawn into a [`Grid`] like a terminal row, in the colours
//! of the active pane's theme, so it needs nothing from the renderer.

use ferossh_render::{Cell, Color, Flags, Grid, Rgb, Theme};
use unicode_width::UnicodeWidthChar;

/// The longest title a tab shows, in cells.
const MAX_TITLE: usize = 24;

/// A tab's connection state, as its dot shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Connected,
    Connecting,
    /// Closed, local, or asking for a host.
    Idle,
}

/// What the strip shows for one tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLabel {
    pub title: String,
    pub health: Health,
    /// Output or a bell arrived while the tab was in the background.
    pub bell: bool,
}

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

    /// The tab a dragged tab would land on at `col`: the nearest one.
    pub fn slot(&self, col: usize) -> usize {
        self.tabs
            .iter()
            .position(|&(start, _, end)| col < end || col < start)
            .unwrap_or(self.tabs.len().saturating_sub(1))
    }
}

fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) * (1.0 - amount) + f32::from(y) * amount).round() as u8;
    Rgb::new(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// The strip's colours, from a pane theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub strip: Rgb,
    pub active: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
}

impl Palette {
    pub fn of(theme: &Theme) -> Self {
        Self {
            strip: mix(theme.background, theme.foreground, 0.08),
            active: theme.background,
            text: theme.foreground,
            dim: mix(theme.foreground, theme.background, 0.45),
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
    // " ● " + title + " × " per tab, and " + " at the end.
    let fixed = 6;
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
        row.put(" ", fg, bg, Flags::empty());
        row.put(if tab.bell { "◆" } else { "●" }, dot, bg, Flags::empty());
        row.put(" ", fg, bg, Flags::empty());
        let flags = if i == active {
            Flags::BOLD
        } else {
            Flags::empty()
        };
        row.put(&fit(&tab.title, title_max), fg, bg, flags);
        row.put(" ", fg, bg, Flags::empty());
        let close = row.cells.len();
        row.put("×", palette.dim, bg, Flags::empty());
        row.put(" ", fg, bg, Flags::empty());
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
        assert!(t.starts_with(" ● web ×  ● db ×  + "), "{t:?}");
        assert_eq!(l.hit(0), Some(Hit::Tab(0)));
        assert_eq!(l.hit(7), Some(Hit::Close(0)));
        assert_eq!(l.hit(10), Some(Hit::Tab(1)));
        assert_eq!(l.hit(15), Some(Hit::Close(1)));
        assert_eq!(l.hit(18), Some(Hit::NewTab));
        assert_eq!(l.hit(40), None);
        assert_eq!(l.slot(0), 0);
        assert_eq!(l.slot(12), 1);
        assert_eq!(l.slot(50), 1);
        // The active tab is bold on the pane's background.
        let cell = grid.cell(12, 0).unwrap();
        assert!(cell.flags.contains(Flags::BOLD));
        assert_eq!(cell.bg, Color::Rgb(Theme::default().background));
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
}
