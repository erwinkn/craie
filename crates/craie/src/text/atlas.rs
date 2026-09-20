//! Glyph atlas: etagere-packed pages mirrored on the CPU, uploaded to GPU
//! texture arrays through dirty-rect writes.
//!
//! Two page sets: an R8 alpha atlas for monochrome glyphs and an RGBA atlas
//! for color bitmap glyphs (emoji). No eviction yet — pages grow on demand.

use etagere::{AtlasAllocator, size2};

use crate::geom::RectPx;

pub const ATLAS_PAGE_SIZE: u32 = 2048;
/// Glyph bitmaps get a 1px gutter so linear filtering never bleeds.
const GUTTER: u32 = 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct AtlasStats {
    pub alpha_pages: u32,
    pub color_pages: u32,
    pub allocations: u64,
}

struct Page {
    alloc: AtlasAllocator,
    /// CPU mirror of the page contents (bytes_per_pixel * size^2).
    data: Vec<u8>,
    dirty: Option<RectPx>,
}

impl Page {
    fn new(page_size: u32, bpp: u32) -> Page {
        Page {
            alloc: AtlasAllocator::new(size2(page_size as i32, page_size as i32)),
            data: vec![0; (page_size * page_size * bpp) as usize],
            dirty: None,
        }
    }
}

pub struct GlyphAtlas {
    page_size: u32,
    alpha: Vec<Page>,
    color: Vec<Page>,
    pub stats: AtlasStats,
}

/// Where an allocation landed.
#[derive(Clone, Copy, Debug)]
pub struct AtlasSlot {
    pub page: u16,
    pub x: u16,
    pub y: u16,
}

impl GlyphAtlas {
    pub fn new() -> GlyphAtlas {
        GlyphAtlas {
            page_size: ATLAS_PAGE_SIZE,
            alpha: Vec::new(),
            color: Vec::new(),
            stats: AtlasStats::default(),
        }
    }

    /// Allocates a w×h rect in the alpha atlas and writes the 1-byte mask.
    pub fn write_alpha(&mut self, w: u32, h: u32, data: &[u8]) -> Option<AtlasSlot> {
        let slot = alloc(&mut self.alpha, self.page_size, 1, w, h, &mut self.stats)?;
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
    pub fn write_color(&mut self, w: u32, h: u32, data: &[u8]) -> Option<AtlasSlot> {
        let slot = alloc(&mut self.color, self.page_size, 4, w, h, &mut self.stats)?;
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

    pub fn clear_dirty(&mut self) {
        for p in self.alpha.iter_mut().chain(self.color.iter_mut()) {
            p.dirty = None;
        }
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
                page: i as u16,
                x: (r.min.x + GUTTER as i32) as u16,
                y: (r.min.y + GUTTER as i32) as u16,
            });
        }
    }
    // No room anywhere: grow a page and retry once.
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
    match &mut page.dirty {
        Some(d) => d.union(rect),
        None => page.dirty = Some(rect),
    }
}
