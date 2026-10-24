//! DirectWrite/Direct2D glyph atlas and grid drawing.
//!
//! The renderer draws any number of panes into one window. Each pane brings
//! its own grid, font and colour theme, so a tab or split can look different
//! from its neighbours. Glyphs are rasterised once per font, size and DPI
//! into a shared atlas and then blitted per cell; only dirty rows are redrawn.
//!
//! Everything except the Direct2D backend is platform-neutral and tested on
//! any host.

mod atlas;
mod cell;
mod cursor;
#[cfg(windows)]
mod d2d;
mod font;
mod grid;
mod theme;

pub use atlas::{AtlasPacker, Slot};
pub use cell::{Cell, Color, Flags, Rgb};
pub use cursor::{Cursor, CursorShape};
#[cfg(windows)]
pub use d2d::{Frame, PaneView, PixelRect, Renderer};
pub use font::{CellMetrics, DesignMetrics, FontSpec};
pub use grid::Grid;
pub use theme::Theme;
