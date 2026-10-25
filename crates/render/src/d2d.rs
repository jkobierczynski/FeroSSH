//! The Direct2D/DirectWrite backend.
//!
//! One `Renderer` draws into one window. Fonts (text formats and cell
//! metrics) are cached per family, size and DPI; each cached font gets its
//! own glyph atlas on the current render target. Glyphs are rasterised into
//! the atlas once, in grayscale, and drawn per cell as an opacity mask in the
//! cell's foreground colour, so one atlas serves every colour and theme.

// Direct2D and DirectWrite are COM APIs; every unsafe block below is a direct
// call on an interface this module created and owns.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, E_FAIL, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED,
    D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_OPACITY_MASK_CONTENT_TEXT_NATURAL,
    D2D1_PRESENT_OPTIONS_RETAIN_CONTENTS, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Bitmap, ID2D1BitmapRenderTarget,
    ID2D1Factory, ID2D1HwndRenderTarget, ID2D1RenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE,
    DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_GLYPH_METRICS,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM};
use windows::core::{BOOL, Error, HSTRING, Result, w};

use crate::atlas::{AtlasPacker, Slot};
use crate::cell::{Cell, Flags, Rgb};
use crate::cursor::{Cursor, CursorShape};
use crate::font::{CellMetrics, DesignMetrics, FontSpec};
use crate::grid::Grid;
use crate::theme::Theme;

/// Monospace families tried, in order, when a pane's family is missing.
/// Courier New ships with every Windows version.
const FALLBACK_FAMILIES: [&str; 3] = ["Cascadia Mono", "Consolas", "Courier New"];

/// Atlas texture side, in pixels; 4 MB at one byte per pixel.
const ATLAS_SIDE: u32 = 2048;

const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};
const WHITE: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// A rectangle in client-area pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// One pane to draw: its cells and how it wants them to look.
#[derive(Debug)]
pub struct PaneView<'a> {
    pub grid: &'a Grid,
    pub font: &'a FontSpec,
    pub theme: &'a Theme,
    /// The area the pane owns, padding included.
    pub rect: PixelRect,
    /// Top-left corner of cell (0, 0).
    pub origin: (i32, i32),
    pub cursor: Option<Cursor>,
    /// Redraw every row and the padding, not only the grid's dirty rows.
    pub full: bool,
}

/// Result of a draw call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    Presented,
    /// The GPU device was lost. The renderer has dropped its device
    /// resources; invalidate the window and draw again.
    Lost,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FontKey {
    family: String,
    size_milli_pt: u32,
    dpi: u32,
}

impl FontKey {
    fn new(spec: &FontSpec, dpi: u32) -> Self {
        Self {
            family: spec.family.clone(),
            size_milli_pt: (spec.size_pt * 1000.0).round() as u32,
            dpi,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    ch: char,
    /// Combining marks drawn with `ch`; empty (and unallocated) for most.
    marks: String,
    /// Index into `Font::formats`: bit 0 bold, bit 1 italic.
    style: u8,
    wide: bool,
}

impl GlyphKey {
    fn of(cell: &Cell, grid: &Grid) -> Self {
        let mut style = 0;
        if cell.flags.contains(Flags::BOLD) {
            style |= 1;
        }
        if cell.flags.contains(Flags::ITALIC) {
            style |= 2;
        }
        Self {
            ch: cell.ch,
            marks: grid.cluster(cell).to_owned(),
            style,
            wide: cell.flags.contains(Flags::WIDE),
        }
    }
}

/// Device-independent font resources.
struct Font {
    /// Regular, bold, italic, bold italic.
    formats: [IDWriteTextFormat; 4],
    metrics: CellMetrics,
}

/// A glyph atlas for one font on the current render target.
struct Atlas {
    rt: ID2D1BitmapRenderTarget,
    bitmap: ID2D1Bitmap,
    ink: ID2D1SolidColorBrush,
    packer: AtlasPacker,
    slots: HashMap<GlyphKey, Slot>,
}

/// Device-dependent resources, rebuilt when the device is lost.
struct Target {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    atlases: HashMap<FontKey, Atlas>,
}

/// Draws panes of terminal cells into one window.
pub struct Renderer {
    hwnd: HWND,
    size: (u32, u32),
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    fonts: HashMap<FontKey, Font>,
    target: Option<Target>,
}

impl fmt::Debug for Renderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Renderer")
            .field("size", &self.size)
            .field("fonts", &self.fonts.len())
            .field("has_target", &self.target.is_some())
            .finish_non_exhaustive()
    }
}

impl Renderer {
    /// A renderer for `hwnd`, whose client area is `width` x `height` pixels.
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        // SAFETY: plain factory constructors with valid constant arguments.
        let (factory, dwrite) = unsafe {
            (
                D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?,
            )
        };
        Ok(Self {
            hwnd,
            size: (width, height),
            factory,
            dwrite,
            fonts: HashMap::new(),
            target: None,
        })
    }

    /// Cell geometry for `font` at `dpi`, loading the font if needed.
    pub fn metrics(&mut self, font: &FontSpec, dpi: u32) -> Result<CellMetrics> {
        let key = FontKey::new(font, dpi);
        if let Some(f) = self.fonts.get(&key) {
            return Ok(f.metrics);
        }
        let loaded = self.load_font(font, dpi)?;
        let metrics = loaded.metrics;
        self.fonts.insert(key, loaded);
        Ok(metrics)
    }

    /// Drops every cached font and atlas, e.g. after a DPI change.
    pub fn clear_font_cache(&mut self) {
        self.fonts.clear();
        if let Some(t) = &mut self.target {
            t.atlases.clear();
        }
    }

    /// Follows a change of the window's client size.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        if let Some(t) = &self.target {
            let size = D2D_SIZE_U {
                width: width.max(1),
                height: height.max(1),
            };
            // SAFETY: valid pointer to a local size.
            if unsafe { t.rt.Resize(&size) }.is_err() {
                self.target = None;
            }
        }
    }

    /// Draws the window's panes, then `fills` (split dividers, the focus
    /// frame) over them. Pass every pane: if the render target had to be
    /// recreated, all of them are redrawn in full.
    pub fn draw(
        &mut self,
        panes: &[PaneView<'_>],
        fills: &[(PixelRect, Rgb)],
        dpi: u32,
    ) -> Result<Frame> {
        match self.draw_inner(panes, fills, dpi) {
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                self.target = None;
                Ok(Frame::Lost)
            }
            other => other,
        }
    }

    fn draw_inner(
        &mut self,
        panes: &[PaneView<'_>],
        fills: &[(PixelRect, Rgb)],
        dpi: u32,
    ) -> Result<Frame> {
        for p in panes {
            self.metrics(p.font, dpi)?;
        }
        let fresh = self.target.is_none();
        if fresh {
            self.target = Some(self.create_target()?);
        }
        let Some(target) = self.target.as_mut() else {
            return Err(Error::from(E_FAIL));
        };

        // Rasterise every glyph this frame needs, per atlas, before drawing:
        // an atlas that fills up is cleared, which must not happen mid-frame.
        let mut needed: HashMap<FontKey, Vec<GlyphKey>> = HashMap::new();
        for p in panes {
            let keys = needed.entry(FontKey::new(p.font, dpi)).or_default();
            for y in rows_to_draw(p, fresh) {
                if let Some(row) = p.grid.row(y) {
                    keys.extend(
                        row.iter()
                            .filter(|c| !c.is_empty_glyph())
                            .map(|c| GlyphKey::of(c, p.grid)),
                    );
                }
            }
        }
        for (key, glyphs) in needed {
            let Some(font) = self.fonts.get(&key) else {
                continue;
            };
            let atlas = match target.atlases.entry(key) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => e.insert(Atlas::new(&target.rt)?),
            };
            atlas.prepare(font, &glyphs)?;
        }

        let rt: &ID2D1RenderTarget = &target.rt;
        // SAFETY: drawing calls on our own target between Begin and EndDraw.
        unsafe {
            rt.BeginDraw();
            if fresh {
                let bg = panes
                    .first()
                    .map(|p| p.theme.background)
                    .unwrap_or_default();
                rt.Clear(Some(&color(bg)));
            }
        }
        for p in panes {
            let key = FontKey::new(p.font, dpi);
            if let (Some(font), Some(atlas)) = (self.fonts.get(&key), target.atlases.get(&key)) {
                draw_pane(rt, &target.brush, atlas, &font.metrics, p, fresh);
            }
        }
        for (r, c) in fills {
            let r = rect(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
            // SAFETY: drawing calls between Begin and EndDraw.
            unsafe {
                target.brush.SetColor(&color(*c));
                rt.FillRectangle(&r, &target.brush);
            }
        }
        // SAFETY: closes the BeginDraw above.
        unsafe { rt.EndDraw(None, None)? };
        Ok(Frame::Presented)
    }

    fn create_target(&self) -> Result<Target> {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            // 96 DPI makes one DIP one pixel; the renderer scales fonts by
            // the window's DPI itself so that cells stay whole pixels.
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U {
                width: self.size.0.max(1),
                height: self.size.1.max(1),
            },
            // Keep the back buffer between frames so that only dirty rows
            // need drawing.
            presentOptions: D2D1_PRESENT_OPTIONS_RETAIN_CONTENTS,
        };
        // SAFETY: valid pointers to locals; the window outlives the renderer.
        unsafe {
            let rt = self.factory.CreateHwndRenderTarget(&props, &hwnd_props)?;
            // Opacity masks require aliased mode; cells are pixel-aligned.
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            let brush = rt.CreateSolidColorBrush(&WHITE, None)?;
            Ok(Target {
                rt,
                brush,
                atlases: HashMap::new(),
            })
        }
    }

    fn load_font(&self, spec: &FontSpec, dpi: u32) -> Result<Font> {
        // SAFETY: DirectWrite queries with valid out-pointers to locals.
        unsafe {
            let mut collection: Option<IDWriteFontCollection> = None;
            self.dwrite
                .GetSystemFontCollection(&mut collection, false)?;
            let collection = collection.ok_or_else(|| Error::from(E_FAIL))?;

            let mut found = None;
            for name in std::iter::once(spec.family.as_str()).chain(FALLBACK_FAMILIES) {
                let name = HSTRING::from(name);
                let mut index = 0;
                let mut exists = BOOL(0);
                collection.FindFamilyName(&name, &mut index, &mut exists)?;
                if exists.as_bool() {
                    found = Some((name, index));
                    break;
                }
            }
            let (family_name, index) = found.ok_or_else(|| Error::from(E_FAIL))?;

            let face = collection
                .GetFontFamily(index)?
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                )?
                .CreateFontFace()?;
            let mut fm = DWRITE_FONT_METRICS::default();
            face.GetMetrics(&mut fm);
            let zero = u32::from('0');
            let mut glyph = 0u16;
            face.GetGlyphIndices(&zero, 1, &mut glyph)?;
            let mut gm = DWRITE_GLYPH_METRICS::default();
            face.GetDesignGlyphMetrics(&glyph, 1, &mut gm, false)?;

            let design = DesignMetrics {
                units_per_em: fm.designUnitsPerEm,
                ascent: fm.ascent,
                descent: fm.descent,
                line_gap: fm.lineGap,
                advance: gm.advanceWidth,
                underline_position: fm.underlinePosition,
                underline_thickness: fm.underlineThickness,
            };
            let metrics = CellMetrics::new(&design, spec.em_px(dpi));

            let format = |weight: DWRITE_FONT_WEIGHT, style: DWRITE_FONT_STYLE| {
                let f = self.dwrite.CreateTextFormat(
                    &family_name,
                    &collection,
                    weight,
                    style,
                    DWRITE_FONT_STRETCH_NORMAL,
                    metrics.em_px,
                    w!("en-us"),
                )?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                // Pin the baseline so fallback fonts (CJK, symbols) line up
                // with the primary font instead of using their own ascent.
                f.SetLineSpacing(
                    DWRITE_LINE_SPACING_METHOD_UNIFORM,
                    metrics.height as f32,
                    metrics.baseline,
                )?;
                Ok::<_, Error>(f)
            };
            Ok(Font {
                formats: [
                    format(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL)?,
                    format(DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_NORMAL)?,
                    format(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_ITALIC)?,
                    format(DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_ITALIC)?,
                ],
                metrics,
            })
        }
    }
}

impl Atlas {
    fn new(parent: &ID2D1HwndRenderTarget) -> Result<Self> {
        // SAFETY: resource creation on our own target with valid locals.
        unsafe {
            let side = ATLAS_SIDE.min(parent.GetMaximumBitmapSize());
            let dips = D2D_SIZE_F {
                width: side as f32,
                height: side as f32,
            };
            let pixels = D2D_SIZE_U {
                width: side,
                height: side,
            };
            let format = |format| D2D1_PIXEL_FORMAT {
                format,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            };
            // Alpha-only is a quarter of the memory; fall back to BGRA on
            // drivers that refuse it. Only the alpha channel is used.
            let rt = parent
                .CreateCompatibleRenderTarget(
                    Some(&dips),
                    Some(&pixels),
                    Some(&format(DXGI_FORMAT_A8_UNORM)),
                    D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
                )
                .or_else(|_| {
                    parent.CreateCompatibleRenderTarget(
                        Some(&dips),
                        Some(&pixels),
                        Some(&format(DXGI_FORMAT_B8G8R8A8_UNORM)),
                        D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
                    )
                })?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            let ink = rt.CreateSolidColorBrush(&WHITE, None)?;
            let bitmap = rt.GetBitmap()?;
            rt.BeginDraw();
            rt.Clear(Some(&TRANSPARENT));
            rt.EndDraw(None, None)?;
            Ok(Self {
                rt,
                bitmap,
                ink,
                packer: AtlasPacker::new(side, side),
                slots: HashMap::new(),
            })
        }
    }

    /// Makes sure every glyph in `keys` has a slot.
    fn prepare(&mut self, font: &Font, keys: &[GlyphKey]) -> Result<()> {
        if keys.iter().all(|k| self.slots.contains_key(k)) {
            return Ok(());
        }
        let (cw, ch) = (font.metrics.width, font.metrics.height);
        let mut evicted = false;
        let mut utf16: Vec<u16> = Vec::new();
        // SAFETY: drawing calls on our own atlas target between Begin and
        // EndDraw, with valid pointers to locals.
        unsafe {
            self.rt.BeginDraw();
            let mut i = 0;
            while let Some(key) = keys.get(i) {
                i += 1;
                if self.slots.contains_key(key) {
                    continue;
                }
                let width = if key.wide { cw * 2 } else { cw };
                let slot = match self.packer.alloc(width, ch) {
                    Some(slot) => slot,
                    None if !evicted => {
                        // Full: start over and re-rasterise this frame's glyphs.
                        evicted = true;
                        self.rt.Clear(Some(&TRANSPARENT));
                        self.packer.reset();
                        self.slots.clear();
                        i = 0;
                        continue;
                    }
                    // More distinct glyphs in one frame than fit; skip the rest.
                    None => continue,
                };
                let rect = slot_rect(&slot);
                self.rt
                    .PushAxisAlignedClip(&rect, D2D1_ANTIALIAS_MODE_ALIASED);
                self.rt.Clear(Some(&TRANSPARENT));
                if let Some(format) = font.formats.get(usize::from(key.style)) {
                    utf16.clear();
                    let mut unit = [0u16; 2];
                    utf16.extend_from_slice(key.ch.encode_utf16(&mut unit));
                    utf16.extend(key.marks.encode_utf16());
                    self.rt.DrawText(
                        &utf16,
                        format,
                        &rect,
                        &self.ink,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                self.rt.PopAxisAlignedClip();
                self.slots.insert(key.clone(), slot);
            }
            self.rt.EndDraw(None, None)
        }
    }
}

/// Rows of `pane` that this frame redraws.
fn rows_to_draw<'a>(pane: &'a PaneView<'_>, force: bool) -> impl Iterator<Item = usize> + 'a {
    let all = pane.full || force;
    (0..pane.grid.rows()).filter(move |y| all || pane.grid.is_dirty(*y))
}

fn draw_pane(
    rt: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    atlas: &Atlas,
    m: &CellMetrics,
    pane: &PaneView<'_>,
    force: bool,
) {
    let theme = pane.theme;
    let (cw, ch) = (m.width as f32, m.height as f32);
    let (ox, oy) = (pane.origin.0 as f32, pane.origin.1 as f32);
    let bounds = rect(
        pane.rect.left as f32,
        pane.rect.top as f32,
        pane.rect.right as f32,
        pane.rect.bottom as f32,
    );
    let fill = |r: &D2D_RECT_F, c: Rgb| {
        // SAFETY: drawing calls inside the caller's BeginDraw.
        unsafe {
            brush.SetColor(&color(c));
            rt.FillRectangle(r, brush);
        }
    };

    // SAFETY: drawing call inside the caller's BeginDraw; popped below.
    unsafe { rt.PushAxisAlignedClip(&bounds, D2D1_ANTIALIAS_MODE_ALIASED) };
    if pane.full || force {
        fill(&bounds, theme.background);
    }
    for y in rows_to_draw(pane, force) {
        let Some(row) = pane.grid.row(y) else {
            continue;
        };
        let top = oy + y as f32 * ch;
        let cursor = pane.cursor.filter(|c| c.y == y);
        let block_at = |x: usize, cell: &Cell| {
            cursor.is_some_and(|c| {
                c.shape == CursorShape::Block
                    && (c.x == x || (c.x + 1 == x && cell.flags.contains(Flags::WIDE_SPACER)))
            })
        };
        let colours = |x: usize, cell: &Cell| {
            if block_at(x, cell) {
                return (theme.cursor_text, theme.cursor);
            }
            let (mut fg, bg) = (theme.fg(cell.fg), theme.bg(cell.bg));
            if cell.flags.contains(Flags::DIM) {
                fg = blend(fg, bg);
            }
            if cell.flags.contains(Flags::INVERSE) {
                (bg, fg)
            } else {
                (fg, bg)
            }
        };

        // The row's full width, so that the side padding is repainted too.
        fill(
            &rect(bounds.left, top, bounds.right, top + ch),
            theme.background,
        );
        for (x, cell) in row.iter().enumerate() {
            let (_, bg) = colours(x, cell);
            if bg != theme.background {
                let left = ox + x as f32 * cw;
                fill(&rect(left, top, left + cw, top + ch), bg);
            }
        }
        for (x, cell) in row.iter().enumerate() {
            let left = ox + x as f32 * cw;
            let (fg, _) = colours(x, cell);
            if !cell.is_empty_glyph()
                && let Some(slot) = atlas.slots.get(&GlyphKey::of(cell, pane.grid))
            {
                let src = slot_rect(slot);
                let dst = rect(left, top, left + slot.w as f32, top + slot.h as f32);
                // SAFETY: drawing call inside the caller's BeginDraw.
                unsafe {
                    brush.SetColor(&color(fg));
                    rt.FillOpacityMask(
                        &atlas.bitmap,
                        brush,
                        D2D1_OPACITY_MASK_CONTENT_TEXT_NATURAL,
                        Some(&dst),
                        Some(&src),
                    );
                }
            }
            if cell.flags.contains(Flags::UNDERLINE) {
                let u = top + m.underline_top;
                fill(&rect(left, u, left + cw, u + m.underline_thickness), fg);
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                // Through the middle of lowercase letters.
                let s = top + (m.baseline * 0.7).round();
                fill(&rect(left, s, left + cw, s + m.underline_thickness), fg);
            }
        }
        if let Some(c) = cursor {
            let left = ox + c.x as f32 * cw;
            let bar = (m.underline_thickness * 2.0).max(2.0).round();
            match c.shape {
                CursorShape::Underline => {
                    fill(
                        &rect(left, top + ch - bar, left + cw, top + ch),
                        theme.cursor,
                    );
                }
                CursorShape::Beam => fill(&rect(left, top, left + bar, top + ch), theme.cursor),
                CursorShape::Block | CursorShape::Hollow => {}
            }
        }
        if let Some(c) = cursor.filter(|c| c.shape == CursorShape::Hollow) {
            let wide = row
                .get(c.x)
                .is_some_and(|cell| cell.flags.contains(Flags::WIDE));
            let left = ox + c.x as f32 * cw;
            let right = left + if wide { 2.0 * cw } else { cw };
            let outline = rect(left + 0.5, top + 0.5, right - 0.5, top + ch - 0.5);
            // SAFETY: drawing call inside the caller's BeginDraw.
            unsafe {
                brush.SetColor(&color(theme.cursor));
                rt.DrawRectangle(&outline, brush, 1.0, None::<&ID2D1StrokeStyle>);
            }
        }
    }
    // SAFETY: matches the push above.
    unsafe { rt.PopAxisAlignedClip() };
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn slot_rect(s: &Slot) -> D2D_RECT_F {
    rect(
        s.x as f32,
        s.y as f32,
        (s.x + s.w) as f32,
        (s.y + s.h) as f32,
    )
}

/// Halfway between two colours, for dim text.
fn blend(a: Rgb, b: Rgb) -> Rgb {
    let mix = |x: u8, y: u8| ((u16::from(x) + u16::from(y)) / 2) as u8;
    Rgb::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
}

fn color(c: Rgb) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: 1.0,
    }
}
