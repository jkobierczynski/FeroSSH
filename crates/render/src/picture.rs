//! Pictures a host printed into the terminal (Sixel, kitty graphics,
//! iTerm2), drawn over the cells they cover.

use std::sync::Arc;

/// A decoded picture: premultiplied `0xAARRGGBB` pixels, row by row.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    /// Unique in the process while the picture lives; the renderer keeps
    /// it on the GPU under this key.
    pub key: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u32]>,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("key", &self.key)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Which part of a picture a cell shows: the picture is laid over
/// `cols` x `rows` cells and this cell is (`col`, `row`) of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PictureCell {
    pub key: u32,
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
}

impl PictureCell {
    /// The cell's part of a `width` x `height` picture, in picture
    /// pixels: left, top, right, bottom.
    pub fn source(&self, width: u32, height: u32) -> (f32, f32, f32, f32) {
        let fx = width as f32 / f32::from(self.cols.max(1));
        let fy = height as f32 / f32::from(self.rows.max(1));
        let (c, r) = (f32::from(self.col), f32::from(self.row));
        (c * fx, r * fy, (c + 1.0) * fx, (r + 1.0) * fy)
    }

    /// Whether `next` is the cell just right of this one in the same
    /// picture, so both can be drawn in one go.
    pub fn continues_with(&self, next: &PictureCell) -> bool {
        next.key == self.key
            && next.row == self.row
            && next.rows == self.rows
            && next.cols == self.cols
            && next.col == self.col.wrapping_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_rects() {
        let c = PictureCell {
            key: 1,
            row: 1,
            col: 2,
            rows: 2,
            cols: 4,
        };
        assert_eq!(c.source(40, 20), (20.0, 10.0, 30.0, 20.0));
        let next = PictureCell { col: 3, ..c };
        assert!(c.continues_with(&next));
        assert!(!next.continues_with(&c));
    }
}
