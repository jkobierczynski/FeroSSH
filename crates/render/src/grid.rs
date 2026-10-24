//! A fixed-size grid of cells with per-row dirty tracking.

use crate::cell::Cell;

/// The cells a pane shows, plus which rows changed since the last draw.
#[derive(Clone, Debug)]
pub struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    dirty: Vec<bool>,
}

impl Grid {
    /// A blank grid; sizes are clamped to at least one cell.
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            cells: vec![Cell::BLANK; cols * rows],
            dirty: vec![true; rows],
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// One row, or `None` past the bottom.
    pub fn row(&self, y: usize) -> Option<&[Cell]> {
        let start = y.checked_mul(self.cols)?;
        self.cells.get(start..start.checked_add(self.cols)?)
    }

    pub fn cell(&self, x: usize, y: usize) -> Option<&Cell> {
        self.row(y)?.get(x)
    }

    /// A mutable cell; marks its row dirty.
    pub fn cell_mut(&mut self, x: usize, y: usize) -> Option<&mut Cell> {
        if x >= self.cols || y >= self.rows {
            return None;
        }
        self.mark_dirty(y);
        self.cells.get_mut(y * self.cols + x)
    }

    /// Blanks every cell of one row.
    pub fn clear_row(&mut self, y: usize) {
        if y >= self.rows {
            return;
        }
        let start = y * self.cols;
        if let Some(row) = self.cells.get_mut(start..start + self.cols) {
            row.fill(Cell::BLANK);
        }
        self.mark_dirty(y);
    }

    /// Blanks the whole grid.
    pub fn clear(&mut self) {
        self.cells.fill(Cell::BLANK);
        self.mark_all_dirty();
    }

    /// Moves every row up by one and blanks the bottom row.
    pub fn scroll_up(&mut self) {
        self.cells.rotate_left(self.cols);
        self.clear_row(self.rows - 1);
        self.mark_all_dirty();
    }

    /// Resizes, keeping the top-left content and blanking new cells.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut cells = vec![Cell::BLANK; cols * rows];
        let keep = cols.min(self.cols);
        for (y, dst) in cells.chunks_exact_mut(cols).take(rows).enumerate() {
            if let (Some(src), Some(dst)) = (self.row(y), dst.get_mut(..keep))
                && let Some(src) = src.get(..keep)
            {
                dst.copy_from_slice(src);
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = cells;
        self.dirty = vec![true; rows];
    }

    pub fn is_dirty(&self, y: usize) -> bool {
        self.dirty.get(y).copied().unwrap_or(false)
    }

    pub fn any_dirty(&self) -> bool {
        self.dirty.iter().any(|d| *d)
    }

    /// The first and last dirty row, if any.
    pub fn dirty_span(&self) -> Option<(usize, usize)> {
        let first = self.dirty.iter().position(|d| *d)?;
        let last = self.dirty.iter().rposition(|d| *d)?;
        Some((first, last))
    }

    pub fn mark_dirty(&mut self, y: usize) {
        if let Some(d) = self.dirty.get_mut(y) {
            *d = true;
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty.fill(true);
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.fill(false);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn put(g: &mut Grid, x: usize, y: usize, ch: char) {
        g.cell_mut(x, y).unwrap().ch = ch;
    }

    #[test]
    fn new_grid_is_blank_and_dirty() {
        let g = Grid::new(3, 2);
        assert_eq!(g.row(1).unwrap(), &[Cell::BLANK; 3]);
        assert!(g.is_dirty(0) && g.is_dirty(1));
        assert!(g.row(2).is_none());
    }

    #[test]
    fn writes_mark_only_their_row() {
        let mut g = Grid::new(3, 3);
        g.clear_dirty();
        put(&mut g, 1, 2, 'x');
        assert_eq!(g.dirty_span(), Some((2, 2)));
        assert!(!g.is_dirty(0));
        assert!(g.cell_mut(3, 0).is_none());
    }

    #[test]
    fn scroll_moves_rows_up() {
        let mut g = Grid::new(2, 2);
        put(&mut g, 0, 1, 'b');
        g.scroll_up();
        assert_eq!(g.cell(0, 0).unwrap().ch, 'b');
        assert_eq!(g.cell(0, 1).unwrap().ch, ' ');
    }

    #[test]
    fn resize_keeps_top_left() {
        let mut g = Grid::new(3, 3);
        put(&mut g, 0, 0, 'a');
        put(&mut g, 2, 2, 'z');
        g.resize(2, 4);
        assert_eq!((g.cols(), g.rows()), (2, 4));
        assert_eq!(g.cell(0, 0).unwrap().ch, 'a');
        assert!(g.row(3).unwrap().iter().all(|c| *c == Cell::BLANK));
        g.resize(0, 0);
        assert_eq!((g.cols(), g.rows()), (1, 1));
    }
}
