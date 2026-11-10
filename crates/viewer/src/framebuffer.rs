//! The window's copy of the remote screen, built from what the viewer
//! process sends. Nothing it is told is trusted: a rectangle off the
//! screen, or pixels before any screen size, is refused, and the caller
//! ends the session.

use crate::pipe::FromViewer;
use crate::{MAX_PIXELS, MAX_SIDE};

/// A screen-space rectangle: left, top, right, bottom (exclusive).
pub type Dirty = (u16, u16, u16, u16);

/// The remote screen, `0x00RRGGBB` per pixel.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    pixels: Vec<u32>,
    /// What changed since the last [`take_dirty`](Self::take_dirty).
    dirty: Option<Dirty>,
}

/// What a viewer message did to the screen, if it was about the screen.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// It changed (or resized) the screen.
    Drawn,
    /// Not a screen message: the caller handles it.
    Other(FromViewer),
}

impl Framebuffer {
    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// The changed area since the last call.
    pub fn take_dirty(&mut self) -> Option<Dirty> {
        self.dirty.take()
    }

    fn mark(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let r = (x, y, x + w, y + h);
        self.dirty = Some(match self.dirty {
            None => r,
            Some((l, t, rr, b)) => (l.min(r.0), t.min(r.1), rr.max(r.2), b.max(r.3)),
        });
    }

    fn fits(&self, x: u16, y: u16, w: u16, h: u16) -> bool {
        u32::from(x) + u32::from(w) <= u32::from(self.width)
            && u32::from(y) + u32::from(h) <= u32::from(self.height)
    }

    /// Applies a message; `Err` when it is not possible on this screen.
    pub fn apply(&mut self, msg: FromViewer) -> Result<Applied, String> {
        match msg {
            FromViewer::Size { width, height } => {
                if width > MAX_SIDE
                    || height > MAX_SIDE
                    || usize::from(width) * usize::from(height) > MAX_PIXELS
                {
                    return Err("The remote screen is too large.".into());
                }
                self.width = width;
                self.height = height;
                self.pixels = vec![0; usize::from(width) * usize::from(height)];
                self.dirty = Some((0, 0, width, height));
            }
            FromViewer::Pixels {
                x,
                y,
                width,
                height,
                data,
            } => {
                if !self.fits(x, y, width, height)
                    || data.len() != usize::from(width) * usize::from(height)
                {
                    return Err("The viewer drew off the remote screen.".into());
                }
                let stride = usize::from(self.width);
                for (row, line) in data.chunks_exact(usize::from(width)).enumerate() {
                    let at = (usize::from(y) + row) * stride + usize::from(x);
                    if let Some(dst) = self.pixels.get_mut(at..at + usize::from(width)) {
                        dst.copy_from_slice(line);
                    }
                }
                self.mark(x, y, width, height);
            }
            FromViewer::Copy {
                src_x,
                src_y,
                x,
                y,
                width,
                height,
            } => {
                if !self.fits(x, y, width, height) || !self.fits(src_x, src_y, width, height) {
                    return Err("The viewer copied off the remote screen.".into());
                }
                let stride = usize::from(self.width);
                let w = usize::from(width);
                let rows: Vec<usize> = (0..usize::from(height)).collect();
                // Overlapping copies downwards go bottom row first.
                let order: Box<dyn Iterator<Item = &usize>> = if src_y < y {
                    Box::new(rows.iter().rev())
                } else {
                    Box::new(rows.iter())
                };
                for &row in order {
                    let from = (usize::from(src_y) + row) * stride + usize::from(src_x);
                    let to = (usize::from(y) + row) * stride + usize::from(x);
                    if from + w <= self.pixels.len() && to + w <= self.pixels.len() {
                        self.pixels.copy_within(from..from + w, to);
                    }
                }
                self.mark(x, y, width, height);
            }
            other => return Ok(Applied::Other(other)),
        }
        Ok(Applied::Drawn)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn fb(w: u16, h: u16) -> Framebuffer {
        let mut f = Framebuffer::default();
        f.apply(FromViewer::Size {
            width: w,
            height: h,
        })
        .unwrap();
        f.take_dirty();
        f
    }

    #[test]
    fn draws_copies_and_tracks_damage() {
        let mut f = fb(4, 3);
        f.apply(FromViewer::Pixels {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
            data: vec![1, 2, 3, 4],
        })
        .unwrap();
        assert_eq!(f.pixels()[..8], [0, 1, 2, 0, 0, 3, 4, 0]);
        // Overlapping copy one row down.
        f.apply(FromViewer::Copy {
            src_x: 1,
            src_y: 0,
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        })
        .unwrap();
        assert_eq!(f.pixels(), [0, 1, 2, 0, 0, 1, 2, 0, 0, 3, 4, 0]);
        assert_eq!(f.take_dirty(), Some((1, 0, 3, 3)));
        assert_eq!(f.take_dirty(), None);
        assert_eq!(
            f.apply(FromViewer::Bell).unwrap(),
            Applied::Other(FromViewer::Bell)
        );
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let mut f = fb(4, 3);
        let off = FromViewer::Pixels {
            x: 3,
            y: 0,
            width: 2,
            height: 1,
            data: vec![0, 0],
        };
        assert!(f.apply(off).is_err());
        let short = FromViewer::Pixels {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
            data: vec![0],
        };
        assert!(f.apply(short).is_err());
        let copy = FromViewer::Copy {
            src_x: 0,
            src_y: 2,
            x: 0,
            y: 0,
            width: 1,
            height: 2,
        };
        assert!(f.apply(copy).is_err());
        let mut empty = Framebuffer::default();
        assert!(
            empty
                .apply(FromViewer::Pixels {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                    data: vec![0]
                })
                .is_err(),
            "no pixels before a size"
        );
    }
}
