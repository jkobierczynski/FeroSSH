//! Bounded decoders for pictures a host prints into the terminal: Sixel,
//! the kitty graphics protocol and iTerm2's inline images (PNG only).
//!
//! Everything a host sends is untrusted. Every decoder here is safe Rust
//! (`unsafe` is forbidden in this crate), checks sizes before it
//! allocates, and stops at [`Limits`]: a picture larger than allowed is
//! refused, never truncated into a huge allocation. Sixel is the one
//! exception: it has no size up front (its raster attributes need not be
//! true), so a Sixel picture that grows past the limits keeps the part
//! drawn within them, as terminals with a fixed Sixel canvas do. Nothing here reads a
//! file, opens a socket or answers the host with anything but a fixed
//! status; the kitty protocol's file and shared-memory transmissions
//! are refused outright.
//!
//! Pixels come out as premultiplied `0xAARRGGBB`, ready for Direct2D.

#![forbid(unsafe_code)]

pub mod base64;
pub mod iterm;
pub mod kitty;
pub mod png;
pub mod sixel;

/// A decoded picture: `width` x `height` premultiplied `0xAARRGGBB`
/// pixels, row by row.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Picture {
    /// Bytes the pixels take.
    pub fn bytes(&self) -> usize {
        self.pixels.len().saturating_mul(4)
    }

    /// The `w` x `h` part at (`x`, `y`), clipped to the picture; `None`
    /// when nothing is left.
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Option<Picture> {
        let w = w.min(self.width.saturating_sub(x));
        let h = h.min(self.height.saturating_sub(y));
        if w == 0 || h == 0 {
            return None;
        }
        let mut pixels = Vec::with_capacity(w as usize * h as usize);
        for row in y..y + h {
            let start = row as usize * self.width as usize + x as usize;
            pixels.extend_from_slice(self.pixels.get(start..start + w as usize)?);
        }
        Some(Picture {
            width: w,
            height: h,
            pixels,
        })
    }
}

/// How large a picture may get. The defaults keep one picture at 16 MB
/// of pixels and its encoded form at 24 MB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Widest and tallest picture, in pixels.
    pub max_side: u32,
    /// Most pixels in one picture.
    pub max_pixels: usize,
    /// Most bytes of encoded data (base64, Sixel or compressed) for one
    /// picture.
    pub max_encoded: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        max_side: 8192,
        max_pixels: 4096 * 1024,
        max_encoded: 24 << 20,
    };

    /// Whether a `w` x `h` picture is allowed.
    pub fn allows(&self, w: u32, h: u32) -> bool {
        w > 0
            && h > 0
            && w <= self.max_side
            && h <= self.max_side
            && (w as usize).saturating_mul(h as usize) <= self.max_pixels
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Why a picture was not decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Malformed data.
    Invalid,
    /// Larger than [`Limits`] allow.
    TooLarge,
    /// A format or option FeroSSH does not take (an iTerm2 JPEG, a kitty
    /// file transmission).
    Unsupported,
    /// A kitty command named an image that is not kept.
    NotFound,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::Invalid => "malformed picture",
            Error::TooLarge => "picture too large",
            Error::Unsupported => "picture format not supported",
            Error::NotFound => "no such picture",
        })
    }
}

impl std::error::Error for Error {}

/// Premultiplies straight `r g b a` into `0xAARRGGBB`.
pub fn premultiply(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let m = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u32;
    if a == 255 {
        0xFF00_0000 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
    } else {
        u32::from(a) << 24 | m(r) << 16 | m(g) << 8 | m(b)
    }
}

/// Raw pixels as kitty sends them (`f=24` RGB or `f=32` RGBA), `w` x `h`.
pub fn from_raw(
    data: &[u8],
    w: u32,
    h: u32,
    alpha: bool,
    limits: &Limits,
) -> Result<Picture, Error> {
    if !limits.allows(w, h) {
        return Err(if w == 0 || h == 0 {
            Error::Invalid
        } else {
            Error::TooLarge
        });
    }
    let bpp = if alpha { 4 } else { 3 };
    let n = w as usize * h as usize;
    // Exactly the pixels the size says: more or less is malformed.
    if Some(data.len()) != n.checked_mul(bpp) {
        return Err(Error::Invalid);
    }
    let pixels = data
        .chunks_exact(bpp)
        .map(|p| match *p {
            [r, g, b, a] => premultiply(r, g, b, a),
            [r, g, b] => premultiply(r, g, b, 255),
            _ => 0,
        })
        .collect();
    Ok(Picture {
        width: w,
        height: h,
        pixels,
    })
}

/// zlib-inflates `data`, refusing more than `max` bytes out.
pub fn inflate(data: &[u8], max: usize) -> Result<Vec<u8>, Error> {
    use miniz_oxide::inflate::TINFLStatus;
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, max).map_err(|e| {
        if e.status == TINFLStatus::HasMoreOutput {
            Error::TooLarge
        } else {
            Error::Invalid
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies() {
        assert_eq!(premultiply(255, 0, 0, 255), 0xFFFF_0000);
        assert_eq!(premultiply(255, 255, 255, 0), 0);
        assert_eq!(premultiply(255, 128, 0, 128), 0x8080_4000);
    }

    #[test]
    fn raw_and_limits() {
        let p = from_raw(&[1, 2, 3, 4, 5, 6], 2, 1, false, &Limits::DEFAULT).ok();
        assert_eq!(p.map(|p| p.pixels), Some(vec![0xFF01_0203, 0xFF04_0506]));
        assert_eq!(
            from_raw(&[1, 2], 2, 1, false, &Limits::DEFAULT),
            Err(Error::Invalid)
        );
        assert_eq!(
            from_raw(&[1, 2, 3, 4], 1, 1, false, &Limits::DEFAULT),
            Err(Error::Invalid),
            "a byte too many"
        );
        assert_eq!(
            from_raw(&[], 9000, 1, false, &Limits::DEFAULT),
            Err(Error::TooLarge)
        );
    }

    #[test]
    fn crops() {
        let p = Picture {
            width: 3,
            height: 2,
            pixels: vec![1, 2, 3, 4, 5, 6],
        };
        assert_eq!(p.crop(1, 0, 5, 5).map(|c| c.pixels), Some(vec![2, 3, 5, 6]));
        assert_eq!(p.crop(3, 0, 1, 1), None);
    }

    #[test]
    fn inflate_is_bounded() {
        let data = miniz_oxide::deflate::compress_to_vec_zlib(&[0u8; 10_000], 6);
        assert_eq!(inflate(&data, 20_000).map(|v| v.len()), Ok(10_000));
        assert_eq!(inflate(&data, 100), Err(Error::TooLarge));
        assert_eq!(inflate(b"junk", 100), Err(Error::Invalid));
    }
}
