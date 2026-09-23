//! Glyph raster cache.
//!
//! A cache entry is NOT keyed by glyph id alone. The key covers every input
//! that changes the raster output: font identity (interned), glyph id,
//! effective pixel size, variation coordinates + synthetic styling
//! (interned), and the quantized subpixel offset.

use std::collections::HashMap;

use etagere::AllocId;

use craie_scene::{AtlasSlot, GlyphAtlas};

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
    /// etagere allocation id — `None` for zero-area glyphs (spaces).
    pub alloc: Option<AllocId>,
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

/// A cached glyph plus the frame it was last emitted on — the LRU
/// signal the atlas eviction pass uses.
#[derive(Clone, Copy, Debug)]
struct Entry {
    glyph: CachedGlyph,
    last_used: u32,
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
    /// Interned (normalized coords, embolden, skew) rows. The table is
    /// tiny — a linear scan beats a per-lookup `Box<[i16]>` key alloc.
    coords: Vec<CoordsRow>,
    map: HashMap<GlyphKey, Entry>,
    /// Frame counter for LRU stamping — bumped by `begin_frame`.
    tick: u32,
    pub stats: CacheStats,
}

struct CoordsRow {
    coords: Box<[i16]>,
    embolden: bool,
    skew: i16,
}

impl GlyphCache {
    pub fn new() -> GlyphCache {
        GlyphCache {
            fonts: HashMap::new(),
            coords: Vec::new(),
            map: HashMap::new(),
            tick: 0,
            stats: CacheStats::default(),
        }
    }

    /// Advances the LRU clock. Called once per paint pass.
    pub fn begin_frame(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    /// Interns a font identity to a u16 slot. One map lookup per glyph run.
    pub fn font_slot(&mut self, blob_id: u64, face_index: u32) -> u16 {
        let next = self.fonts.len() as u16;
        *self.fonts.entry((blob_id, face_index)).or_insert(next)
    }

    /// Interns variation coords + synthesis to a u16 slot.
    /// `synthesis` is (embolden, skew_degrees quantized to i16*64).
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
        match self.map.get_mut(key) {
            Some(entry) => {
                self.stats.hits += 1;
                entry.last_used = self.tick;
                Some(entry.glyph)
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    pub fn insert(&mut self, key: GlyphKey, entry: CachedGlyph) {
        self.map.insert(
            key,
            Entry {
                glyph: entry,
                last_used: self.tick,
            },
        );
    }

    /// Evicts the least-recently-used entry in `color`'s page set and
    /// frees its atlas slot. Entries used on the current frame are
    /// ineligible. Returns false when nothing evictable remains.
    pub fn evict_oldest(
        &mut self,
        atlas: &mut GlyphAtlas,
        color: bool,
    ) -> bool {
        let victim = self
            .map
            .iter()
            .filter(|(_, e)| e.glyph.color == color && e.last_used < self.tick)
            .min_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| *k);
        let Some(key) = victim else { return false };
        let entry = self.map.remove(&key).unwrap();
        if let Some(alloc) = entry.glyph.alloc {
            atlas.free(
                color,
                AtlasSlot {
                    alloc,
                    page: entry.glyph.page,
                    x: entry.glyph.x,
                    y: entry.glyph.y,
                },
            );
        }
        true
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

#[cfg(test)]
mod atlas_tests {
    use super::*;

    fn key(g: u16) -> GlyphKey {
        GlyphKey {
            font: 0,
            coords: 0,
            glyph: g,
            size_bits: 0,
            subpixel: 0,
            _pad: 0,
        }
    }

    /// A capped, full page set returns None; evicting the oldest cache
    /// entry frees a slot and the next write lands.
    #[test]
    fn eviction_frees_atlas_space() {
        let mut atlas = GlyphAtlas::for_test(64, 1, 1);
        let mut cache = GlyphCache::new();
        let px = [7u8; 16 * 16];

        // Fill the single 64x64 alpha page (16x16 glyphs + 2px gutter).
        let mut n = 0u16;
        while let Some(slot) = atlas.write_alpha(16, 16, &px) {
            cache.insert(
                key(n),
                CachedGlyph {
                    alloc: Some(slot.alloc),
                    page: slot.page,
                    color: false,
                    x: slot.x,
                    y: slot.y,
                    w: 16,
                    h: 16,
                    left: 0,
                    top: 0,
                },
            );
            n += 1;
            cache.begin_frame();
        }
        assert!(n > 0, "test atlas must hold at least one glyph");
        assert_eq!(atlas.alpha_pages(), 1);

        // Full and capped: the next write must fail.
        assert!(atlas.write_alpha(16, 16, &px).is_none());

        // Evict the oldest (everything was stamped before this frame):
        // the freed slot accepts the write again.
        assert!(cache.evict_oldest(&mut atlas, false));
        assert!(atlas.write_alpha(16, 16, &px).is_some());
        assert_eq!(atlas.stats.evictions, 1);
    }

    /// Entries stamped on the current frame are never evicted — a glyph
    /// can't be freed while this pass is still drawing it.
    #[test]
    fn current_frame_entries_are_safe() {
        let mut atlas = GlyphAtlas::for_test(64, 1, 1);
        let mut cache = GlyphCache::new();
        let slot = atlas.write_alpha(16, 16, &[1u8; 256]).unwrap();
        cache.insert(
            key(0),
            CachedGlyph {
                alloc: Some(slot.alloc),
                page: slot.page,
                color: false,
                x: slot.x,
                y: slot.y,
                w: 16,
                h: 16,
                left: 0,
                top: 0,
            },
        );
        // No begin_frame: the entry sits on the current tick.
        assert!(!cache.evict_oldest(&mut atlas, false));
        assert_eq!(atlas.stats.evictions, 0);
    }
}
