//! Sixel: the body of `ESC P <params> q <body> ESC \`.
//!
//! Pixels the picture never sets stay transparent, so the pane's own
//! background shows through, whatever the host asked for with P2.

use crate::{Error, Limits, Picture, premultiply};

/// Colour registers kept; higher register numbers wrap.
const REGISTERS: usize = 1024;
/// The largest repeat count honoured; more runs off the picture anyway.
const MAX_REPEAT: u32 = 1 << 16;
/// Times each pixel of the largest picture may be drawn, on average.
const OVERDRAW: usize = 8;

/// The VT340's 16 default colours, as 0-100 percentages.
const VT340: [[u8; 3]; 16] = [
    [0, 0, 0],
    [20, 20, 80],
    [80, 13, 13],
    [20, 80, 20],
    [80, 20, 80],
    [20, 80, 80],
    [80, 80, 20],
    [53, 53, 53],
    [26, 26, 26],
    [33, 33, 60],
    [60, 26, 26],
    [33, 60, 33],
    [60, 33, 60],
    [33, 60, 60],
    [60, 60, 33],
    [80, 80, 80],
];

fn percent(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

/// HLS (hue 0-360 with blue at 0, lightness and saturation 0-100) to RGB.
fn hls(h: u32, l: u32, s: u32) -> (u8, u8, u8) {
    let (l, s) = (f64::from(l.min(100)) / 100.0, f64::from(s.min(100)) / 100.0);
    // DEC's hue 0 is blue; the usual formula has red there.
    let h = f64::from((h + 240) % 360) / 360.0;
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    (channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
}

/// The picture being drawn: grows as sixels land further right or down.
struct Canvas {
    width: u32,
    height: u32,
    /// Allocated size; `pixels` is `cap_w` x `cap_h`.
    cap_w: u32,
    cap_h: u32,
    pixels: Vec<u32>,
    limits: Limits,
    /// Pixel writes left: drawing over the same pixels again and again
    /// (`$` and repeats) is cut off at a few times the picture's pixels,
    /// so a short sequence cannot keep the window busy.
    budget: usize,
}

impl Canvas {
    /// Makes room for (`x`, `y`); false when that is past the limits.
    fn reach(&mut self, x: u32, y: u32) -> bool {
        if x >= self.limits.max_side || y >= self.limits.max_side {
            return false;
        }
        if x < self.cap_w && y < self.cap_h {
            return true;
        }
        let w = if x < self.cap_w {
            self.cap_w
        } else {
            (x + 1).max(self.cap_w * 2).max(64)
        };
        let h = if y < self.cap_h {
            self.cap_h
        } else {
            (y + 1).max(self.cap_h * 2).max(6)
        };
        let (w, h) = (w.min(self.limits.max_side), h.min(self.limits.max_side));
        // Within the pixel budget, grow only as far as needed.
        let (w, h) = if (w as usize) * (h as usize) <= self.limits.max_pixels {
            (w, h)
        } else {
            (self.cap_w.max(x + 1), self.cap_h.max(y + 1))
        };
        if (w as usize) * (h as usize) > self.limits.max_pixels {
            return false;
        }
        let mut pixels = vec![0u32; w as usize * h as usize];
        for row in 0..self.cap_h as usize {
            let from = row * self.cap_w as usize;
            let to = row * w as usize;
            if let (Some(src), Some(dst)) = (
                self.pixels.get(from..from + self.cap_w as usize),
                pixels.get_mut(to..to + self.cap_w as usize),
            ) {
                dst.copy_from_slice(src);
            }
        }
        self.pixels = pixels;
        self.cap_w = w;
        self.cap_h = h;
        true
    }

    fn set(&mut self, x: u32, y: u32, colour: u32) {
        if let Some(p) = self
            .pixels
            .get_mut(y as usize * self.cap_w as usize + x as usize)
        {
            *p = colour;
        }
        self.width = self.width.max(x + 1);
        self.height = self.height.max(y + 1);
    }

    fn finish(self) -> Result<Picture, Error> {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 {
            return Err(Error::Invalid);
        }
        let mut pixels = Vec::with_capacity(w as usize * h as usize);
        for row in 0..h as usize {
            let from = row * self.cap_w as usize;
            pixels.extend_from_slice(
                self.pixels
                    .get(from..from + w as usize)
                    .ok_or(Error::Invalid)?,
            );
        }
        Ok(Picture {
            width: w,
            height: h,
            pixels,
        })
    }
}

/// Reads a run of `;`-separated numbers starting at `body[*at]`.
fn numbers(body: &[u8], at: &mut usize) -> Vec<u32> {
    let mut out = vec![0u32];
    while let Some(&c) = body.get(*at) {
        match c {
            b'0'..=b'9' => {
                if let Some(last) = out.last_mut() {
                    *last = last.saturating_mul(10).saturating_add(u32::from(c - b'0'));
                }
            }
            b';' if out.len() < 8 => out.push(0),
            b';' => {}
            _ => break,
        }
        *at += 1;
    }
    out
}

/// Decodes a Sixel body (everything after the `q`).
pub fn decode(body: &[u8], limits: &Limits) -> Result<Picture, Error> {
    if body.len() > limits.max_encoded {
        return Err(Error::TooLarge);
    }
    let mut palette: Vec<u32> = (0..REGISTERS)
        .map(|i| {
            let [r, g, b] = VT340.get(i % 16).copied().unwrap_or([0, 0, 0]);
            premultiply(percent(r.into()), percent(g.into()), percent(b.into()), 255)
        })
        .collect();
    let mut canvas = Canvas {
        width: 0,
        height: 0,
        cap_w: 0,
        cap_h: 0,
        pixels: Vec::new(),
        limits: *limits,
        budget: limits.max_pixels.saturating_mul(OVERDRAW),
    };
    let mut colour = palette.first().copied().unwrap_or(0xFF00_0000);
    let (mut x, mut y) = (0u32, 0u32);
    let mut at = 0;
    let mut clipped = false;
    while let Some(&c) = body.get(at) {
        at += 1;
        match c {
            b'?'..=b'~' => {
                draw(&mut canvas, x, y, c - b'?', 1, colour, &mut clipped);
                x = x.saturating_add(1);
            }
            b'!' => {
                let n = numbers(body, &mut at).first().copied().unwrap_or(1);
                let n = n.clamp(1, MAX_REPEAT);
                if let Some(&c @ b'?'..=b'~') = body.get(at) {
                    at += 1;
                    draw(&mut canvas, x, y, c - b'?', n, colour, &mut clipped);
                    x = x.saturating_add(n);
                }
            }
            b'#' => {
                let p = numbers(body, &mut at);
                let reg = p.first().map_or(0, |&r| r as usize % REGISTERS);
                if let [_, kind, a, b, c] = p.as_slice() {
                    let rgb = match kind {
                        1 => Some(hls(*a, *b, *c)),
                        2 => Some((percent(*a), percent(*b), percent(*c))),
                        _ => None,
                    };
                    if let (Some((r, g, b)), Some(slot)) = (rgb, palette.get_mut(reg)) {
                        *slot = premultiply(r, g, b, 255);
                    }
                }
                colour = palette.get(reg).copied().unwrap_or(colour);
            }
            b'"' => {
                // Raster attributes: aspect ratio and a size we need not
                // trust; the picture is as large as what is drawn.
                let _ = numbers(body, &mut at);
            }
            b'$' => x = 0,
            b'-' => {
                x = 0;
                y = y.saturating_add(6);
            }
            _ => {}
        }
    }
    canvas.finish()
}

/// Draws one sixel `bits` (six pixels, top bit first) `n` times at `x`.
fn draw(canvas: &mut Canvas, x: u32, y: u32, bits: u8, n: u32, colour: u32, clipped: &mut bool) {
    if bits == 0 || *clipped {
        // A blank sixel still makes the picture that wide, as terminals
        // draw it.
        if bits == 0 && !*clipped && n > 0 && canvas.reach(x.saturating_add(n - 1), y) {
            canvas.width = canvas.width.max(x.saturating_add(n));
            canvas.height = canvas.height.max(y + 1);
        }
        return;
    }
    let last = x.saturating_add(n - 1);
    let bottom = y.saturating_add(5);
    let cost = (n as usize).saturating_mul(bits.count_ones() as usize);
    if canvas.budget < cost || !canvas.reach(last, bottom) {
        // Past the limits: the rest is dropped, the part drawn stays.
        *clipped = true;
        return;
    }
    canvas.budget -= cost;
    for bit in 0..6 {
        if bits & (1 << bit) != 0 {
            for px in x..=last {
                canvas.set(px, y + bit, colour);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_columns_and_bands() {
        // Register 1 red; a full column, then a repeat of three, then a
        // second band with the top pixel only.
        let p = decode(b"#1;2;100;0;0#1~!3~-@", &Limits::DEFAULT);
        let p = p.ok();
        assert_eq!(p.as_ref().map(|p| (p.width, p.height)), Some((4, 7)));
        let px = p.map(|p| p.pixels).unwrap_or_default();
        assert_eq!(px.first(), Some(&0xFFFF_0000));
        assert_eq!(px.get(3), Some(&0xFFFF_0000));
        assert_eq!(px.get(6 * 4), Some(&0xFFFF_0000));
        assert_eq!(px.get(6 * 4 + 1), Some(&0));
    }

    #[test]
    fn hls_colours() {
        assert_eq!(hls(120, 50, 100), (255, 0, 0));
        assert_eq!(hls(0, 50, 100), (0, 0, 255));
        assert_eq!(hls(0, 100, 0), (255, 255, 255));
    }

    #[test]
    fn limits_hold() {
        let small = Limits {
            max_side: 10,
            max_pixels: 100,
            max_encoded: 1000,
        };
        // A repeat far past the width draws nothing more.
        let p = decode(b"~!60000~", &small).ok();
        assert_eq!(p.map(|p| (p.width, p.height)), Some((1, 6)));
        assert_eq!(decode(b"", &small), Err(Error::Invalid));
        assert_eq!(decode(&[b'~'; 2000], &small), Err(Error::TooLarge));
        // Drawing the same pixels over and over stops.
        let mut over = b"#1".to_vec();
        for _ in 0..150 {
            over.extend_from_slice(b"!10~$");
        }
        let p = decode(&over, &small).ok();
        assert_eq!(p.map(|p| p.width), Some(10), "the part drawn stays");
        // Bands past the bottom stop.
        let p = decode(b"~-~-~-~", &small).ok();
        assert_eq!(p.map(|p| p.height), Some(6));
    }
}
