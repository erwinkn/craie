//! Text subsystem (ARCHITECTURE.md §5, §6): owned paragraph layout over
//! HarfRust and skrifa -> Swash raster -> glyph cache -> stable
//! `RasterId`s in the scene's raster atlas -> chunk-local glyph instances
//! whose brush is a paint slot.
//!
//! Text nodes lay out through `paragraph` (owned). Text inputs still use
//! Parley's editor until owned editing lands (step 3b); both paths share
//! the `FontStore` and the glyph cache.
//!
//! Coarse resources (font store, shaping buffers, `ScaleContext`, Parley
//! contexts) are created once and reused; nothing here is per-paragraph.

mod cache;
pub mod fonts;
pub mod paragraph;
mod raster;
#[cfg(test)]
mod tests;

pub use cache::{CacheStats, CachedGlyph, GlyphCache, GlyphKey};
pub use raster::Rasterizer;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use craie_core::Point;
use craie_scene::{ChunkWriter, PaintSlot, RasterAtlas, RasterId};
use parley::layout::{GlyphRun, Layout, PositionedLayoutItem};
use parley::style::StyleProperty;
use parley::{Alignment, AlignmentOptions, FontContext, LayoutContext};
use swash::zeno::Vector;

use fonts::{FontAttrs, FontBlob, FontInstanceId, FontSource, FontStore, ScriptTag, Synthesis};
use paragraph::{HIDDEN, Paragraph, Resolve, Shaper, TextSpec, WrapScratch};

pub use parley;
pub use swash;

/// A styled range within one Parley paragraph's text (inputs).
#[derive(Clone)]
pub struct TextSpan {
    pub range: Range<usize>,
    pub style: StyleProperty<'static, PaintSlot>,
}

/// Inputs for laying out one Parley paragraph (inputs and their
/// placeholders, until step 3b).
pub struct ParagraphSpec<'a> {
    pub text: &'a str,
    /// Default styles for the whole paragraph.
    pub defaults: &'a [StyleProperty<'static, PaintSlot>],
    /// Ranged style overrides.
    pub spans: &'a [TextSpan],
}

/// Font resolution over the platform's `FontSource`: the primary face per
/// (family, style) and fallback faces per cluster, cached.
pub struct Fonts {
    pub store: FontStore,
    source: Box<dyn FontSource>,
    /// (family, attrs) -> primary instance. Few entries: a linear scan,
    /// no key allocation per lookup.
    primary: Vec<(String, FontAttrs, Option<FontInstanceId>)>,
    /// The source's fallback candidates per (first character, script,
    /// attrs, emoji), in its priority order. Each cluster picks the first
    /// candidate covering all of it, so the answer depends on the cluster
    /// alone, never on text laid out before.
    candidates: HashMap<(char, ScriptTag, FontAttrs, bool), Vec<FontInstanceId>>,
}

impl Fonts {
    pub fn new(source: Box<dyn FontSource>) -> Fonts {
        Fonts {
            store: FontStore::new(),
            source,
            primary: Vec::new(),
            candidates: HashMap::new(),
        }
    }

    /// Replaces the source; faces already interned stay.
    pub fn set_source(&mut self, source: Box<dyn FontSource>) {
        self.source = source;
        self.primary.clear();
        self.candidates.clear();
    }
}

impl Resolve for Fonts {
    fn primary(&mut self, family: &str, attrs: FontAttrs) -> Option<FontInstanceId> {
        if let Some((_, _, f)) = self
            .primary
            .iter()
            .find(|(fam, a, _)| fam == family && *a == attrs)
        {
            return *f;
        }
        let blob = self
            .source
            .select(family, attrs)
            .or_else(|| self.source.select("sans-serif", attrs));
        let font = blob.and_then(|b| self.store.instance_of(&b));
        self.primary.push((family.to_string(), attrs, font));
        font
    }

    fn fallback(
        &mut self,
        cluster: &str,
        script: ScriptTag,
        attrs: FontAttrs,
    ) -> Option<FontInstanceId> {
        let first = cluster.chars().find(|&c| !paragraph::ignorable(c))?;
        let emoji = fonts::emoji_presentation(cluster);
        let key = (first, script, attrs, emoji);
        if !self.candidates.contains_key(&key) {
            let blobs = self.source.fallback(first, script, attrs, emoji);
            let list = blobs
                .iter()
                .filter_map(|b| self.store.instance_of(b))
                .collect();
            self.candidates.insert(key, list);
        }
        let list = &self.candidates[&key];
        let store = &mut self.store;
        // The first candidate that covers the whole cluster; else the first
        // (it covers the first character).
        list.iter()
            .copied()
            .find(|&f| paragraph::covers_cluster(store, f, cluster))
            .or(list.first().copied())
    }

    fn store(&mut self) -> &mut FontStore {
        &mut self.store
    }
}

pub struct TextEngine {
    /// Parley contexts for inputs (until step 3b), made on first use:
    /// `FontContext::new` scans the system fonts.
    parley: Option<Box<(FontContext, LayoutContext<PaintSlot>)>>,
    pub fonts: Fonts,
    shaper: Shaper,
    wrap: WrapScratch,
    raster: Rasterizer,
    pub cache: GlyphCache,
    /// The family of a `TextSpec` with an empty family: `system-ui`, React
    /// Native's default (ARCHITECTURE.md §5, Decisions).
    pub default_family: String,
    /// Paragraphs shaped (cost counter): owned paragraphs and Parley
    /// layouts alike. Rewrapping at another width is not shaping.
    pub shapes: u64,
}

/// Per-`emit` counters, for experiments and instrumentation.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmitStats {
    pub glyph_runs: u32,
    pub glyphs: u32,
}

impl TextEngine {
    /// An engine over the default font source (`fonts::default_source`).
    pub fn new() -> TextEngine {
        TextEngine::with_source(fonts::default_source())
    }

    pub fn with_source(source: Box<dyn FontSource>) -> TextEngine {
        TextEngine {
            parley: None,
            default_family: "system-ui".to_string(),
            fonts: Fonts::new(source),
            shaper: Shaper::default(),
            wrap: WrapScratch::default(),
            raster: Rasterizer::new(),
            cache: GlyphCache::new(),
            shapes: 0,
        }
    }

    /// Shapes `spec` and lays it out at `max_width` (logical points; None:
    /// unbounded). The display scale does not take part: a scale change
    /// never reshapes.
    pub fn layout_text(&mut self, spec: &TextSpec<'_>, max_width: Option<f32>) -> Paragraph {
        self.shapes += 1;
        let spec = TextSpec {
            family: if spec.family.is_empty() {
                &self.default_family
            } else {
                spec.family
            },
            ..*spec
        };
        let mut p = self.shaper.shape(&spec, &mut self.fonts);
        p.rewrap_with(max_width, &mut self.wrap);
        p
    }

    /// Lays out an already shaped paragraph at another width.
    pub fn rewrap(&mut self, p: &mut Paragraph, max_width: Option<f32>) {
        p.rewrap_with(max_width, &mut self.wrap);
    }

    /// A raster's identity independent of interning order (test oracle).
    pub fn stable_key(&self, id: RasterId) -> Option<u64> {
        self.cache.stable_key(id, &self.fonts.store)
    }

    /// Appends a paragraph's glyphs to a chunk at `origin` (chunk-local
    /// logical units). Each glyph takes its span's paint slot, or `brush`
    /// when set. Positions come from the paragraph's placement store;
    /// only raster misses do Swash work.
    ///
    /// Glyph positions are quantized on the physical-pixel grid relative
    /// to the chunk origin, which the renderer snaps to a whole device
    /// pixel at rest, so a moved chunk reuses every raster.
    pub fn emit_paragraph(
        &mut self,
        p: &Paragraph,
        origin: Point,
        scale: f32,
        brush: Option<PaintSlot>,
        atlas: &mut RasterAtlas,
        out: &mut ChunkWriter,
    ) -> EmitStats {
        let mut stats = EmitStats::default();
        let TextEngine {
            raster,
            cache,
            fonts,
            ..
        } = self;
        for run in &p.runs {
            stats.glyph_runs += 1;
            let glyphs = p.glyphs[run.glyphs.start as usize..run.glyphs.end as usize]
                .iter()
                .filter(|g| g.id != HIDDEN)
                .map(|g| {
                    (
                        g.id,
                        (origin.x + g.x + g.dx) * scale,
                        (origin.y + g.y) * scale,
                        brush.unwrap_or(PaintSlot(g.style as u32)),
                    )
                });
            emit_glyphs(
                raster,
                cache,
                &fonts.store,
                run.font,
                run.size * scale,
                scale,
                glyphs,
                atlas,
                out,
                &mut stats,
            );
        }
        stats
    }

    /// The Parley contexts (inputs until step 3b), made on first use.
    pub fn parley(&mut self) -> (&mut FontContext, &mut LayoutContext<PaintSlot>) {
        let cx = self
            .parley
            .get_or_insert_with(|| Box::new((FontContext::new(), LayoutContext::new())));
        (&mut cx.0, &mut cx.1)
    }

    /// Lays out one Parley paragraph in logical units (inputs until step
    /// 3b). `max_width` is the wrap width in logical units.
    pub fn layout_paragraph(
        &mut self,
        spec: &ParagraphSpec,
        max_width: Option<f32>,
    ) -> Layout<PaintSlot> {
        self.shapes += 1;
        let (font_cx, layout_cx) = self.parley();
        let mut builder = layout_cx.ranged_builder(font_cx, spec.text, 1.0, false);
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

    /// Appends a Parley layout's glyphs to a chunk (inputs until step 3b),
    /// through the same font store and glyph cache as owned paragraphs.
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
                    self.emit_parley_run(&glyph_run, origin, scale, brush, atlas, out, &mut stats);
                }
            }
        }
        stats
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_parley_run(
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
        let synthesis = run.synthesis();
        debug_assert!(font.data.id() < fonts::RAW_ID_BASE);
        let blob = FontBlob {
            id: font.data.id(),
            index: font.index,
            bytes: Arc::new(font.data.clone()),
            synthesis: Synthesis {
                embolden: synthesis.embolden(),
                skew: synthesis.skew().unwrap_or(0.0),
            },
            variations: Vec::new(),
        };
        let store = &mut self.fonts.store;
        let Some(instance) = store
            .face(&blob)
            .and_then(|face| store.instance(face, run.normalized_coords(), blob.synthesis))
        else {
            return;
        };
        let slot = brush.unwrap_or(glyph_run.style().brush);
        let baseline = (origin.y + glyph_run.baseline()) * scale;
        let mut run_x = (origin.x + glyph_run.offset()) * scale;
        let glyphs: Vec<(u16, f32, f32, PaintSlot)> = glyph_run
            .glyphs()
            .map(|g| {
                let x = run_x + g.x * scale;
                run_x += g.advance * scale;
                (g.id as u16, x, baseline + g.y * scale, slot)
            })
            .collect();
        let TextEngine {
            raster,
            cache,
            fonts,
            ..
        } = self;
        emit_glyphs(
            raster,
            cache,
            &fonts.store,
            instance,
            run.font_size() * scale,
            scale,
            glyphs.into_iter(),
            atlas,
            out,
            stats,
        );
    }

    /// Rasterizes `ids` again after eviction. Needs neither the paragraph
    /// nor its layout: the cache kept each raster's key, and the store
    /// the font instance.
    pub fn ensure_resident(&mut self, ids: &[RasterId], atlas: &mut RasterAtlas) {
        for &id in ids {
            if atlas.entry(id).resident {
                continue;
            }
            let Some(key) = self.cache.key_of(id) else {
                continue;
            };
            let inst = self.fonts.store.instance_data(key.font);
            let face = self.fonts.store.face_data(inst.face);
            // The size the raster was made at: smaller than the key's
            // size for a glyph downscaled to fit a page (made without a
            // subpixel offset).
            let key_size = f32::from_bits(key.size_bits);
            let size = self.cache.raster_size(id).unwrap_or(key_size);
            let downscaled = size != key_size;
            let embolden = if inst.synthesis.embolden {
                size * 0.02
            } else {
                0.0
            };
            let skew = (inst.synthesis.skew != 0.0).then_some(inst.synthesis.skew);
            let Some(mut scaler) =
                self.raster
                    .scaler(face.bytes.as_ref().as_ref(), face.index, size, &inst.coords)
            else {
                continue;
            };
            let offset = if downscaled {
                Vector::new(0.0, 0.0)
            } else {
                Vector::new(
                    cache::subpixel_offset(key.subpixel & 3),
                    cache::subpixel_offset(key.subpixel >> 2),
                )
            };
            if let Some(r) = Rasterizer::render(&mut scaler, key.glyph, offset, embolden, skew) {
                self.cache.stats.rasters += 1;
                self.cache.stats.rerasters += 1;
                atlas.insert(id, &r.image.data);
            }
        }
    }
}

/// Emits glyphs of one font instance at `size_px` (physical pixels):
/// `glyphs` yields (glyph id, physical x, physical y, paint slot). Cache
/// hits pin their raster; misses rasterize (a glyph larger than an atlas
/// page is rasterized smaller and drawn scaled up). Positions pass
/// through: the raster path keeps bitmap geometry only.
#[allow(clippy::too_many_arguments)]
fn emit_glyphs(
    raster: &mut Rasterizer,
    cache: &mut GlyphCache,
    store: &FontStore,
    font: FontInstanceId,
    size_px: f32,
    scale: f32,
    glyphs: impl Iterator<Item = (u16, f32, f32, PaintSlot)>,
    atlas: &mut RasterAtlas,
    out: &mut ChunkWriter,
    stats: &mut EmitStats,
) {
    let inst = store.instance_data(font);
    let face = store.face_data(inst.face);
    let bytes = face.bytes.as_ref().as_ref();
    let embolden = if inst.synthesis.embolden {
        size_px * 0.02
    } else {
        0.0
    };
    let skew = (inst.synthesis.skew != 0.0).then_some(inst.synthesis.skew);
    // Lazy: a run whose glyphs all hit the cache never builds a scaler.
    let mut scaler = None;
    let inv = 1.0 / scale;
    for (glyph, gx, gy, slot) in glyphs {
        stats.glyphs += 1;
        let (ix, sx) = cache::quantize_subpixel(gx);
        let (iy, sy) = cache::quantize_subpixel(gy);
        let key = GlyphKey {
            font,
            glyph,
            size_bits: size_px.to_bits(),
            subpixel: sx | (sy << 2),
        };
        let cached = match cache.get(&key) {
            Some(c) => {
                // Pin for this frame; if evicted earlier, `prepare`
                // re-rasterizes it before drawing.
                if let Some(r) = c.raster {
                    atlas.touch(r);
                }
                c
            }
            None => {
                if scaler.is_none() {
                    scaler = raster.scaler(bytes, face.index, size_px, &inst.coords);
                }
                let Some(sc) = scaler.as_mut() else {
                    continue;
                };
                let Some(rastered) = Rasterizer::render(
                    sc,
                    glyph,
                    Vector::new(cache::subpixel_offset(sx), cache::subpixel_offset(sy)),
                    embolden,
                    skew,
                ) else {
                    continue;
                };
                cache.stats.rasters += 1;
                let p = rastered.image.placement;
                let c = if p.width == 0 || p.height == 0 {
                    // Zero-area glyphs draw nothing.
                    CachedGlyph {
                        raster: None,
                        w: 0,
                        h: 0,
                        left: 0,
                        top: 0,
                    }
                } else if atlas.fits(p.width, p.height) {
                    let id = atlas.new_id(p.width as u16, p.height as u16, rastered.color);
                    atlas.insert(id, &rastered.image.data);
                    cache.set_raster_size(id, size_px);
                    CachedGlyph {
                        raster: Some(id),
                        w: p.width as u16,
                        h: p.height as u16,
                        left: p.left as i16,
                        top: p.top as i16,
                    }
                } else {
                    // Larger than a page: rasterize smaller so it fits, and
                    // draw the bitmap scaled up to its full size. Softer,
                    // but the glyph renders. Drops the run's scaler: the
                    // context builds one scaler at a time.
                    scaler = None;
                    let fit = (atlas.page_size() - 4) as f32;
                    let k = (fit / p.width as f32).min(fit / p.height as f32);
                    let small_size = size_px * k;
                    let Some(mut small) =
                        raster.scaler(bytes, face.index, small_size, &inst.coords)
                    else {
                        continue;
                    };
                    let Some(r) = Rasterizer::render(
                        &mut small,
                        glyph,
                        Vector::new(0.0, 0.0),
                        embolden * k,
                        skew,
                    ) else {
                        continue;
                    };
                    cache.stats.rasters += 1;
                    atlas.stats.downscaled += 1;
                    let q = r.image.placement;
                    let quad_w = (q.width as f32 / k).ceil().min(u16::MAX as f32) as u16;
                    let quad_h = (q.height as f32 / k).ceil().min(u16::MAX as f32) as u16;
                    let id = atlas.new_scaled_id(
                        q.width as u16,
                        q.height as u16,
                        quad_w,
                        quad_h,
                        r.color,
                    );
                    atlas.insert(id, &r.image.data);
                    cache.set_raster_size(id, small_size);
                    CachedGlyph {
                        raster: Some(id),
                        w: quad_w,
                        h: quad_h,
                        left: (q.left as f32 / k).round() as i16,
                        top: (q.top as f32 / k).round() as i16,
                    }
                };
                cache.insert(key, c);
                c
            }
        };
        let Some(raster_id) = cached.raster else {
            continue;
        };
        out.glyph(
            (ix + cached.left as i32) as f32 * inv,
            (iy - cached.top as i32) as f32 * inv,
            cached.w as f32 * inv,
            cached.h as f32 * inv,
            raster_id,
            slot,
        );
    }
}

impl Default for TextEngine {
    fn default() -> TextEngine {
        TextEngine::new()
    }
}
