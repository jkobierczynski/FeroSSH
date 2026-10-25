//! Pictures in a pane instead of cells: the remote screen of the built-in
//! desktop viewer. Only FeroSSH built with the `images` feature draws
//! them; [`fit`] is neutral so the window can map the mouse.

/// A picture for a pane, `0x00RRGGBB` per pixel, row by row.
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    /// Stays the same while the picture is the same one (the pane's id):
    /// the renderer keeps its copy on the GPU under this key.
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u32],
    /// What changed since the last frame, in picture pixels: left, top,
    /// right, bottom (exclusive).
    pub dirty: Option<(u32, u32, u32, u32)>,
}

/// Where a `width` x `height` picture goes in `area` (left, top, right,
/// bottom): as large as fits, keeping its shape, never enlarged, centred.
pub fn fit(area: (i32, i32, i32, i32), width: u32, height: u32) -> (i32, i32, i32, i32) {
    let (l, t, r, b) = area;
    let (aw, ah) = ((r - l).max(0) as f64, (b - t).max(0) as f64);
    if width == 0 || height == 0 {
        return (l, t, l, t);
    }
    let scale = (aw / f64::from(width)).min(ah / f64::from(height)).min(1.0);
    let w = (f64::from(width) * scale).round() as i32;
    let h = (f64::from(height) * scale).round() as i32;
    let x = l + ((r - l) - w) / 2;
    let y = t + ((b - t) - h) / 2;
    (x, y, x + w, y + h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_without_enlarging() {
        assert_eq!(fit((0, 0, 200, 100), 100, 50), (50, 25, 150, 75));
        assert_eq!(fit((0, 0, 100, 100), 200, 100), (0, 25, 100, 75));
        assert_eq!(fit((10, 10, 10, 10), 5, 5), (10, 10, 10, 10));
        assert_eq!(fit((0, 0, 10, 10), 0, 5), (0, 0, 0, 0));
    }
}
