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

use crate::fonts::{FontInstanceId, FontStore};

/// Quarter-pixel subpixel quantization. Each axis uses 2 bits.
pub const SUBPIXEL_BITS: u32 = 2;
pub const SUBPIXEL_STEPS: u32 = 1 << SUBPIXEL_BITS;

/// Compact key into the glyph cache. 12 bytes. The font instance covers
/// the face, variation coordinates, and synthesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub font: FontInstanceId,
    pub glyph: u16,
    /// Exact f32 bits of the effective physical-pixel size.
    pub size_bits: u32,
    /// Quantized subpixel offset: x in bits 0..2, y in bits 2..4.
    pub subpixel: u8,
}

/// A cached glyph: its raster (none for zero-area glyphs such as
/// spaces) and its bitmap geometry in physical pixels, relative to the
/// glyph origin. No position: placements live in the paragraph.
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

pub struct GlyphCache {
    map: HashMap<GlyphKey, CachedGlyph>,
    /// Key per raster id, for re-rasterization.
    pub(crate) keys: Vec<Option<GlyphKey>>,
    /// Pixel size each raster was made at, by raster id.
    raster_sizes: Vec<f32>,
    pub stats: CacheStats,
}

impl GlyphCache {
    pub fn new() -> GlyphCache {
        GlyphCache {
            map: HashMap::new(),
            keys: Vec::new(),
            raster_sizes: Vec::new(),
            stats: CacheStats::default(),
        }
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

    /// Records the pixel size raster `id` was made at.
    pub fn set_raster_size(&mut self, id: RasterId, size: f32) {
        let i = id.0 as usize;
        if self.raster_sizes.len() <= i {
            self.raster_sizes.resize(i + 1, f32::NAN);
        }
        self.raster_sizes[i] = size;
    }

    /// The pixel size raster `id` was made at.
    pub fn raster_size(&self, id: RasterId) -> Option<f32> {
        self.raster_sizes
            .get(id.0 as usize)
            .copied()
            .filter(|s| !s.is_nan())
    }

    /// The key a raster id was produced from.
    pub fn key_of(&self, id: RasterId) -> Option<GlyphKey> {
        self.keys.get(id.0 as usize).copied().flatten()
    }

    /// A raster's identity independent of interning order: font file
    /// content and face, coordinates, synthesis, glyph, size, subpixel.
    /// Two engines that rasterized the same glyph agree on it (test
    /// oracle).
    pub fn stable_key(&self, id: RasterId, store: &FontStore) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let k = self.key_of(id)?;
        let inst = store.instance_data(k.font);
        let face = store.face_data(inst.face);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Blob ids are per source; identify the font by content.
        let bytes = face.bytes.as_ref().as_ref();
        let edge = bytes.len().min(4096);
        (bytes.len(), face.index).hash(&mut h);
        bytes[..edge].hash(&mut h);
        bytes[bytes.len() - edge..].hash(&mut h);
        (
            &*inst.coords,
            inst.synthesis.embolden,
            inst.synthesis.skew.to_bits(),
        )
            .hash(&mut h);
        (k.glyph, k.size_bits, k.subpixel).hash(&mut h);
        Some(h.finish())
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
