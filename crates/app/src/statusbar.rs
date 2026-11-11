//! The server status bar: one row under the panes showing the load of the
//! server the focused pane is logged in to (Settings > Server status
//! bar). The worker samples it ([`ferossh_ipc::Message::StatsOpen`]) and
//! sends only numbers; this draws them, quietly, in the tab strip's
//! colours:
//!
//! ```text
//!  web01   CPU 12%   Memory 3.1 / 7.8 GB   Disk 41% of 40 GB   ↓ 1.2 MB/s  ↑ 8.0 KB/s
//! ```
//!
//! Labels are dim, figures in the text colour, and a figure at
//! [`HIGH`] or more is amber. What does not fit is left off from the end.

use ferossh_ipc::ServerStats;
use ferossh_render::{Cell, Color, Flags, Grid, Rgb};
use unicode_width::UnicodeWidthChar;

use crate::tabstrip::{DIM_CONTRAST, Palette, contrast, fit, mix};

/// A share (of CPU, memory or disk), in tenths of a percent, from which
/// it shows in amber.
pub const HIGH: u64 = 900;
/// Seconds between samples.
pub const INTERVAL: u16 = 2;
/// The longest host name shown, in cells.
const MAX_HOST: usize = 24;

const AMBER: Rgb = Rgb::new(0xe5, 0xb5, 0x3b);

/// What the bar shows for the focused pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    /// The pane is not logged in to an SSH server (a local shell, Telnet,
    /// a closed session).
    NoServer,
    /// Logged in; the first sample has not come yet.
    Waiting {
        host: String,
    },
    Stats {
        host: String,
        stats: ServerStats,
    },
    /// The server cannot run the sampling command (no `sh`: a Windows
    /// server, a restricted shell) or stopped it.
    Unavailable {
        host: String,
    },
}

/// One piece of the bar: a dim label and its figure.
struct Part {
    label: &'static str,
    value: String,
    high: bool,
}

/// Amber, moved toward the text colour until it reads on the bar (a
/// light bar makes plain amber faint).
fn high_colour(palette: &Palette) -> Rgb {
    let floor = DIM_CONTRAST.min(contrast(palette.text, palette.strip));
    let mut colour = AMBER;
    let mut amount = 0.0_f32;
    while amount < 1.0 && contrast(colour, palette.strip) < floor {
        amount = (amount + 0.1).min(1.0);
        colour = mix(AMBER, palette.text, amount);
    }
    colour
}

/// What the bar says, label by label.
fn parts(shown: &Shown) -> (Option<&str>, Vec<Part>) {
    let note = |text: &str| Part {
        label: "",
        value: text.to_owned(),
        high: false,
    };
    match shown {
        Shown::NoServer => (None, vec![note("No server statistics for this pane")]),
        Shown::Waiting { host } => (Some(host), vec![note("…")]),
        Shown::Unavailable { host } => (
            Some(host),
            vec![note("This server does not report its load")],
        ),
        Shown::Stats { host, stats } => {
            let mut parts = Vec::new();
            if let Some(cpu) = stats.cpu {
                parts.push(Part {
                    label: "CPU",
                    value: percent(u64::from(cpu), 1000),
                    high: u64::from(cpu) >= HIGH,
                });
            }
            if let Some((used, all)) = stats.mem {
                let (u, a) = pair_size(used, all);
                parts.push(Part {
                    label: "Memory",
                    value: format!("{u} / {a}"),
                    high: share(used, all) >= HIGH,
                });
            }
            if let Some((used, size)) = stats.disk {
                parts.push(Part {
                    label: "Disk",
                    value: format!("{} of {}", percent(used, size), size_kib(size)),
                    high: share(used, size) >= HIGH,
                });
            }
            if let Some((rx, tx)) = stats.net {
                parts.push(Part {
                    label: "↓",
                    value: rate(rx),
                    high: false,
                });
                parts.push(Part {
                    label: "↑",
                    value: rate(tx),
                    high: false,
                });
            }
            if parts.is_empty() {
                parts.push(note("…"));
            }
            (Some(host), parts)
        }
    }
}

/// `part` of `whole` in tenths of a percent; 0 of nothing.
fn share(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    (u128::from(part) * 1000 / u128::from(whole)).min(1000) as u64
}

/// `part` of `whole` as a rounded whole percent.
fn percent(part: u64, whole: u64) -> String {
    format!("{}%", (share(part, whole) + 5) / 10)
}

/// Units of 1024, from KiB.
const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];

/// The unit `kib` reads best in, and its size in KiB.
fn unit_of(kib: u64) -> (&'static str, u64) {
    let mut unit = "KB";
    let mut scale = 1u64;
    for next in UNITS.iter().skip(1) {
        if kib < scale.saturating_mul(1024) {
            break;
        }
        unit = next;
        scale = scale.saturating_mul(1024);
    }
    (unit, scale)
}

/// `kib` in `scale`, rounded, with one decimal below 10.
fn in_unit(kib: u64, scale: u64) -> String {
    let scale = u128::from(scale.max(1));
    let tenths = (u128::from(kib) * 10 + scale / 2) / scale;
    if tenths < 100 {
        format!("{}.{}", tenths / 10, tenths % 10)
    } else {
        ((u128::from(kib) + scale / 2) / scale).to_string()
    }
}

/// A size in KiB: `512 KB`, `3.1 GB`, `40 GB`.
pub fn size_kib(kib: u64) -> String {
    let (unit, scale) = unit_of(kib);
    format!("{} {unit}", in_unit(kib, scale))
}

/// Used and all, in all's unit: `3.1` and `7.8 GB`.
fn pair_size(used: u64, all: u64) -> (String, String) {
    let (unit, scale) = unit_of(all);
    (
        in_unit(used, scale),
        format!("{} {unit}", in_unit(all, scale)),
    )
}

/// Bytes per second: `0 B/s`, `820 B/s`, `12 KB/s`, `1.2 MB/s`.
pub fn rate(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B/s");
    }
    // The unit from whole KiB, the figure from the bytes, so 1.5 KB/s
    // stays 1.5.
    let (unit, scale) = unit_of(bytes / 1024);
    let bytes_per_unit = scale.saturating_mul(1024);
    format!("{} {unit}/s", in_unit(bytes, bytes_per_unit))
}

/// A row being filled cell by cell, up to `width` cells.
struct Row {
    cells: Vec<Cell>,
    width: usize,
    bg: Rgb,
}

impl Row {
    fn room(&self) -> usize {
        self.width.saturating_sub(self.cells.len())
    }

    fn put(&mut self, text: &str, fg: Rgb, flags: Flags) {
        for c in text.chars() {
            let w = c.width().unwrap_or(0);
            if w == 0 || w > self.room() {
                continue;
            }
            let mut f = flags;
            if w == 2 {
                f.insert(Flags::WIDE);
            }
            self.cells
                .push(Cell::new(c, Color::Rgb(fg), Color::Rgb(self.bg), f));
            if w == 2 {
                self.cells.push(Cell::new(
                    ' ',
                    Color::Rgb(fg),
                    Color::Rgb(self.bg),
                    Flags::WIDE_SPACER,
                ));
            }
        }
    }
}

fn cells(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Draws the bar into `grid`'s first row.
pub fn draw(grid: &mut Grid, shown: &Shown, palette: &Palette) {
    let mut row = Row {
        cells: Vec::new(),
        width: grid.cols(),
        bg: palette.strip,
    };
    let (host, parts) = parts(shown);
    row.put(" ", palette.dim, Flags::empty());
    if let Some(host) = host {
        let host = fit(host, MAX_HOST);
        row.put(&host, palette.text, Flags::BOLD);
        row.put("   ", palette.dim, Flags::empty());
    }
    for (i, part) in parts.iter().enumerate() {
        let gap = if i == 0 {
            0
        } else if part.label == "↑" {
            2
        } else {
            3
        };
        let label = if part.label.is_empty() {
            0
        } else {
            cells(part.label) + 1
        };
        // A part shows whole or not at all.
        if gap + label + cells(&part.value) > row.room() {
            break;
        }
        row.put(&" ".repeat(gap), palette.dim, Flags::empty());
        if !part.label.is_empty() {
            row.put(part.label, palette.dim, Flags::empty());
            row.put(" ", palette.dim, Flags::empty());
        }
        let (fg, flags) = if part.label.is_empty() {
            (palette.dim, Flags::empty())
        } else if part.high {
            (high_colour(palette), Flags::BOLD)
        } else {
            (palette.text, Flags::empty())
        };
        row.put(&part.value, fg, flags);
    }
    let pad = Cell::new(
        ' ',
        Color::Rgb(palette.dim),
        Color::Rgb(palette.strip),
        Flags::empty(),
    );
    let width = grid.cols();
    row.cells.resize(width, pad);
    grid.set_row(0, row.cells);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn text(grid: &Grid) -> String {
        grid.row(0)
            .unwrap()
            .iter()
            .filter(|c| !c.flags.contains(Flags::WIDE_SPACER))
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn palette() -> Palette {
        Palette::from_colours(
            Rgb::new(0x20, 0x20, 0x20),
            Rgb::new(0xe0, 0xe0, 0xe0),
            Rgb::new(0x80, 0x80, 0xff),
        )
    }

    const GB: u64 = 1024 * 1024;

    fn busy() -> Shown {
        Shown::Stats {
            host: "web01".into(),
            stats: ServerStats {
                cpu: Some(123),
                mem: Some((3 * GB + GB / 10, 7 * GB + 8 * GB / 10)),
                disk: Some((41 * 40 * GB / 100, 40 * GB)),
                net: Some((1_250_000, 8 * 1024)),
            },
        }
    }

    #[test]
    fn shows_every_figure_when_there_is_room() {
        let mut grid = Grid::new(120, 1);
        draw(&mut grid, &busy(), &palette());
        assert_eq!(
            text(&grid),
            " web01   CPU 12%   Memory 3.1 / 7.8 GB   Disk 41% of 40 GB   ↓ 1.2 MB/s  ↑ 8.0 KB/s"
        );
    }

    #[test]
    fn leaves_off_what_does_not_fit_from_the_end() {
        let mut grid = Grid::new(40, 1);
        draw(&mut grid, &busy(), &palette());
        assert_eq!(text(&grid), " web01   CPU 12%   Memory 3.1 / 7.8 GB");
        let mut tiny = Grid::new(3, 1);
        draw(&mut tiny, &busy(), &palette());
        assert_eq!(text(&tiny), " we");
    }

    #[test]
    fn a_full_disk_is_amber() {
        let mut grid = Grid::new(120, 1);
        let shown = Shown::Stats {
            host: "db".into(),
            stats: ServerStats {
                disk: Some((95, 100)),
                ..ServerStats::default()
            },
        };
        draw(&mut grid, &shown, &palette());
        let row = grid.row(0).unwrap();
        let at = text(&grid).find("95%").unwrap();
        assert_eq!(row[at].fg, Color::Rgb(AMBER));
    }

    #[test]
    fn amber_darkens_on_a_light_bar() {
        let light = Palette::from_colours(
            Rgb::new(0xf0, 0xf0, 0xf0),
            Rgb::new(0x10, 0x10, 0x10),
            Rgb::new(0x00, 0x60, 0xc0),
        );
        let c = high_colour(&light);
        assert_ne!(c, AMBER);
        assert!(contrast(c, light.strip) >= DIM_CONTRAST);
        assert_eq!(high_colour(&palette()), AMBER);
    }

    #[test]
    fn says_why_there_is_nothing() {
        let mut grid = Grid::new(80, 1);
        draw(&mut grid, &Shown::NoServer, &palette());
        assert_eq!(text(&grid), " No server statistics for this pane");
        draw(&mut grid, &Shown::Waiting { host: "a".into() }, &palette());
        assert_eq!(text(&grid), " a   …");
        draw(
            &mut grid,
            &Shown::Unavailable { host: "w".into() },
            &palette(),
        );
        assert_eq!(text(&grid), " w   This server does not report its load");
    }

    #[test]
    fn sizes_and_rates_read_well() {
        assert_eq!(size_kib(512), "512 KB");
        assert_eq!(size_kib(1024), "1.0 MB");
        assert_eq!(size_kib(40 * GB), "40 GB");
        assert_eq!(size_kib(10 * GB + 9 * GB / 10), "11 GB");
        assert_eq!(size_kib(u64::MAX), "16777216 PB");
        assert_eq!(rate(0), "0 B/s");
        assert_eq!(rate(1023), "1023 B/s");
        assert_eq!(rate(12 * 1024), "12 KB/s");
        assert_eq!(rate(1536), "1.5 KB/s");
        assert_eq!(rate(u64::MAX), "16384 PB/s");
        assert_eq!(percent(0, 0), "0%");
        assert_eq!(percent(1, 3), "33%");
        assert_eq!(percent(999, 1000), "100%");
    }
}
