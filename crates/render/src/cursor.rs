//! Where and how a pane draws its cursor.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    /// Filled cell, for the focused pane.
    Block,
    /// Outline, for an unfocused pane.
    Hollow,
    /// A bar under the cell.
    Underline,
    /// A thin bar at the left edge of the cell.
    Beam,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub x: usize,
    pub y: usize,
    pub shape: CursorShape,
}
