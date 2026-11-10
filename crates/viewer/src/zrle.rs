//! ZRLE rectangles (RFC 6143, 7.7.6): a zlib stream that lasts the whole
//! session, holding 64x64 tiles, each raw, solid, palette-packed, RLE or
//! palette RLE. Pixels are "CPIXELs": with the client's 32-bit, depth 24
//! format, the three low bytes, blue first.

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{MZFlush, MZStatus};

use crate::vnc::fill;

const TILE: u16 = 64;

/// The most bytes a rectangle's tiles may inflate to: a subencoding byte
/// per tile and the longest sensible encoding of every pixel (RLE of
/// single pixels: three bytes and a length), with room to spare. More is
/// a decompression bomb.
fn inflate_limit(width: u16, height: u16) -> usize {
    let tiles = usize::from(width).div_ceil(64) * usize::from(height).div_ceil(64);
    tiles * (1 + 3 * 16) + usize::from(width) * usize::from(height) * 5 + 1024
}

fn inflate_rect(z: &mut InflateState, mut input: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let r = inflate(z, input, &mut chunk, MZFlush::None);
        let used = r.bytes_consumed;
        let made = r.bytes_written;
        out.extend_from_slice(chunk.get(..made).unwrap_or_default());
        if out.len() > limit {
            return Err("A ZRLE rectangle inflates to more than it can hold.".into());
        }
        input = input.get(used..).unwrap_or_default();
        match r.status {
            Ok(MZStatus::Ok) | Err(miniz_oxide::MZError::Buf) => {}
            Ok(MZStatus::StreamEnd) => {
                return Err("The VNC server ended its ZRLE stream.".into());
            }
            Ok(MZStatus::NeedDict) | Err(_) => {
                return Err("The VNC server sent ZRLE data that does not inflate.".into());
            }
        }
        // Done once all input is in and the last call filled no buffer.
        if input.is_empty() && made < chunk.len() {
            return Ok(out);
        }
        if used == 0 && made == 0 {
            return Err("The VNC server sent ZRLE data that does not inflate.".into());
        }
    }
}

struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bytes<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = self.data.get(self.pos).copied().ok_or_else(short)?;
        self.pos += 1;
        Ok(b)
    }

    fn cpixel(&mut self) -> Result<u32, String> {
        let b = self.data.get(self.pos..self.pos + 3).ok_or_else(short)?;
        self.pos += 3;
        Ok(crate::rgb(b))
    }

    /// An RLE run length: bytes of 255 and a last one below, plus one.
    fn run(&mut self, max: usize) -> Result<usize, String> {
        let mut n = 1usize;
        loop {
            let b = self.u8()?;
            n += usize::from(b);
            if n > max {
                return Err("A ZRLE run is longer than its tile.".into());
            }
            if b != 255 {
                return Ok(n);
            }
        }
    }
}

fn short() -> String {
    "A ZRLE rectangle ended early.".into()
}

/// Decodes one ZRLE rectangle into `width` x `height` pixels.
pub(crate) fn decode(
    z: &mut InflateState,
    compressed: &[u8],
    width: u16,
    height: u16,
) -> Result<Vec<u32>, String> {
    let data = inflate_rect(z, compressed, inflate_limit(width, height))?;
    let mut b = Bytes {
        data: &data,
        pos: 0,
    };
    let mut out = vec![0u32; usize::from(width) * usize::from(height)];
    let mut tile = Vec::with_capacity(usize::from(TILE) * usize::from(TILE));
    let mut ty = 0u16;
    while ty < height {
        let th = (height - ty).min(TILE);
        let mut tx = 0u16;
        while tx < width {
            let tw = (width - tx).min(TILE);
            let area = usize::from(tw) * usize::from(th);
            tile.clear();
            tile.resize(area, 0);
            decode_tile(&mut b, &mut tile, tw, th)?;
            for (row, line) in tile.chunks_exact(usize::from(tw)).enumerate() {
                let at = (usize::from(ty) + row) * usize::from(width) + usize::from(tx);
                if let Some(dst) = out.get_mut(at..at + usize::from(tw)) {
                    dst.copy_from_slice(line);
                }
            }
            tx += tw;
        }
        ty += th;
    }
    Ok(out)
}

fn decode_tile(b: &mut Bytes<'_>, tile: &mut [u32], tw: u16, th: u16) -> Result<(), String> {
    let area = tile.len();
    let sub = b.u8()?;
    match sub {
        0 => {
            for p in tile.iter_mut() {
                *p = b.cpixel()?;
            }
        }
        1 => {
            let c = b.cpixel()?;
            fill(tile, tw, th, (0, 0, tw, th), c);
        }
        2..=16 => {
            let palette: Vec<u32> = (0..sub).map(|_| b.cpixel()).collect::<Result<_, _>>()?;
            let bits = match sub {
                2 => 1,
                3 | 4 => 2,
                _ => 4,
            };
            let per_byte = 8 / bits;
            let mask = (1u8 << bits) - 1;
            for row in tile.chunks_exact_mut(usize::from(tw)) {
                let mut byte = 0u8;
                for (i, p) in row.iter_mut().enumerate() {
                    if i % per_byte == 0 {
                        byte = b.u8()?;
                    }
                    let shift = 8 - bits * (i % per_byte + 1);
                    let index = usize::from((byte >> shift) & mask);
                    *p = *palette
                        .get(index)
                        .ok_or("A ZRLE tile points past its palette.")?;
                }
            }
        }
        128 => {
            let mut at = 0;
            while at < area {
                let c = b.cpixel()?;
                let n = b.run(area - at)?;
                if let Some(run) = tile.get_mut(at..at + n) {
                    run.fill(c);
                }
                at += n;
            }
        }
        130..=255 => {
            let size = usize::from(sub - 128);
            let palette: Vec<u32> = (0..size).map(|_| b.cpixel()).collect::<Result<_, _>>()?;
            let mut at = 0;
            while at < area {
                let i = b.u8()?;
                let c = *palette
                    .get(usize::from(i & 127))
                    .ok_or("A ZRLE tile points past its palette.")?;
                let n = if i & 128 != 0 { b.run(area - at)? } else { 1 };
                if let Some(run) = tile.get_mut(at..at + n) {
                    run.fill(c);
                }
                at += n;
            }
        }
        s => {
            return Err(format!(
                "A ZRLE tile has subencoding {s}, which does not exist."
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use miniz_oxide::deflate::core::{
        CompressorOxide, TDEFLFlush, compress, create_comp_flags_from_zip_params,
    };

    /// A zlib stream the way VNC servers write it: one stream, flushed
    /// (not finished) after each rectangle.
    struct Deflater(Box<CompressorOxide>);

    impl Deflater {
        fn new() -> Self {
            let mut c = Box::<CompressorOxide>::default();
            c.set_format_and_level(miniz_oxide::DataFormat::Zlib, 6);
            let _ = create_comp_flags_from_zip_params;
            Self(c)
        }

        fn rect(&mut self, data: &[u8]) -> Vec<u8> {
            let mut out = vec![0u8; data.len() * 2 + 1024];
            let (_, _, written) = compress(&mut self.0, data, &mut out, TDEFLFlush::Sync);
            out.truncate(written);
            out
        }
    }

    fn state() -> Box<InflateState> {
        InflateState::new_boxed(miniz_oxide::DataFormat::Zlib)
    }

    #[test]
    fn every_tile_kind() {
        let mut d = Deflater::new();
        let mut z = state();
        // 66x2: one 64x2 tile and one 2x2 tile.
        let mut raw = Vec::new();
        // Tile 1: plain RLE, colour 0x0a0b0c for 64, then 0x01 for 64.
        raw.extend([128, 0x0c, 0x0b, 0x0a, 63, 1, 0, 0, 63]);
        // Tile 2: packed palette of 2, rows "01", "10".
        raw.extend([2, 5, 0, 0, 6, 0, 0, 0b0100_0000, 0b1000_0000]);
        let data = decode(&mut z, &d.rect(&raw), 66, 2).unwrap();
        assert_eq!(data[0], 0x0a0b0c);
        assert_eq!(data[63], 0x0a0b0c);
        assert_eq!(data[64], 5);
        assert_eq!(data[65], 6);
        assert_eq!(data[66], 1);
        assert_eq!(data[66 + 64..], [6, 5]);

        // The stream carries on into the next rectangle: palette RLE and
        // solid and raw tiles.
        let raw = [130, 1, 0, 0, 2, 0, 0, 0x81, 2, 0];
        assert_eq!(decode(&mut z, &d.rect(&raw), 2, 2).unwrap(), [2, 2, 2, 1]);
        let raw = [1, 9, 0, 0];
        assert_eq!(decode(&mut z, &d.rect(&raw), 3, 1).unwrap(), [9, 9, 9]);
        let raw = [0, 1, 0, 0, 2, 0, 0];
        assert_eq!(decode(&mut z, &d.rect(&raw), 2, 1).unwrap(), [1, 2]);
    }

    #[test]
    fn bad_tiles_are_refused() {
        for raw in [
            &[17u8][..],
            &[129],
            &[128, 1, 2, 3, 255, 255],
            &[2, 1, 0, 0],
            &[130, 1, 0, 0, 2, 0, 0, 5],
        ] {
            let mut d = Deflater::new();
            let mut z = state();
            assert!(decode(&mut z, &d.rect(raw), 4, 1).is_err(), "{raw:?}");
        }
        // Not zlib at all.
        assert!(decode(&mut state(), b"nonsense", 1, 1).is_err());
    }

    #[test]
    fn a_bomb_stops_at_the_limit() {
        let mut d = Deflater::new();
        let zeros = vec![0u8; 10_000_000];
        let packed = d.rect(&zeros);
        assert!(packed.len() < 100_000);
        let err = decode(&mut state(), &packed, 16, 16).unwrap_err();
        assert!(err.contains("more than it can hold"), "{err}");
    }
}
