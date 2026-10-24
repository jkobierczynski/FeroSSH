//! Font choice and the cell geometry derived from it.

/// The font a pane asks for. Each pane has its own.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// Family name; the renderer falls back to a monospace system font if
    /// it is not installed.
    pub family: String,
    /// Size in points.
    pub size_pt: f32,
}

impl FontSpec {
    pub const MIN_PT: f32 = 6.0;
    pub const MAX_PT: f32 = 72.0;
    pub const DEFAULT_PT: f32 = 11.0;

    pub fn new(family: &str, size_pt: f32) -> Self {
        Self {
            family: family.to_owned(),
            size_pt: size_pt.clamp(Self::MIN_PT, Self::MAX_PT),
        }
    }

    /// The same family, `steps` half-points larger (or smaller).
    #[must_use]
    pub fn zoomed(&self, steps: i32) -> Self {
        Self::new(&self.family, self.size_pt + steps as f32 * 0.5)
    }

    /// The pixel size of one em at `dpi`.
    pub fn em_px(&self, dpi: u32) -> f32 {
        self.size_pt * dpi as f32 / 72.0
    }
}

impl Default for FontSpec {
    fn default() -> Self {
        Self::new("Cascadia Mono", Self::DEFAULT_PT)
    }
}

/// The font's design metrics, in font design units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DesignMetrics {
    pub units_per_em: u16,
    pub ascent: u16,
    pub descent: u16,
    pub line_gap: i16,
    /// Advance width of `0`, which every monospace font has.
    pub advance: u32,
    /// Distance from the baseline to the top of the underline; positive up.
    pub underline_position: i16,
    pub underline_thickness: u16,
}

/// Cell geometry in whole device pixels, so the grid never blurs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// Distance from the top of a cell to the baseline.
    pub baseline: f32,
    /// Distance from the top of a cell to the top of the underline.
    pub underline_top: f32,
    pub underline_thickness: f32,
    /// The font size in pixels (DIPs at 96 DPI).
    pub em_px: f32,
}

impl CellMetrics {
    pub fn new(design: &DesignMetrics, em_px: f32) -> Self {
        let upem = f32::from(design.units_per_em.max(1));
        let scale = em_px / upem;
        let ascent = f32::from(design.ascent) * scale;
        let descent = f32::from(design.descent) * scale;
        let gap = f32::from(design.line_gap.max(0)) * scale;
        let width = (design.advance as f32 * scale).round().max(1.0);
        let height = (ascent + descent + gap).ceil().max(1.0);
        // Split any rounding slack and the line gap evenly above and below.
        let baseline = ((height - ascent - descent) / 2.0 + ascent).round();
        let thickness = (f32::from(design.underline_thickness) * scale)
            .round()
            .max(1.0);
        let underline_top = (baseline - f32::from(design.underline_position) * scale)
            .round()
            .min(height - thickness);
        Self {
            width: width as u32,
            height: height as u32,
            baseline,
            underline_top,
            underline_thickness: thickness,
            em_px,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cascadia Mono's real design metrics.
    const CASCADIA: DesignMetrics = DesignMetrics {
        units_per_em: 2048,
        ascent: 1900,
        descent: 480,
        line_gap: 0,
        advance: 1200,
        underline_position: -280,
        underline_thickness: 100,
    };

    #[test]
    fn cells_are_whole_pixels_and_scale_with_dpi() {
        let spec = FontSpec::new("Cascadia Mono", 12.0);
        let at96 = CellMetrics::new(&CASCADIA, spec.em_px(96));
        let at192 = CellMetrics::new(&CASCADIA, spec.em_px(192));
        assert_eq!((at96.width, at96.height), (9, 19));
        assert!(at192.width >= at96.width * 2 - 1 && at192.width <= at96.width * 2 + 1);
        assert!(at192.height >= at96.height * 2 - 1 && at192.height <= at96.height * 2 + 1);
    }

    #[test]
    fn baseline_and_underline_stay_inside_the_cell() {
        for pt in [6.0, 9.5, 11.0, 16.0, 72.0] {
            let m = CellMetrics::new(&CASCADIA, FontSpec::new("x", pt).em_px(144));
            assert!(m.baseline > 0.0 && m.baseline < m.height as f32);
            assert!(m.underline_top >= m.baseline);
            assert!(m.underline_top + m.underline_thickness <= m.height as f32);
        }
    }

    #[test]
    fn zoom_is_clamped() {
        let f = FontSpec::default();
        assert_eq!(f.zoomed(-1000).size_pt, FontSpec::MIN_PT);
        assert_eq!(f.zoomed(1000).size_pt, FontSpec::MAX_PT);
        assert_eq!(f.zoomed(2).size_pt, FontSpec::DEFAULT_PT + 1.0);
    }
}
