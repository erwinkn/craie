//! Glyph raster cache.
//!
//! A cache entry is NOT keyed by glyph id alone. The key covers every input
//! that changes the raster output: font identity (interned), glyph id,
//! effective pixel size, variation coordinates + synthetic styling
//! (interned), and the quantized subpixel offset.

use std::collections::HashMap;

/// Quarter-pixel subpixel quantization. Each axis uses 2 bits.
pub const SUBPIXEL_BITS: u32 = 2;
pub const SUBPIXEL_STEPS: u32 = 1 << SUBPIXEL_BITS;

/// Compact key into the glyph cache. 16 bytes.
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

/// Where a rasterized glyph lives in the atlas, plus how to place it.
#[derive(Clone, Copy, Debug)]
pub struct CachedGlyph {
    /// Atlas page index (within the alpha or color page set).
    pub page: u16,
    /// True when the glyph is a 32-bit color bitmap (emoji etc.).
    pub color: bool,
    pub x: u16,
    pub y: u16,
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
    /// Number of actual Swash rasterizations performed.
    pub rasters: u64,
}

pub struct GlyphCache {
    /// (blob id, face index) -> compact font slot.
    fonts: HashMap<(u64, u32), u16>,
    /// (normalized coords, embolden, skew) -> compact coords slot.
    coords: HashMap<(Box<[i16]>, bool, i16), u16>,
    map: HashMap<GlyphKey, CachedGlyph>,
    pub stats: CacheStats,
}

impl GlyphCache {
    pub fn new() -> GlyphCache {
        GlyphCache {
            fonts: HashMap::new(),
            coords: HashMap::new(),
            map: HashMap::new(),
            stats: CacheStats::default(),
        }
    }

    /// Interns a font identity to a u16 slot. One map lookup per glyph run.
    pub fn font_slot(&mut self, blob_id: u64, face_index: u32) -> u16 {
        let next = self.fonts.len() as u16;
        *self.fonts.entry((blob_id, face_index)).or_insert(next)
    }

    /// Interns variation coords + synthesis to a u16 slot.
    /// `synthesis` is (embolden, skew_degrees quantized to i16*64).
    pub fn coords_slot(&mut self, coords: &[i16], embolden: bool, skew: i16) -> u16 {
        let next = self.coords.len() as u16;
        *self
            .coords
            .entry((coords.into(), embolden, skew))
            .or_insert(next)
    }

    pub fn get(&mut self, key: &GlyphKey) -> Option<CachedGlyph> {
        match self.map.get(key) {
            Some(entry) => {
                self.stats.hits += 1;
                Some(*entry)
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    pub fn insert(&mut self, key: GlyphKey, entry: CachedGlyph) {
        self.map.insert(key, entry);
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
