//! Text subsystem (ARCHITECTURE.md §5, §6): owned paragraph layout over
//! HarfRust and skrifa -> Swash raster -> glyph cache -> stable
//! `RasterId`s in the scene's raster atlas -> chunk-local glyph instances
//! whose brush is a paint slot.
//!
//! Text nodes lay out through `paragraph`, text inputs through `editor`
//! over the same paragraph; both share the `FontStore` and the glyph
//! cache. Parley is not a dependency: it is the harness's oracle (E01).
//!
//! Coarse resources (font store, shaping buffers, plans, `ScaleContext`,
//! wrap scratch) are created once and reused; nothing here is
//! per-paragraph.

mod cache;
pub mod editor;
pub mod fonts;
pub mod paragraph;
mod raster;
#[cfg(test)]
mod tests;

pub use cache::{CacheStats, CachedGlyph, GlyphCache, GlyphKey};
pub use raster::Rasterizer;

use std::collections::HashMap;

use craie_core::Point;
use craie_scene::{ChunkWriter, PaintSlot, RasterAtlas, RasterId};
use swash::zeno::Vector;

use fonts::{FontAttrs, FontInstanceId, FontSource, FontStore, ScriptTag};
use paragraph::{HIDDEN, Paragraph, Resolve, Shaper, TextSpec, WrapScratch};

pub use swash;

/// Fallback candidates per cluster text.
type ClusterCandidates = HashMap<Box<str>, Vec<FontInstanceId>>;

/// Span decoration flags (`TextEngine::emit_paragraph`).
pub mod decoration {
    pub const UNDERLINE: u8 = 1;
    pub const LINE_THROUGH: u8 = 2;
}

/// Underline and line-through rects for `emit_paragraph`: per segment,
/// each stretch of glyphs of one decorated span, from its placements.
fn emit_decorations(
    p: &Paragraph,
    origin: Point,
    brush: Option<PaintSlot>,
    decorations: &[u8],
    out: &mut ChunkWriter,
) {
    for line in &p.lines {
        for seg in &p.segs[line.segs.start as usize..line.segs.end as usize] {
            let m = &p.runs[seg.run as usize].metrics;
            let g = &p.glyphs[seg.glyphs.start as usize..seg.glyphs.end as usize];
            let mut i = 0;
            while i < g.len() {
                let style = g[i].style;
                let mut j = i;
                let (mut x0, mut x1) = (f32::MAX, f32::MIN);
                while j < g.len() && g[j].style == style {
                    if g[j].id != HIDDEN {
                        x0 = x0.min(g[j].x);
                        x1 = x1.max(g[j].x + g[j].advance);
                    }
                    j += 1;
                }
                let d = decorations.get(style as usize).copied().unwrap_or(0);
                if d != 0 && x1 > x0 {
                    let slot = brush.unwrap_or(PaintSlot(style as u32));
                    let mut bar = |offset: f32, size: f32| {
                        let top = origin.y + line.baseline - offset;
                        out.rect(
                            craie_core::geom::Rect::new(origin.x + x0, top, x1 - x0, size),
                            0.0,
                            slot,
                        );
                    };
                    if d & decoration::UNDERLINE != 0 {
                        bar(m.underline_offset, m.underline_size);
                    }
                    if d & decoration::LINE_THROUGH != 0 {
                        bar(m.strike_offset, m.strike_size);
                    }
                }
                i = j;
            }
        }
    }
}

/// Font resolution over the platform's `FontSource`: the primary face per
/// (family, style) and fallback faces per cluster, cached.
pub struct Fonts {
    pub store: FontStore,
    source: Box<dyn FontSource>,
    /// Fonts the app registered (`register`): asked first for a family
    /// they hold; generic names stay the source's.
    registered: fonts::RawFonts,
    /// Primary instances resolved from registered fonts: their spans ask
    /// registered fonts first in fallback, as an app's bundled fallback
    /// faces. Other spans never fall back to a registered font, as a web
    /// font no `font-family` names never draws.
    registered_primaries: Vec<FontInstanceId>,
    /// (family, attrs) -> primary instance. Few entries: a linear scan,
    /// no key allocation per lookup.
    primary: Vec<(String, FontAttrs, Option<FontInstanceId>)>,
    /// Fallback candidates per (script, attrs, emoji, registered primary)
    /// and cluster, in priority order. A cluster picks the first candidate
    /// covering all of it, so the answer depends on the cluster alone,
    /// never on text laid out before.
    candidates: HashMap<(ScriptTag, FontAttrs, bool, bool), ClusterCandidates>,
    /// The family an empty family name means: `system-ui`, React Native's
    /// default (ARCHITECTURE.md §5, Decisions).
    default_family: String,
}

impl Fonts {
    pub fn new(source: Box<dyn FontSource>) -> Fonts {
        Fonts {
            store: FontStore::new(),
            source,
            registered: fonts::RawFonts::new(),
            registered_primaries: Vec::new(),
            primary: Vec::new(),
            candidates: HashMap::new(),
            default_family: "system-ui".to_string(),
        }
    }

    pub fn default_family(&self) -> &str {
        &self.default_family
    }

    /// Sets what an empty family means. Resolved defaults are forgotten;
    /// spans applied before keep the instance they resolved to.
    pub fn set_default_family(&mut self, family: &str) {
        family.clone_into(&mut self.default_family);
        self.primary.retain(|(f, _, _)| !f.is_empty());
    }

    /// Registers a font file's faces (under `family`, else the file's
    /// own family names) ahead of the source; the last registration of a
    /// face wins. Spans resolved before keep their faces until resolved
    /// again: the caller re-resolves those naming a returned family.
    /// Returns the family of each face added (lowercase; none: not a
    /// font, or the same bytes again).
    pub fn register(&mut self, bytes: fonts::FaceBytes, family: Option<&str>) -> Vec<String> {
        let added = self.registered.add_as(bytes, family);
        if !added.is_empty() {
            self.primary.clear();
            self.candidates.clear();
        }
        added
    }

    /// Replaces the source; faces already interned stay.
    pub fn set_source(&mut self, source: Box<dyn FontSource>) {
        self.source = source;
        self.primary.clear();
        self.candidates.clear();
    }
}

impl Resolve for Fonts {
    /// The primary instance of `family` ("" is the default family) in
    /// `attrs`, cached per (family, attrs).
    fn primary(&mut self, family: &str, attrs: FontAttrs) -> Option<FontInstanceId> {
        if let Some((_, _, f)) = self
            .primary
            .iter()
            .find(|(fam, a, _)| fam == family && *a == attrs)
        {
            return *f;
        }
        let name = if family.is_empty() {
            self.default_family.as_str()
        } else {
            family
        };
        let registered = if fonts::RawFonts::generic(&name.trim().to_lowercase()) {
            None
        } else {
            self.registered.select(name, attrs)
        };
        let own = registered.is_some();
        let blob = registered
            .or_else(|| self.source.select(name, attrs))
            .or_else(|| self.source.select("sans-serif", attrs));
        let font = blob.and_then(|b| self.store.instance_of(&b));
        if let Some(f) = font.filter(|f| own && !self.registered_primaries.contains(f)) {
            self.registered_primaries.push(f);
        }
        self.primary.push((family.to_string(), attrs, font));
        font
    }

    fn fallback(
        &mut self,
        cluster: &str,
        script: ScriptTag,
        attrs: FontAttrs,
        primary: Option<FontInstanceId>,
    ) -> Option<FontInstanceId> {
        cluster.chars().find(|&c| !fonts::ignorable(c))?;
        let emoji = fonts::emoji_presentation(cluster);
        let own = primary.is_some_and(|p| self.registered_primaries.contains(&p));
        let by_cluster = self
            .candidates
            .entry((script, attrs, emoji, own))
            .or_default();
        if !by_cluster.contains_key(cluster) {
            // A registered span asks registered fonts first, except for
            // emoji presentation, where the system's color emoji font
            // comes first.
            let mut blobs = Vec::new();
            let mut registered = |blobs: &mut Vec<_>| {
                if own {
                    blobs.extend(self.registered.fallback(cluster, script, attrs, emoji));
                }
            };
            if !emoji {
                registered(&mut blobs);
            }
            blobs.extend(self.source.fallback(cluster, script, attrs, emoji));
            if emoji {
                registered(&mut blobs);
            }
            let list = blobs
                .iter()
                .filter_map(|b| self.store.instance_of(b))
                .collect();
            by_cluster.insert(cluster.into(), list);
        }
        let list = &by_cluster[cluster];
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
    pub fonts: Fonts,
    shaper: Shaper,
    wrap: WrapScratch,
    raster: Rasterizer,
    pub cache: GlyphCache,
    /// Paragraphs shaped (cost counter): text nodes, inputs, and
    /// placeholders alike. Rewrapping at another width is not shaping.
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
        let mut p = self.shaper.shape(spec, &mut self.fonts);
        p.rewrap_with(max_width, &mut self.wrap);
        p
    }

    /// The primary font of `family` ("" is the default family) in
    /// `weight` and `italic`: what a span resolves to once, when applied.
    pub fn font(&mut self, family: &str, weight: u16, italic: bool) -> Option<FontInstanceId> {
        self.fonts.primary(family, FontAttrs { weight, italic })
    }

    /// Sets what an empty family means (default `system-ui`).
    pub fn set_default_family(&mut self, family: &str) {
        self.fonts.set_default_family(family);
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
    ///
    /// `decorations` holds each span's `decoration` flags (missing: none):
    /// underline and line-through rects over the span's placed glyphs on
    /// each segment, at the run font's decoration metrics, in the span's
    /// paint slot.
    #[allow(clippy::too_many_arguments)]
    pub fn emit_paragraph(
        &mut self,
        p: &Paragraph,
        origin: Point,
        scale: f32,
        brush: Option<PaintSlot>,
        decorations: &[u8],
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
            // Glyphs past a line limit's cut are not on any line.
            let glyphs = p.glyphs[run.glyphs.start as usize..run.glyphs.end as usize]
                .iter()
                .filter(|g| g.id != HIDDEN && g.cluster < p.visible_end)
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
        if let Some(e) = p.ellipsis.as_ref().filter(|e| e.line.is_some()) {
            // In span zero's paint.
            let mut pen = e.x;
            let glyphs = e.glyphs.iter().map(|&(id, advance)| {
                let x = pen;
                pen += advance;
                (
                    id,
                    (origin.x + x) * scale,
                    (origin.y + e.y) * scale,
                    brush.unwrap_or(PaintSlot(0)),
                )
            });
            emit_glyphs(
                raster,
                cache,
                &fonts.store,
                e.font,
                e.size * scale,
                scale,
                glyphs,
                atlas,
                out,
                &mut stats,
            );
        }
        if decorations.iter().any(|&d| d != 0) {
            emit_decorations(p, origin, brush, decorations, out);
        }
        stats
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
