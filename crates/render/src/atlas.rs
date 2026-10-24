//! Space allocation inside a glyph atlas texture.
//!
//! Terminal glyphs are all one or two cells wide and one cell high, so a
//! simple shelf packer wastes almost nothing. When the texture is full the
//! renderer clears it and starts over; rows already on screen keep their
//! pixels, so only the current frame's glyphs are re-rasterised.

/// A rectangle inside the atlas, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A shelf packer with a one-pixel gutter between slots.
#[derive(Clone, Debug)]
pub struct AtlasPacker {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    shelf: u32,
}

const GUTTER: u32 = 1;

impl AtlasPacker {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            x: 0,
            y: 0,
            shelf: 0,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Reserves `w` x `h` pixels, or `None` when the atlas is full.
    pub fn alloc(&mut self, w: u32, h: u32) -> Option<Slot> {
        if w == 0 || h == 0 || w > self.width || h > self.height {
            return None;
        }
        if self.x + w > self.width {
            self.y += self.shelf + GUTTER;
            self.x = 0;
            self.shelf = 0;
        }
        if self.y + h > self.height {
            return None;
        }
        let slot = Slot {
            x: self.x,
            y: self.y,
            w,
            h,
        };
        self.x += w + GUTTER;
        self.shelf = self.shelf.max(h);
        Some(slot)
    }

    /// Forgets every slot.
    pub fn reset(&mut self) {
        self.x = 0;
        self.y = 0;
        self.shelf = 0;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn overlaps(a: &Slot, b: &Slot) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    #[test]
    fn slots_never_overlap_and_stay_inside() {
        let mut p = AtlasPacker::new(64, 64);
        let mut slots = Vec::new();
        while let Some(s) = p.alloc(if slots.len() % 3 == 0 { 14 } else { 7 }, 13) {
            assert!(s.x + s.w <= 64 && s.y + s.h <= 64);
            slots.push(s);
        }
        assert!(slots.len() > 20);
        for (i, a) in slots.iter().enumerate() {
            for b in slots.iter().skip(i + 1) {
                assert!(!overlaps(a, b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn full_atlas_refuses_until_reset() {
        let mut p = AtlasPacker::new(10, 10);
        assert!(p.alloc(10, 10).is_some());
        assert!(p.alloc(1, 1).is_none());
        p.reset();
        assert_eq!(
            p.alloc(4, 4).unwrap(),
            Slot {
                x: 0,
                y: 0,
                w: 4,
                h: 4
            }
        );
        assert!(p.alloc(11, 1).is_none());
        assert!(p.alloc(0, 1).is_none());
    }
}
