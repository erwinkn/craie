//! Text subsystem: Parley layout -> Swash raster -> Craie cache/atlas ->
//! flat `GlyphInstance` rows.
//!
//! Coarse resources (`FontContext`, `LayoutContext`, `ScaleContext`) are
//! created once and reused; nothing here is per-paragraph.

mod atlas;
mod cache;
mod raster;

pub use atlas::{ATLAS_PAGE_SIZE, AtlasStats, GlyphAtlas};
pub use cache::{CacheStats, GlyphCache, GlyphKey};
pub use raster::Rasterizer;

use std::ops::Range;

use parley::layout::{GlyphRun, Layout, PositionedLayoutItem};
use parley::style::StyleProperty;
use parley::{Alignment, AlignmentOptions, FontContext, LayoutContext};
use swash::zeno::Vector;

use crate::geom::Point;
use crate::scene::{Color, GlyphInstance};

pub use parley;
pub use swash;

/// A styled range within one paragraph's text.
#[derive(Clone)]
pub struct TextSpan {
    pub range: Range<usize>,
    pub style: StyleProperty<'static, Color>,
}

/// Inputs for laying out one paragraph. This is the shape the host bridge
/// will eventually produce; for milestone 0 the example builds it directly.
pub struct ParagraphSpec<'a> {
    pub text: &'a str,
    /// Default styles for the whole paragraph.
    pub defaults: Vec<StyleProperty<'static, Color>>,
    /// Ranged style overrides.
    pub spans: Vec<TextSpan>,
}

pub struct TextEngine {
    pub font_cx: FontContext,
    pub layout_cx: LayoutContext<Color>,
    raster: Rasterizer,
    pub cache: GlyphCache,
    pub atlas: GlyphAtlas,
}

/// Per-`emit` counters, for experiments and instrumentation.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmitStats {
    pub glyph_runs: u32,
    pub glyphs: u32,
    pub cache_hits_before: u64,
    pub cache_misses_before: u64,
}

impl TextEngine {
    pub fn new() -> TextEngine {
        TextEngine {
            font_cx: FontContext::new(),
            layout_cx: LayoutContext::new(),
            raster: Rasterizer::new(),
            cache: GlyphCache::new(),
            atlas: GlyphAtlas::new(),
        }
    }

    /// Lays out one paragraph. `scale` is the display scale factor (2.0 on
    /// Retina); `max_width` is the wrap width in *physical* pixels.
    ///
    /// The returned layout borrows nothing — it owns its shaped data and can
    /// be retained across frames.
    pub fn layout_paragraph(
        &mut self,
        spec: &ParagraphSpec,
        scale: f32,
        max_width: Option<f32>,
    ) -> Layout<Color> {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, spec.text, scale, true);
        for default in &spec.defaults {
            builder.push_default(default.clone());
        }
        for span in &spec.spans {
            builder.push(span.style.clone(), span.range.clone());
        }
        let mut layout: Layout<Color> = builder.build(spec.text);
        layout.break_all_lines(max_width);
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    /// Walks a laid-out paragraph and appends glyph instances positioned at
    /// `origin` (physical pixels). Rasterizes and atlas-allocates only cache
    /// misses, so re-emitting an unchanged paragraph performs no Swash work.
    pub fn emit(
        &mut self,
        layout: &Layout<Color>,
        origin: Point,
        out: &mut Vec<GlyphInstance>,
    ) -> EmitStats {
        let mut stats = EmitStats {
            cache_hits_before: self.cache.stats.hits,
            cache_misses_before: self.cache.stats.misses,
            ..EmitStats::default()
        };
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(glyph_run) = item {
                    stats.glyph_runs += 1;
                    self.emit_run(&glyph_run, origin, out, &mut stats);
                }
            }
        }
        stats
    }

    fn emit_run(
        &mut self,
        glyph_run: &GlyphRun<'_, Color>,
        origin: Point,
        out: &mut Vec<GlyphInstance>,
        stats: &mut EmitStats,
    ) {
        let run = glyph_run.run();
        let font = run.font();
        let font_size = run.font_size();
        let coords = run.normalized_coords();
        let synthesis = run.synthesis();
        let skew_q = (synthesis.skew().unwrap_or(0.0) * 64.0) as i16;

        let font_slot = self.cache.font_slot(font.data.id(), font.index);
        let coords_slot = self.cache.coords_slot(coords, synthesis.embolden(), skew_q);

        let color = glyph_run.style().brush;
        let baseline = origin.y + glyph_run.baseline();
        let mut run_x = origin.x + glyph_run.offset();

        let embolden = if synthesis.embolden() {
            font_size * 0.02
        } else {
            0.0
        };
        let mut scaler = self.raster.scaler(font, font_size, coords);

        for glyph in glyph_run.glyphs() {
            let gx = run_x + glyph.x;
            let gy = baseline + glyph.y;
            run_x += glyph.advance;
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

            let entry = match self.cache.get(&key) {
                Some(entry) => entry,
                None => {
                    let Some(scaler) = scaler.as_mut() else {
                        continue;
                    };
                    let Some(rastered) = Rasterizer::render(
                        scaler,
                        glyph.id as u16,
                        Vector::new(cache::subpixel_offset(sx), cache::subpixel_offset(sy)),
                        embolden,
                    ) else {
                        continue;
                    };
                    self.cache.stats.rasters += 1;
                    let entry = Self::insert_into_atlas(&mut self.atlas, &rastered);
                    self.cache.insert(key, entry);
                    entry
                }
            };

            if entry.w == 0 || entry.h == 0 {
                continue;
            }
            let atlas_size = ATLAS_PAGE_SIZE as f32;
            let flags = if entry.color {
                GlyphInstance::FLAG_COLOR
            } else {
                0
            };
            out.push(GlyphInstance {
                position: [
                    (ix + entry.left as i32) as f32,
                    (iy - entry.top as i32) as f32,
                ],
                size: [entry.w as f32, entry.h as f32],
                uv_min: [entry.x as f32 / atlas_size, entry.y as f32 / atlas_size],
                uv_max: [
                    (entry.x + entry.w) as f32 / atlas_size,
                    (entry.y + entry.h) as f32 / atlas_size,
                ],
                color: color.0,
                page: entry.page,
                flags,
            });
        }
    }

    fn insert_into_atlas(
        atlas: &mut GlyphAtlas,
        rastered: &raster::Rastered,
    ) -> cache::CachedGlyph {
        let p = &rastered.image.placement;
        let (w, h) = (p.width, p.height);
        if w == 0 || h == 0 {
            return cache::CachedGlyph {
                page: 0,
                color: false,
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                left: 0,
                top: 0,
            };
        }
        let slot = if rastered.color {
            atlas.write_color(w, h, &rastered.image.data)
        } else {
            atlas.write_alpha(w, h, &rastered.image.data)
        };
        let slot = slot.expect("atlas allocation failed");
        cache::CachedGlyph {
            page: slot.page,
            color: rastered.color,
            x: slot.x,
            y: slot.y,
            w: w as u16,
            h: h as u16,
            left: p.left as i16,
            top: p.top as i16,
        }
    }
}

impl Default for TextEngine {
    fn default() -> TextEngine {
        TextEngine::new()
    }
}
