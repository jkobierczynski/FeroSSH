//! A small PNG decoder: every colour type and bit depth, palettes with
//! transparency, and interlacing. Ancillary chunks other than `tRNS` are
//! skipped, as are checksums (a corrupt picture only looks wrong).

use crate::{Error, Limits, Picture, premultiply};

const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Whether `data` starts like a PNG.
pub fn is_png(data: &[u8]) -> bool {
    data.starts_with(SIGNATURE)
}

#[derive(Clone, Copy)]
struct Header {
    width: u32,
    height: u32,
    depth: u8,
    colour: u8,
    interlaced: bool,
}

impl Header {
    /// Samples per pixel.
    fn channels(&self) -> usize {
        match self.colour {
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 1,
        }
    }

    /// Bits per pixel.
    fn bits(&self) -> usize {
        self.channels() * usize::from(self.depth)
    }

    /// Bytes in one filtered row of `w` pixels, without the filter byte.
    fn row_bytes(&self, w: u32) -> usize {
        (w as usize * self.bits()).div_ceil(8)
    }
}

/// Decodes a PNG within `limits`.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Picture, Error> {
    let mut rest = data.strip_prefix(SIGNATURE).ok_or(Error::Invalid)?;
    let mut header = None;
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut key: Option<[u16; 3]> = None;
    let mut compressed = Vec::new();
    while rest.len() >= 12 {
        let (len, kind) = rest.split_at(4);
        let len = u32::from_be_bytes(len.try_into().map_err(|_| Error::Invalid)?) as usize;
        let (kind, body) = kind.split_at(4);
        let chunk = body.get(..len).ok_or(Error::Invalid)?;
        rest = body.get(len + 4..).ok_or(Error::Invalid)?;
        match kind {
            b"IHDR" => {
                let &[a, b, c, d, e, f, g, h, depth, colour, 0, 0, interlace] = chunk else {
                    return Err(Error::Invalid);
                };
                let width = u32::from_be_bytes([a, b, c, d]);
                let height = u32::from_be_bytes([e, f, g, h]);
                let ok = matches!(
                    (colour, depth),
                    (0, 1 | 2 | 4 | 8 | 16) | (3, 1 | 2 | 4 | 8) | (2 | 4 | 6, 8 | 16)
                );
                if !ok || interlace > 1 || width == 0 || height == 0 {
                    return Err(Error::Invalid);
                }
                if !limits.allows(width, height) {
                    return Err(Error::TooLarge);
                }
                header = Some(Header {
                    width,
                    height,
                    depth,
                    colour,
                    interlaced: interlace == 1,
                });
            }
            b"PLTE" => {
                palette = chunk
                    .chunks_exact(3)
                    .take(256)
                    .map(|c| match *c {
                        [r, g, b] => [r, g, b, 255],
                        _ => [0; 4],
                    })
                    .collect();
            }
            b"tRNS" => match header.map(|h| h.colour) {
                Some(3) => {
                    for (entry, &a) in palette.iter_mut().zip(chunk) {
                        entry[3] = a;
                    }
                }
                Some(0) => {
                    if let &[a, b] = chunk {
                        let v = u16::from_be_bytes([a, b]);
                        key = Some([v, v, v]);
                    }
                }
                Some(2) => {
                    if let &[a, b, c, d, e, f] = chunk {
                        key = Some([
                            u16::from_be_bytes([a, b]),
                            u16::from_be_bytes([c, d]),
                            u16::from_be_bytes([e, f]),
                        ]);
                    }
                }
                _ => {}
            },
            b"IDAT" => {
                if compressed.len() + chunk.len() > limits.max_encoded {
                    return Err(Error::TooLarge);
                }
                compressed.extend_from_slice(chunk);
            }
            b"IEND" => break,
            // An unknown chunk the picture depends on (upper-case first
            // letter): it cannot be shown right.
            [first, ..] if first.is_ascii_uppercase() => return Err(Error::Unsupported),
            _ => {}
        }
    }
    let h = header.ok_or(Error::Invalid)?;
    if h.colour == 3 && palette.is_empty() {
        return Err(Error::Invalid);
    }
    let passes = passes(&h);
    let raw_len = passes
        .iter()
        .filter(|p| p.2 > 0 && p.3 > 0)
        .map(|p| (h.row_bytes(p.2) + 1) * p.3 as usize)
        .sum::<usize>();
    let raw = crate::inflate(&compressed, raw_len)?;
    if raw.len() < raw_len {
        return Err(Error::Invalid);
    }
    let mut pixels = vec![0u32; h.width as usize * h.height as usize];
    let mut at = 0;
    for &(x0, y0, pw, ph, dx, dy) in &passes {
        if pw == 0 || ph == 0 {
            continue;
        }
        let stride = h.row_bytes(pw);
        let size = (stride + 1) * ph as usize;
        let block = raw.get(at..at + size).ok_or(Error::Invalid)?;
        at += size;
        let rows = unfilter(block, stride, h.bits().div_ceil(8))?;
        for (j, row) in rows
            .chunks_exact(stride.max(1))
            .enumerate()
            .take(ph as usize)
        {
            let y = y0 + j as u32 * dy;
            for i in 0..pw {
                let x = x0 + i * dx;
                let p = pixel(&h, row, i as usize, &palette, key);
                if let Some(slot) = pixels.get_mut(y as usize * h.width as usize + x as usize) {
                    *slot = p;
                }
            }
        }
    }
    Ok(Picture {
        width: h.width,
        height: h.height,
        pixels,
    })
}

/// The passes as (x0, y0, width, height, dx, dy): one for a plain
/// picture, Adam7's seven for an interlaced one.
fn passes(h: &Header) -> Vec<(u32, u32, u32, u32, u32, u32)> {
    if !h.interlaced {
        return vec![(0, 0, h.width, h.height, 1, 1)];
    }
    [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ]
    .iter()
    .map(|&(x0, y0, dx, dy)| {
        let w = (h.width + dx - 1 - x0.min(h.width)) / dx;
        let ht = (h.height + dy - 1 - y0.min(h.height)) / dy;
        let w = if x0 >= h.width { 0 } else { w };
        let ht = if y0 >= h.height { 0 } else { ht };
        (x0, y0, w, ht, dx, dy)
    })
    .collect()
}

/// Undoes the per-row filters of one pass; `block` holds each row with
/// its filter byte first. Returns the rows back to back.
fn unfilter(block: &[u8], stride: usize, bpp: usize) -> Result<Vec<u8>, Error> {
    let mut out = vec![0u8; block.len() / (stride + 1) * stride];
    let mut prev_start: Option<usize> = None;
    for (n, row) in block.chunks_exact(stride + 1).enumerate() {
        let (&filter, data) = row.split_first().ok_or(Error::Invalid)?;
        let start = n * stride;
        for i in 0..stride {
            let x = *data.get(i).ok_or(Error::Invalid)?;
            let a = if i >= bpp {
                out.get(start + i - bpp).copied().unwrap_or(0)
            } else {
                0
            };
            let b = prev_start
                .and_then(|p| out.get(p + i).copied())
                .unwrap_or(0);
            let c = match prev_start {
                Some(p) if i >= bpp => out.get(p + i - bpp).copied().unwrap_or(0),
                _ => 0,
            };
            let v = match filter {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => return Err(Error::Invalid),
            };
            if let Some(slot) = out.get_mut(start + i) {
                *slot = v;
            }
        }
        prev_start = Some(start);
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Pixel `i` of an unfiltered row.
fn pixel(h: &Header, row: &[u8], i: usize, palette: &[[u8; 4]], key: Option<[u16; 3]>) -> u32 {
    let depth = usize::from(h.depth);
    // Sample `s` of pixel `i`, at its own depth.
    let sample = |s: usize| -> u16 {
        let bit = (i * h.channels() + s) * depth;
        match depth {
            16 => {
                let at = bit / 8;
                u16::from_be_bytes([
                    row.get(at).copied().unwrap_or(0),
                    row.get(at + 1).copied().unwrap_or(0),
                ])
            }
            8 => u16::from(row.get(bit / 8).copied().unwrap_or(0)),
            _ => {
                let byte = row.get(bit / 8).copied().unwrap_or(0);
                let shift = 8 - depth - bit % 8;
                u16::from(byte >> shift) & ((1 << depth) - 1)
            }
        }
    };
    // To 8 bits.
    let eight = |v: u16| -> u8 {
        match depth {
            16 => (v >> 8) as u8,
            8 => v as u8,
            _ => (u32::from(v) * 255 / ((1 << depth) - 1)) as u8,
        }
    };
    match h.colour {
        3 => {
            let [r, g, b, a] = palette
                .get(usize::from(sample(0)))
                .copied()
                .unwrap_or([0, 0, 0, 255]);
            premultiply(r, g, b, a)
        }
        0 => {
            let v = sample(0);
            let a = if key.is_some_and(|k| k[0] == v) {
                0
            } else {
                255
            };
            let g = eight(v);
            premultiply(g, g, g, a)
        }
        4 => {
            let g = eight(sample(0));
            premultiply(g, g, g, eight(sample(1)))
        }
        2 => {
            let (r, g, b) = (sample(0), sample(1), sample(2));
            let a = if key == Some([r, g, b]) { 0 } else { 255 };
            premultiply(eight(r), eight(g), eight(b), a)
        }
        _ => premultiply(
            eight(sample(0)),
            eight(sample(1)),
            eight(sample(2)),
            eight(sample(3)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0; 4]);
    }

    /// A PNG of `rows` (each with its filter byte).
    pub(crate) fn png(
        w: u32,
        h: u32,
        depth: u8,
        colour: u8,
        interlace: u8,
        rows: &[u8],
        extra: &[(&[u8], &[u8])],
    ) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[depth, colour, 0, 0, interlace]);
        chunk(&mut out, b"IHDR", &ihdr);
        for (k, b) in extra {
            chunk(&mut out, k, b);
        }
        chunk(
            &mut out,
            b"IDAT",
            &miniz_oxide::deflate::compress_to_vec_zlib(rows, 6),
        );
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn rgba_with_filters() {
        // Two rows of two pixels: row 0 unfiltered, row 1 "up".
        let rows = [
            0, 255, 0, 0, 255, 0, 255, 0, 128, //
            2, 0, 0, 255, 0, 0, 0, 0, 0,
        ];
        let p = decode(&png(2, 2, 8, 6, 0, &rows, &[]), &Limits::DEFAULT).ok();
        let p = p.map(|p| p.pixels);
        assert_eq!(
            p,
            Some(vec![0xFFFF_0000, 0x8000_8000, 0xFFFF_00FF, 0x8000_8000])
        );
    }

    #[test]
    fn palette_and_low_depth() {
        let rows = [0, 0b0110_0000];
        let plte = [10, 20, 30, 40, 50, 60];
        let trns = [255, 0];
        let p = decode(
            &png(3, 1, 2, 3, 0, &rows, &[(b"PLTE", &plte), (b"tRNS", &trns)]),
            &Limits::DEFAULT,
        )
        .map(|p| p.pixels);
        // Index 1 (transparent), 2 (none: black), 0.
        assert_eq!(p, Ok(vec![0, 0xFF00_0000, 0xFF0A_141E]));
    }

    #[test]
    fn grey_sixteen_and_interlaced() {
        let rows = [0, 0xFF, 0xFF, 0, 1];
        let p = decode(&png(2, 1, 16, 0, 0, &rows, &[]), &Limits::DEFAULT).map(|p| p.pixels);
        assert_eq!(p, Ok(vec![0xFFFF_FFFF, 0xFF00_0000]));
        // 2x2 interlaced grey 8: pass 1 holds (0,0), pass 6 (1,0), pass 7 row 1.
        let rows = [0, 1, 0, 2, 0, 3, 4];
        let p = decode(&png(2, 2, 8, 0, 1, &rows, &[]), &Limits::DEFAULT).map(|p| p.pixels);
        assert_eq!(
            p,
            Ok(vec![0xFF01_0101, 0xFF02_0202, 0xFF03_0303, 0xFF04_0404])
        );
    }

    #[test]
    fn refuses_bad_and_large() {
        assert_eq!(decode(b"nope", &Limits::DEFAULT), Err(Error::Invalid));
        let big = png(100_000, 1, 8, 0, 0, &[0], &[]);
        assert_eq!(decode(&big, &Limits::DEFAULT), Err(Error::TooLarge));
        let odd = png(1, 1, 8, 0, 0, &[0, 7], &[(b"ZZZZ", b"")]);
        assert_eq!(decode(&odd, &Limits::DEFAULT), Err(Error::Unsupported));
        let fine = png(1, 1, 8, 0, 0, &[0, 7], &[(b"tEXt", b"a\0b")]);
        assert!(decode(&fine, &Limits::DEFAULT).is_ok());
        let short = png(4, 4, 8, 0, 0, &[0, 1], &[]);
        assert_eq!(decode(&short, &Limits::DEFAULT), Err(Error::Invalid));
    }
}
