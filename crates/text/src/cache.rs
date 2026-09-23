//! Glyph raster cache.
//!
//! A cache entry is NOT keyed by glyph id alone. The key covers every input
//! that changes the raster output: font identity (interned), glyph id,
//! effective pixel size, variation coordinates + synthetic styling
//! (interned), and the quantized subpixel offset.
//!
//! Each key maps to a stable `RasterId` in the scene's raster atlas. The
//! atlas owns residency; the cache keeps what it needs to rasterize an
//! evicted glyph again (the key, the font, the coordinates), so a chunk
//! that references the id never changes when its raster moves.

use std::collections::HashMap;

use craie_scene::RasterId;
use parley::FontData;

/// Quarter-pixel subpixel quantization. Each axis uses 2 bits.
pub const SUBPIXEL_BITS: u32 = 2;
pub const SUBPIXEL_STEPS: u32 = 1 << SUBPIXEL_BITS;

/// Compact key into the glyph cache. 12 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// Interned (font blob id, face index).
    pub font: u16,
    /// Interned (normalized coords, synthesis) pair.
    pub coords: u16,
    pub glyph: u16,
    /// Exact f32 bits of the effective physical-pixel size.
    pub size_bits: u32,
    /// Quantized subpixel offset: x in bits 0..2, y in bits 2..4.
    pub subpixel: u8,
    pub _pad: u8,
}

/// A cached glyph: its raster (none for zero-area glyphs such as
/// spaces) and its bearing in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct CachedGlyph {
    pub raster: Option<RasterId>,
    pub w: u16,
    pub h: u16,
    /// Swash placement: bitmap offset relative to the glyph origin.
    pub left: i16,
    pub top: i16,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    /// Swash rasterizations performed (first rasters and re-rasters).
    pub rasters: u64,
    /// Re-rasterizations of evicted glyphs.
    pub rerasters: u64,
}

pub(crate) struct CoordsRow {
    pub coords: Box<[i16]>,
    pub embolden: bool,
    pub skew: i16,
}

pub struct GlyphCache {
    /// (blob id, face index) -> compact font slot.
    fonts: HashMap<(u64, u32), u16>,
    /// Font data per slot, kept to re-rasterize evicted glyphs.
    pub(crate) font_data: Vec<FontData>,
    /// Interned (normalized coords, embolden, skew) rows. The table is
    /// tiny — a linear scan beats a per-lookup `Box<[i16]>` key alloc.
    pub(crate) coords: Vec<CoordsRow>,
    map: HashMap<GlyphKey, CachedGlyph>,
    /// Key per raster id, for re-rasterization.
    pub(crate) keys: Vec<Option<GlyphKey>>,
    pub stats: CacheStats,
}

impl GlyphCache {
    pub fn new() -> GlyphCache {
        GlyphCache {
            fonts: HashMap::new(),
            font_data: Vec::new(),
            coords: Vec::new(),
            map: HashMap::new(),
            keys: Vec::new(),
            stats: CacheStats::default(),
        }
    }

    /// Interns a font identity to a u16 slot. One map lookup per run.
    pub fn font_slot(&mut self, font: &FontData) -> u16 {
        let next = self.fonts.len() as u16;
        let slot = *self
            .fonts
            .entry((font.data.id(), font.index))
            .or_insert(next);
        if slot as usize == self.font_data.len() {
            self.font_data.push(font.clone());
        }
        slot
    }

    /// Interns variation coords + synthesis to a u16 slot.
    /// `skew` is degrees quantized to i16 * 64.
    pub fn coords_slot(&mut self, coords: &[i16], embolden: bool, skew: i16) -> u16 {
        if let Some(i) = self
            .coords
            .iter()
            .position(|r| r.embolden == embolden && r.skew == skew && r.coords.as_ref() == coords)
        {
            return i as u16;
        }
        let slot = self.coords.len() as u16;
        self.coords.push(CoordsRow {
            coords: coords.into(),
            embolden,
            skew,
        });
        slot
    }

    pub fn get(&mut self, key: &GlyphKey) -> Option<CachedGlyph> {
        match self.map.get(key) {
            Some(g) => {
                self.stats.hits += 1;
                Some(*g)
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    pub fn insert(&mut self, key: GlyphKey, glyph: CachedGlyph) {
        if let Some(id) = glyph.raster {
            let i = id.0 as usize;
            if self.keys.len() <= i {
                self.keys.resize(i + 1, None);
            }
            self.keys[i] = Some(key);
        }
        self.map.insert(key, glyph);
    }

    /// The key a raster id was produced from.
    pub fn key_of(&self, id: RasterId) -> Option<GlyphKey> {
        self.keys.get(id.0 as usize).copied().flatten()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Default for GlyphCache {
    fn default() -> GlyphCache {
        GlyphCache::new()
    }
}

/// Splits a physical-pixel coordinate into (integer part, quarter-pixel
/// bucket). Rounds to the nearest quarter pixel rather than truncating so
/// the rasterizer is asked for the offset that will actually be drawn.
///
/// Bucket 4 (rounding 0.9 -> 1.0) folds into the next integer.
pub fn quantize_subpixel(v: f32) -> (i32, u8) {
    let q = (v * SUBPIXEL_STEPS as f32).round() as i32;
    (q >> SUBPIXEL_BITS, (q & (SUBPIXEL_STEPS as i32 - 1)) as u8)
}

/// Inverse of the bucket: the fractional offset passed to Swash, in pixels.
pub fn subpixel_offset(bucket: u8) -> f32 {
    bucket as f32 / SUBPIXEL_STEPS as f32
}
