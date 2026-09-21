//! Glyph atlas: etagere-packed pages mirrored on the CPU, uploaded to GPU
//! texture arrays through dirty-rect writes.
//!
//! Two page sets: an R8 alpha atlas for monochrome glyphs and an RGBA atlas
//! for color bitmap glyphs (emoji). Pages grow on demand up to a cap; at
//! the cap the caller evicts least-recently-used cache entries and retries.

use etagere::{AllocId, AtlasAllocator, size2};

use crate::geom::RectPx;

pub const ATLAS_PAGE_SIZE: u32 = 2048;
/// Glyph bitmaps get a 1px gutter so linear filtering never bleeds.
const GUTTER: u32 = 1;
/// Page-count caps: 4 alpha pages = 16 MB, 2 color pages = 32 MB.
const MAX_ALPHA_PAGES: usize = 4;
const MAX_COLOR_PAGES: usize = 2;

#[derive(Clone, Copy, Debug, Default)]
pub struct AtlasStats {
    pub alpha_pages: u32,
    pub color_pages: u32,
    pub allocations: u64,
    /// Slots returned to an allocator for reuse by other glyphs.
    pub evictions: u64,
}

struct Page {
    alloc: AtlasAllocator,
    /// CPU mirror of the page contents (bytes_per_pixel * size^2).
    data: Vec<u8>,
    dirty: Option<RectPx>,
    /// Union of everything ever blitted — the region the GPU must have on
    /// a fresh texture (grow / first sync). Never cleared.
    used: Option<RectPx>,
}

impl Page {
    fn new(page_size: u32, bpp: u32) -> Page {
        Page {
            alloc: AtlasAllocator::new(size2(page_size as i32, page_size as i32)),
            data: vec![0; (page_size * page_size * bpp) as usize],
            dirty: None,
            used: None,
        }
    }
}

pub struct GlyphAtlas {
    page_size: u32,
    max_alpha: usize,
    max_color: usize,
    alpha: Vec<Page>,
    color: Vec<Page>,
    pub stats: AtlasStats,
}

/// Where an allocation landed.
#[derive(Clone, Copy, Debug)]
pub struct AtlasSlot {
    /// etagere allocation id — needed to `free` the slot on eviction.
    pub alloc: AllocId,
    pub page: u16,
    pub x: u16,
    pub y: u16,
}

impl GlyphAtlas {
    pub fn new() -> GlyphAtlas {
        GlyphAtlas {
            page_size: ATLAS_PAGE_SIZE,
            max_alpha: MAX_ALPHA_PAGES,
            max_color: MAX_COLOR_PAGES,
            alpha: Vec::new(),
            color: Vec::new(),
            stats: AtlasStats::default(),
        }
    }

    /// Small-atlas constructor for eviction tests.
    #[cfg(test)]
    pub fn for_test(page_size: u32, max_alpha: usize, max_color: usize) -> GlyphAtlas {
        GlyphAtlas {
            page_size,
            max_alpha,
            max_color,
            alpha: Vec::new(),
            color: Vec::new(),
            stats: AtlasStats::default(),
        }
    }

    /// Allocates a w×h rect in the alpha atlas and writes the 1-byte mask.
    /// `None` means every alpha page is at the cap and full — evict.
    pub fn write_alpha(&mut self, w: u32, h: u32, data: &[u8]) -> Option<AtlasSlot> {
        let slot = alloc(
            &mut self.alpha,
            self.page_size,
            1,
            self.max_alpha,
            w,
            h,
            &mut self.stats,
        )?;
        blit(
            &mut self.alpha[slot.page as usize],
            self.page_size,
            1,
            slot,
            w,
            h,
            data,
        );
        Some(slot)
    }

    /// Allocates a w×h rect in the color atlas and writes the 4-byte RGBA.
    /// `None` means every color page is at the cap and full — evict.
    pub fn write_color(&mut self, w: u32, h: u32, data: &[u8]) -> Option<AtlasSlot> {
        let slot = alloc(
            &mut self.color,
            self.page_size,
            4,
            self.max_color,
            w,
            h,
            &mut self.stats,
        )?;
        blit(
            &mut self.color[slot.page as usize],
            self.page_size,
            4,
            slot,
            w,
            h,
            data,
        );
        Some(slot)
    }

    pub fn alpha_pages(&self) -> usize {
        self.alpha.len()
    }

    pub fn color_pages(&self) -> usize {
        self.color.len()
    }

    /// CPU mirror + dirty rect of a page, for GPU upload.
    /// `color` selects the page set.
    pub fn page_bytes(&self, color: bool, page: usize) -> (&[u8], Option<RectPx>) {
        let pages = if color { &self.color } else { &self.alpha };
        let p = &pages[page];
        (&p.data, p.dirty)
    }

    /// Union of all allocated content on a page — what a fresh GPU texture
    /// needs to receive even when the incremental dirty rect is clean.
    pub fn page_used(&self, color: bool, page: usize) -> Option<RectPx> {
        let pages = if color { &self.color } else { &self.alpha };
        pages[page].used
    }

    pub fn clear_dirty(&mut self) {
        for p in self.alpha.iter_mut().chain(self.color.iter_mut()) {
            p.dirty = None;
        }
    }

    /// Returns a slot's rectangle to its allocator. The GPU copy of the
    /// pixels stays — dead space until another glyph is blitted over it.
    pub fn free(&mut self, color: bool, slot: AtlasSlot) {
        let pages = if color { &mut self.color } else { &mut self.alpha };
        pages[slot.page as usize].alloc.deallocate(slot.alloc);
        self.stats.evictions += 1;
    }
}

impl Default for GlyphAtlas {
    fn default() -> GlyphAtlas {
        GlyphAtlas::new()
    }
}

fn alloc(
    pages: &mut Vec<Page>,
    page_size: u32,
    bpp: u32,
    max_pages: usize,
    w: u32,
    h: u32,
    stats: &mut AtlasStats,
) -> Option<AtlasSlot> {
    let size = size2((w + GUTTER * 2) as i32, (h + GUTTER * 2) as i32);
    for (i, page) in pages.iter_mut().enumerate() {
        if let Some(a) = page.alloc.allocate(size) {
            let r = a.rectangle;
            stats.allocations += 1;
            return Some(AtlasSlot {
                alloc: a.id,
                page: i as u16,
                x: (r.min.x + GUTTER as i32) as u16,
                y: (r.min.y + GUTTER as i32) as u16,
            });
        }
    }
    // No room anywhere: grow a page and retry once — unless the set is
    // at its cap, in which case the caller must evict.
    if pages.len() >= max_pages {
        return None;
    }
    pages.push(Page::new(page_size, bpp));
    if bpp == 1 {
        stats.alpha_pages += 1;
    } else {
        stats.color_pages += 1;
    }
    let i = pages.len() - 1;
    let a = pages[i].alloc.allocate(size)?;
    stats.allocations += 1;
    let r = a.rectangle;
    Some(AtlasSlot {
        alloc: a.id,
        page: i as u16,
        x: (r.min.x + GUTTER as i32) as u16,
        y: (r.min.y + GUTTER as i32) as u16,
    })
}

fn blit(page: &mut Page, page_size: u32, bpp: u32, slot: AtlasSlot, w: u32, h: u32, src: &[u8]) {
    let stride = (page_size * bpp) as usize;
    let row_bytes = (w * bpp) as usize;
    let x = slot.x as usize;
    let y = slot.y as usize;
    for row in 0..h as usize {
        let dst = (y + row) * stride + x * bpp as usize;
        let src_off = row * row_bytes;
        page.data[dst..dst + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    let rect = RectPx::new(x as u32, y as u32, x as u32 + w, y as u32 + h);
    for slot in [&mut page.dirty, &mut page.used] {
        match slot {
            Some(d) => d.union(rect),
            None => *slot = Some(rect),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::cache::{CachedGlyph, GlyphCache, GlyphKey};

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
