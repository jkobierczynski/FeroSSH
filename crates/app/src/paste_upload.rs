//! Paste and drop for programs that take files by path, such as coding
//! agents: an image on the clipboard, or files pasted or dropped on a
//! pane, go up to a folder in the host's home folder and their paths are
//! typed at the cursor. This module holds the parts that need no window:
//! the folder, the names, the typed text and turning a clipboard bitmap
//! into a PNG.

/// Where pasted and dropped files go, under the host's home folder.
pub const FOLDER: &[u8] = b".cache/ferossh/paste";

/// The largest clipboard image turned into a PNG, in pixels (a 16K by
/// 8K screen); anything bigger is refused rather than held in memory.
const MAX_PIXELS: u64 = 16_384 * 8_192;

/// The local name for a pasted image taken at local time `t` (year,
/// month, day, hour, minute, second): `image-20261118-171530.png`.
pub fn image_name(t: (u16, u8, u8, u8, u8, u8)) -> String {
    let (y, mo, d, h, mi, s) = t;
    format!("image-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}.png")
}

/// `path` as a POSIX shell word: as it is when it holds only characters
/// no shell treats specially, else in single quotes.
pub fn quoted(path: &[u8]) -> String {
    let text = String::from_utf8_lossy(path);
    let plain = !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ',' | ':' | '@')
        });
    if plain {
        return text.into_owned();
    }
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// What is typed for uploaded files: each path quoted, a space after
/// each, the way a terminal types dropped files.
pub fn typed(paths: &[Vec<u8>]) -> String {
    paths.iter().map(|p| quoted(p) + " ").collect()
}

/// Whether `bytes` start like a PNG file.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&PNG_SIGNATURE)
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn i32_at(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// One colour of a bit-field pixel: where it sits and how wide it is.
#[derive(Clone, Copy)]
struct Field {
    shift: u32,
    bits: u32,
}

impl Field {
    fn of(mask: u32) -> Option<Self> {
        if mask == 0 {
            return None;
        }
        let shift = mask.trailing_zeros();
        let bits = (mask >> shift).trailing_ones();
        Some(Self { shift, bits })
    }

    /// The field's value in `px`, scaled to 0..=255.
    fn get(self, px: u32) -> u8 {
        let max = (1u64 << self.bits) - 1;
        let v = u64::from(px >> self.shift) & max;
        u8::try_from(v * 255 / max).unwrap_or(u8::MAX)
    }
}

/// A clipboard device-independent bitmap (`CF_DIB` or `CF_DIBV5`: the
/// header, any masks and colour table, then the pixels) as a PNG file.
/// Takes 1, 4, 8, 24 and 32 bits per pixel, uncompressed or with bit
/// fields; anything else is refused with a reason.
pub fn png_from_dib(dib: &[u8]) -> Result<Vec<u8>, String> {
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    const BI_ALPHABITFIELDS: u32 = 6;
    let bad = || "The clipboard image is not a bitmap FeroSSH can read.".to_owned();
    let header = u32_at(dib, 0).ok_or_else(bad)?;
    if header < 40 {
        return Err(bad());
    }
    let width = i32_at(dib, 4).ok_or_else(bad)?;
    let height = i32_at(dib, 8).ok_or_else(bad)?;
    let bpp = u16_at(dib, 14).ok_or_else(bad)?;
    let compression = u32_at(dib, 16).ok_or_else(bad)?;
    let colours_used = u32_at(dib, 32).ok_or_else(bad)?;
    let top_down = height < 0;
    let w = width.unsigned_abs();
    let h = height.unsigned_abs();
    if width <= 0 || h == 0 {
        return Err(bad());
    }
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("The clipboard image is too large to paste.".into());
    }
    let header = header as usize;
    // The masks: in the header from version 4 on, else right after it.
    let (masks, mut data) = match compression {
        BI_RGB => (None, header),
        BI_BITFIELDS | BI_ALPHABITFIELDS if matches!(bpp, 16 | 32) => {
            let count = if compression == BI_ALPHABITFIELDS {
                4
            } else {
                3
            };
            let (at, after) = if header >= 52 {
                (40, header)
            } else {
                (header, header + count * 4)
            };
            let r = u32_at(dib, at).ok_or_else(bad)?;
            let g = u32_at(dib, at + 4).ok_or_else(bad)?;
            let b = u32_at(dib, at + 8).ok_or_else(bad)?;
            let a = if compression == BI_ALPHABITFIELDS || header >= 56 {
                u32_at(dib, at + 12).unwrap_or(0)
            } else {
                0
            };
            (Some((r, g, b, a)), after)
        }
        _ => return Err("The clipboard image is compressed in a way FeroSSH cannot read.".into()),
    };
    let palette_len = match bpp {
        1 | 4 | 8 if colours_used == 0 => 1usize << bpp,
        1 | 4 | 8 => (colours_used as usize).min(1 << bpp),
        _ => 0,
    };
    let palette = dib
        .get(data..data + palette_len * 4)
        .ok_or_else(bad)?
        .to_vec();
    data += palette_len * 4;
    if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) || (bpp == 16 && masks.is_none()) {
        return Err(format!(
            "FeroSSH cannot read {bpp}-bit clipboard images; copy it as a PNG or another depth."
        ));
    }
    let stride = (w as usize * usize::from(bpp)).div_ceil(32) * 4;
    let pixels = dib.get(data..).ok_or_else(bad)?;
    if pixels.len() < stride * h as usize {
        return Err(bad());
    }
    let fields = masks.map(|(r, g, b, a)| (Field::of(r), Field::of(g), Field::of(b), Field::of(a)));
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let row = if top_down { y } else { h as usize - 1 - y };
        let line = pixels
            .get(row * stride..(row + 1) * stride)
            .ok_or_else(bad)?;
        for x in 0..w as usize {
            let px = match bpp {
                1 | 4 | 8 => {
                    let bits = usize::from(bpp);
                    let at = x * bits;
                    let byte = *line.get(at / 8).ok_or_else(bad)?;
                    let shift = 8 - bits - at % 8;
                    let index = usize::from(byte >> shift) & ((1 << bits) - 1);
                    match palette.get(index * 4..index * 4 + 3) {
                        Some([b, g, r]) => [*r, *g, *b, 255],
                        _ => [0, 0, 0, 255],
                    }
                }
                24 => match line.get(x * 3..x * 3 + 3) {
                    Some([b, g, r]) => [*r, *g, *b, 255],
                    _ => return Err(bad()),
                },
                _ => {
                    let size = usize::from(bpp) / 8;
                    let mut raw = [0u8; 4];
                    raw.get_mut(..size)
                        .ok_or_else(bad)?
                        .copy_from_slice(line.get(x * size..x * size + size).ok_or_else(bad)?);
                    let v = u32::from_le_bytes(raw);
                    match fields {
                        Some((r, g, b, a)) => {
                            let get = |f: Option<Field>| f.map_or(0, |f| f.get(v));
                            [get(r), get(g), get(b), a.map_or(255, |a| a.get(v))]
                        }
                        // 32 bits without masks: BGRA, the alpha often unused.
                        None => {
                            let [b, g, r, a] = raw;
                            [r, g, b, a]
                        }
                    }
                }
            };
            rgba.extend_from_slice(&px);
        }
    }
    // Most programs leave the fourth byte of a plain 32-bit bitmap at 0:
    // with no pixel using it, the image is opaque.
    let alpha = rgba.chunks_exact(4).any(|p| p.get(3) != Some(&0));
    let alpha_used = alpha && rgba.chunks_exact(4).any(|p| p.get(3) != Some(&255));
    if !alpha {
        for p in rgba.chunks_exact_mut(4) {
            if let Some(a) = p.get_mut(3) {
                *a = 255;
            }
        }
    }
    Ok(encode_png(w, h, &rgba, alpha_used))
}

/// An 8-bit PNG of `rgba` (`w` by `h`, rows top first), keeping the
/// alpha channel only when `alpha`.
pub fn encode_png(w: u32, h: u32, rgba: &[u8], alpha: bool) -> Vec<u8> {
    let channels = if alpha { 4 } else { 3 };
    let mut raw = Vec::with_capacity((w as usize * channels + 1) * h as usize);
    for row in rgba.chunks_exact(w as usize * 4) {
        raw.push(0); // no filter
        for p in row.chunks_exact(4) {
            raw.extend_from_slice(p.get(..channels).unwrap_or_default());
        }
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    // 8 bits; colour type 6 (RGBA) or 2 (RGB); deflate; no filter; no interlace.
    ihdr.extend_from_slice(&[8, if alpha { 6 } else { 2 }, 0, 0, 0]);
    let mut out = PNG_SIGNATURE.to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(
        &mut out,
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6),
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(out.get(start..).unwrap_or_default());
    out.extend_from_slice(&crc.to_be_bytes());
}

/// The CRC-32 PNG chunks end with (the zlib/IEEE one).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_typed_paths() {
        assert_eq!(
            image_name((2026, 11, 18, 7, 5, 9)),
            "image-20261118-070509.png"
        );
        assert_eq!(
            quoted(b"/home/me/.cache/ferossh/paste/image-20261118-070509.png"),
            "/home/me/.cache/ferossh/paste/image-20261118-070509.png"
        );
        assert_eq!(quoted(b"/h/shot (2).png"), "'/h/shot (2).png'");
        assert_eq!(quoted(b"/h/it's $HOME"), r"'/h/it'\''s $HOME'");
        assert_eq!(quoted(b""), "''");
        assert_eq!(typed(&[b"/a".to_vec(), b"/b c".to_vec()]), "/a '/b c' ");
    }

    #[test]
    fn crc_matches_the_known_value() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    /// A DIB with a 40-byte header and `pixels` after `extra` bytes.
    fn dib(w: i32, h: i32, bpp: u16, compression: u32, extra: &[u8], pixels: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&w.to_le_bytes());
        d.extend_from_slice(&h.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&bpp.to_le_bytes());
        d.extend_from_slice(&compression.to_le_bytes());
        d.extend_from_slice(&[0; 20]);
        d.extend_from_slice(extra);
        d.extend_from_slice(pixels);
        d
    }

    /// The IHDR fields and the unfiltered rows of a PNG this module wrote.
    fn decode(png: &[u8]) -> (u32, u32, u8, Vec<u8>) {
        assert!(is_png(png));
        let mut at = 8;
        let (mut w, mut h, mut kind, mut idat) = (0, 0, 0, Vec::new());
        while let (Some(len), Some(tag)) = (png.get(at..at + 4), png.get(at + 4..at + 8)) {
            let len = u32::from_be_bytes(len.try_into().unwrap_or_default()) as usize;
            let body = png.get(at + 8..at + 8 + len).unwrap_or_default();
            let crc = png.get(at + 8 + len..at + 12 + len).unwrap_or_default();
            assert_eq!(
                crc,
                crc32(png.get(at + 4..at + 8 + len).unwrap_or_default()).to_be_bytes()
            );
            match tag {
                b"IHDR" => {
                    w = u32::from_be_bytes(
                        body.get(..4)
                            .unwrap_or_default()
                            .try_into()
                            .unwrap_or_default(),
                    );
                    h = u32::from_be_bytes(
                        body.get(4..8)
                            .unwrap_or_default()
                            .try_into()
                            .unwrap_or_default(),
                    );
                    kind = body.get(9).copied().unwrap_or_default();
                }
                b"IDAT" => idat.extend_from_slice(body),
                _ => {}
            }
            at += 12 + len;
        }
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).unwrap_or_default();
        (w, h, kind, raw)
    }

    #[test]
    fn a_bottom_up_24_bit_bitmap_becomes_an_rgb_png_top_row_first() {
        // 2x2, rows padded to 8 bytes; the bottom row comes first.
        let pixels = [
            0, 0, 255, 0, 255, 0, 0, 0, // bottom: red, green
            255, 0, 0, 255, 255, 255, 0, 0, // top: blue, white
        ];
        let png = png_from_dib(&dib(2, 2, 24, 0, &[], &pixels)).unwrap_or_default();
        let (w, h, kind, raw) = decode(&png);
        assert_eq!((w, h, kind), (2, 2, 2));
        assert_eq!(raw, [0, 0, 0, 255, 255, 255, 255, 0, 255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn a_32_bit_bitmap_keeps_alpha_only_when_used() {
        let opaque = [1, 2, 3, 0, 4, 5, 6, 0];
        let png = png_from_dib(&dib(2, -1, 32, 0, &[], &opaque)).unwrap_or_default();
        let (_, _, kind, raw) = decode(&png);
        assert_eq!(kind, 2);
        assert_eq!(raw, [0, 3, 2, 1, 6, 5, 4]);
        let see_through = [1, 2, 3, 128, 4, 5, 6, 255];
        let png = png_from_dib(&dib(2, -1, 32, 0, &[], &see_through)).unwrap_or_default();
        let (_, _, kind, raw) = decode(&png);
        assert_eq!(kind, 6);
        assert_eq!(raw, [0, 3, 2, 1, 128, 6, 5, 4, 255]);
    }

    #[test]
    fn bit_fields_and_palettes_are_read() {
        // 16-bit 5-6-5: pure red and pure blue.
        let masks: Vec<u8> = [0xf800u32, 0x07e0, 0x001f]
            .iter()
            .flat_map(|m| m.to_le_bytes())
            .collect();
        let pixels = [0x00, 0xf8, 0x1f, 0x00];
        let png = png_from_dib(&dib(2, 1, 16, 3, &masks, &pixels)).unwrap_or_default();
        let (_, _, _, raw) = decode(&png);
        assert_eq!(raw, [0, 255, 0, 0, 0, 0, 255]);
        // 1-bit with a black and white palette: 10 pattern, padded row.
        let palette = [0, 0, 0, 0, 255, 255, 255, 0];
        let mut d = dib(2, 1, 1, 0, &palette, &[0b1000_0000, 0, 0, 0]);
        if let Some(used) = d.get_mut(32..36) {
            used.copy_from_slice(&2u32.to_le_bytes());
        }
        let (_, _, _, raw) = decode(&png_from_dib(&d).unwrap_or_default());
        assert_eq!(raw, [0, 255, 255, 255, 0, 0, 0]);
    }

    #[test]
    fn broken_or_unsupported_bitmaps_are_refused() {
        assert!(png_from_dib(&[]).is_err());
        assert!(
            png_from_dib(&dib(2, 2, 24, 0, &[], &[0; 4])).is_err(),
            "too short"
        );
        assert!(png_from_dib(&dib(0, 2, 24, 0, &[], &[])).is_err());
        assert!(
            png_from_dib(&dib(1, 1, 24, 1, &[], &[0; 4])).is_err(),
            "RLE"
        );
        assert!(png_from_dib(&dib(1, 1, 16, 0, &[], &[0; 4])).is_err());
        assert!(png_from_dib(&dib(100_000, 100_000, 24, 0, &[], &[])).is_err());
    }
}
