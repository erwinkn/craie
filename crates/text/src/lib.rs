//! Text subsystem: Parley layout -> Swash raster -> glyph cache ->
//! stable `RasterId`s in the scene's raster atlas -> chunk-local glyph
//! instances whose brush is a paint slot.
//!
//! Coarse resources (`FontContext`, `LayoutContext`, `ScaleContext`) are
//! created once and reused; nothing here is per-paragraph.

mod cache;
mod raster;

pub use cache::{CacheStats, CachedGlyph, GlyphCache, GlyphKey};
pub use raster::Rasterizer;

use std::ops::Range;

use craie_core::Point;
use craie_scene::{ChunkWriter, PaintSlot, RasterAtlas, RasterId};
use parley::layout::{GlyphRun, Layout, PositionedLayoutItem};
use parley::style::StyleProperty;
use parley::{Alignment, AlignmentOptions, FontContext, LayoutContext};
use swash::zeno::Vector;

pub use parley;
pub use swash;

/// A styled range within one paragraph's text.
#[derive(Clone)]
pub struct TextSpan {
    pub range: Range<usize>,
    pub style: StyleProperty<'static, PaintSlot>,
}

/// Inputs for laying out one paragraph. Borrowed throughout — laying out
/// a paragraph allocates nothing on the spec itself.
pub struct ParagraphSpec<'a> {
    pub text: &'a str,
    /// Default styles for the whole paragraph.
    pub defaults: &'a [StyleProperty<'static, PaintSlot>],
    /// Ranged style overrides.
    pub spans: &'a [TextSpan],
}

pub struct TextEngine {
    pub font_cx: FontContext,
    pub layout_cx: LayoutContext<PaintSlot>,
    raster: Rasterizer,
    pub cache: GlyphCache,
    /// Paragraphs shaped and broken into lines (cost counter).
    pub shapes: u64,
}

/// Per-`emit` counters, for experiments and instrumentation.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmitStats {
    pub glyph_runs: u32,
    pub glyphs: u32,
}

impl TextEngine {
    pub fn new() -> TextEngine {
        TextEngine {
            font_cx: FontContext::new(),
            layout_cx: LayoutContext::new(),
            raster: Rasterizer::new(),
            cache: GlyphCache::new(),
            shapes: 0,
        }
    }

    /// Lays out one paragraph in logical units — the display scale does
    /// not participate in layout, so a monitor-scale change never reshapes.
    /// `max_width` is the wrap width in logical units.
    ///
    /// The returned layout borrows nothing — it owns its shaped data and can
    /// be retained across frames.
    pub fn layout_paragraph(
        &mut self,
        spec: &ParagraphSpec,
        max_width: Option<f32>,
    ) -> Layout<PaintSlot> {
        self.shapes += 1;
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, spec.text, 1.0, false);
        for default in spec.defaults {
            builder.push_default(default.clone());
        }
        for span in spec.spans {
            builder.push(span.style.clone(), span.range.clone());
        }
        let mut layout: Layout<PaintSlot> = builder.build(spec.text);
        layout.break_all_lines(max_width);
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    /// Appends a laid-out paragraph's glyphs to a chunk, positioned at
    /// `origin` (chunk-local logical units). Each glyph takes its run's
    /// brush as its paint slot, or `brush` when set. Rasterizes only
    /// cache misses; re-emitting an unchanged paragraph does no Swash work.
    ///
    /// Glyph positions are quantized on the physical-pixel grid relative
    /// to the chunk origin, which the renderer snaps to a whole device
    /// pixel, so a moved chunk reuses every raster.
    pub fn emit(
        &mut self,
        layout: &Layout<PaintSlot>,
        origin: Point,
        scale: f32,
        brush: Option<PaintSlot>,
        atlas: &mut RasterAtlas,
        out: &mut ChunkWriter,
    ) -> EmitStats {
        let mut stats = EmitStats::default();
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(glyph_run) = item {
                    stats.glyph_runs += 1;
                    self.emit_run(&glyph_run, origin, scale, brush, atlas, out, &mut stats);
                }
            }
        }
        stats
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_run(
        &mut self,
        glyph_run: &GlyphRun<'_, PaintSlot>,
        origin: Point,
        scale: f32,
        brush: Option<PaintSlot>,
        atlas: &mut RasterAtlas,
        out: &mut ChunkWriter,
        stats: &mut EmitStats,
    ) {
        let run = glyph_run.run();
        let font = run.font();
        // Layout is logical; the rasterizer sees physical pixel size.
        let font_size = run.font_size() * scale;
        let coords = run.normalized_coords();
        let synthesis = run.synthesis();
        let skew_q = (synthesis.skew().unwrap_or(0.0) * 64.0) as i16;

        let font_slot = self.cache.font_slot(font);
        let coords_slot = self.cache.coords_slot(coords, synthesis.embolden(), skew_q);

        let slot = brush.unwrap_or(glyph_run.style().brush);
        let baseline = (origin.y + glyph_run.baseline()) * scale;
        let mut run_x = (origin.x + glyph_run.offset()) * scale;

        let embolden = if synthesis.embolden() {
            font_size * 0.02
        } else {
            0.0
        };
        let skew = synthesis.skew();
        // Lazy: a run whose glyphs all hit the cache never builds a scaler.
        let mut scaler = None;
        let inv = 1.0 / scale;

        for glyph in glyph_run.glyphs() {
            let gx = run_x + glyph.x * scale;
            let gy = baseline + glyph.y * scale;
            run_x += glyph.advance * scale;
            stats.glyphs += 1;

            let (ix, sx) = cache::quantize_subpixel(gx);
            let (iy, sy) = cache::quantize_subpixel(gy);
            let key = GlyphKey {
                font: font_slot,
                coords: coords_slot,
                glyph: glyph.id as u16,
                size_bits: font_size.to_bits(),
                subpixel: sx | (sy << 2),
                _pad: 0,
            };

            let cached = match self.cache.get(&key) {
                Some(c) => c,
                None => {
                    if scaler.is_none() {
                        scaler = self.raster.scaler(font, font_size, coords);
                    }
                    let Some(scaler) = scaler.as_mut() else {
                        continue;
                    };
                    let Some(rastered) = Rasterizer::render(
                        scaler,
                        glyph.id as u16,
                        Vector::new(cache::subpixel_offset(sx), cache::subpixel_offset(sy)),
                        embolden,
                        skew,
                    ) else {
                        continue;
                    };
                    self.cache.stats.rasters += 1;
                    let p = rastered.image.placement;
                    let raster = if p.width == 0 || p.height == 0 {
                        None
                    } else {
                        let id = atlas.new_id(p.width as u16, p.height as u16, rastered.color);
                        atlas.insert(id, &rastered.image.data);
                        Some(id)
                    };
                    let c = CachedGlyph {
                        raster,
                        w: p.width as u16,
                        h: p.height as u16,
                        left: p.left as i16,
                        top: p.top as i16,
                    };
                    self.cache.insert(key, c);
                    c
                }
            };
            let Some(raster) = cached.raster else { continue };
            out.glyph(
                (ix + cached.left as i32) as f32 * inv,
                (iy - cached.top as i32) as f32 * inv,
                cached.w as f32 * inv,
                cached.h as f32 * inv,
                raster,
                slot,
            );
        }
    }

    /// Rasterizes `ids` again after eviction. Needs neither the paragraph
    /// nor its layout: the cache kept each raster's key and font.
    pub fn ensure_resident(&mut self, ids: &[RasterId], atlas: &mut RasterAtlas) {
        for &id in ids {
            if atlas.entry(id).resident {
                continue;
            }
            let Some(key) = self.cache.key_of(id) else { continue };
            let font = self.cache.font_data[key.font as usize].clone();
            let row = &self.cache.coords[key.coords as usize];
            let coords: Vec<i16> = row.coords.to_vec();
            let size = f32::from_bits(key.size_bits);
            let embolden = if row.embolden { size * 0.02 } else { 0.0 };
            let skew = (row.skew != 0).then(|| row.skew as f32 / 64.0);
            let Some(mut scaler) = self.raster.scaler(&font, size, &coords) else { continue };
            let offset = Vector::new(
                cache::subpixel_offset(key.subpixel & 3),
                cache::subpixel_offset(key.subpixel >> 2),
            );
            if let Some(r) = Rasterizer::render(&mut scaler, key.glyph, offset, embolden, skew) {
                self.cache.stats.rasters += 1;
                self.cache.stats.rerasters += 1;
                atlas.insert(id, &r.image.data);
            }
        }
    }
}

impl Default for TextEngine {
    fn default() -> TextEngine {
        TextEngine::new()
    }
}
