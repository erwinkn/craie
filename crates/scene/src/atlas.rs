//! Raster atlas: stable `RasterId`s with separate residency.
//!
//! A raster (glyph bitmap today, icons later) gets a `RasterId` once and
//! keeps it. Residency — which page, which rect — is a separate table
//! the GPU reads, so drawing records never bake atlas coordinates and a
//! relocation touches no chunk.
//!
//! Pages are etagere-packed 2048² textures mirrored on the CPU, uploaded
//! through dirty rects. Two page sets: R8 alpha and RGBA color. A page
//! cap bounds memory; allocation failure evicts the least-recently-used
//! raster that is not pinned. A raster is pinned while its `last_used`
//! epoch equals the current epoch: rasters are stamped as chunks use
//! them, and the frame driver stamps every raster of every visible chunk
//! (re-rasterizing the missing ones) before drawing, so a visible glyph
//! never disappears mid-frame. When the pinned set alone exceeds the
//! cap, pages grow past it and `stats.over_budget_pages` records it.

use craie_core::RectPx;
use craie_core::dirty::DirtyRanges;
use etagere::{AllocId, AtlasAllocator, size2};

pub const ATLAS_PAGE_SIZE: u32 = 2048;
/// Bitmaps get a 1px gutter so linear filtering never bleeds.
const GUTTER: u32 = 1;
/// Page caps: 4 alpha pages = 16 MiB, 2 color pages = 32 MiB.
const MAX_ALPHA_PAGES: usize = 4;
const MAX_COLOR_PAGES: usize = 2;

/// Stable identity of a raster. Dense; indexes the residency table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct RasterId(pub u32);

#[derive(Clone, Copy, Debug, Default)]
pub struct AtlasStats {
    pub alpha_pages: u32,
    pub color_pages: u32,
    pub allocations: u64,
    pub evictions: u64,
    /// Pages allocated past the cap because the pinned set did not fit.
    pub over_budget_pages: u32,
    /// Rasters refused because they exceed a page. The text engine
    /// rasterizes such glyphs smaller instead (`downscaled`), so this
    /// stays zero for text.
    pub oversized: u64,
    /// Glyphs rasterized below their drawn size to fit a page.
    pub downscaled: u64,
}

/// Where a raster lives, plus its pixel size. Size is known even while
/// the raster is not resident (it is a property of the bitmap).
#[derive(Clone, Copy, Debug, Default)]
pub struct Residency {
    /// Bitmap size in atlas pixels.
    pub w: u16,
    pub h: u16,
    /// Drawn size in device pixels. Equals the bitmap size, except for a
    /// raster made smaller to fit a page, which draws scaled up.
    pub quad_w: u16,
    pub quad_h: u16,
    pub color: bool,
    pub resident: bool,
    pub page: u16,
    pub x: u16,
    pub y: u16,
    alloc: Option<AllocId>,
    pub last_used: u32,
}

/// Residency row as the GPU reads it: 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RasterGpu {
    /// x | y << 16, atlas pixels.
    pub xy: u32,
    /// Bitmap w | h << 16, atlas pixels.
    pub wh: u32,
    /// page | color << 16.
    pub page: u32,
    /// Drawn w | h << 16, device pixels.
    pub quad: u32,
}

struct Page {
    alloc: AtlasAllocator,
    data: Vec<u8>,
    dirty: Option<RectPx>,
    /// Union of everything ever blitted: what a fresh GPU texture needs.
    used: Option<RectPx>,
}

impl Page {
    fn new(size: u32, bpp: u32) -> Page {
        Page {
            alloc: AtlasAllocator::new(size2(size as i32, size as i32)),
            data: vec![0; (size * size * bpp) as usize],
            dirty: None,
            used: None,
        }
    }
}

pub struct RasterAtlas {
    page_size: u32,
    max_alpha: usize,
    max_color: usize,
    alpha: Vec<Page>,
    color: Vec<Page>,
    entries: Vec<Residency>,
    gpu: Vec<RasterGpu>,
    gpu_dirty: DirtyRanges,
    epoch: u32,
    pub stats: AtlasStats,
}

impl Default for RasterAtlas {
    fn default() -> RasterAtlas {
        RasterAtlas::new()
    }
}

impl RasterAtlas {
    pub fn new() -> RasterAtlas {
        RasterAtlas::with_budget(ATLAS_PAGE_SIZE, MAX_ALPHA_PAGES, MAX_COLOR_PAGES)
    }

    /// Custom page size and caps (tests, pressure benchmarks).
    pub fn with_budget(page_size: u32, max_alpha: usize, max_color: usize) -> RasterAtlas {
        RasterAtlas {
            page_size,
            max_alpha,
            max_color,
            alpha: Vec::new(),
            color: Vec::new(),
            entries: Vec::new(),
            gpu: Vec::new(),
            gpu_dirty: DirtyRanges::default(),
            epoch: 1,
            stats: AtlasStats::default(),
        }
    }

    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// Starts a new residency epoch. Everything stamped before is
    /// evictable until stamped again.
    pub fn begin_epoch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Whether a `w`×`h` raster fits a page with its gutter.
    pub fn fits(&self, w: u32, h: u32) -> bool {
        w + GUTTER * 2 <= self.page_size && h + GUTTER * 2 <= self.page_size
    }

    /// Registers a raster of `w`×`h` pixels. Not resident until `insert`.
    pub fn new_id(&mut self, w: u16, h: u16, color: bool) -> RasterId {
        self.new_scaled_id(w, h, w, h, color)
    }

    /// Registers a `w`×`h` bitmap drawn at `quad_w`×`quad_h` device px:
    /// a raster made smaller than its drawn size to fit a page.
    pub fn new_scaled_id(
        &mut self,
        w: u16,
        h: u16,
        quad_w: u16,
        quad_h: u16,
        color: bool,
    ) -> RasterId {
        let id = self.entries.len() as u32;
        self.entries.push(Residency {
            w,
            h,
            quad_w,
            quad_h,
            color,
            ..Residency::default()
        });
        self.gpu.push(RasterGpu {
            xy: 0,
            wh: w as u32 | (h as u32) << 16,
            page: (color as u32) << 16,
            quad: quad_w as u32 | (quad_h as u32) << 16,
        });
        self.gpu_dirty.add(id as usize..id as usize + 1);
        RasterId(id)
    }

    pub fn entry(&self, id: RasterId) -> &Residency {
        &self.entries[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Pins `id` for this epoch. Returns whether it is resident; a
    /// zero-area raster counts as resident.
    pub fn touch(&mut self, id: RasterId) -> bool {
        let e = &mut self.entries[id.0 as usize];
        e.last_used = self.epoch;
        e.resident || e.w == 0 || e.h == 0
    }

    /// Makes `id` resident with `data` (w·h·bpp bytes). Evicts unpinned
    /// rasters of the same set when the cap is reached; grows past the
    /// cap when everything left is pinned. Pins `id`.
    pub fn insert(&mut self, id: RasterId, data: &[u8]) {
        let (w, h, color) = {
            let e = &self.entries[id.0 as usize];
            (e.w as u32, e.h as u32, e.color)
        };
        self.entries[id.0 as usize].last_used = self.epoch;
        if w == 0 || h == 0 || self.entries[id.0 as usize].resident {
            return;
        }
        if !self.fits(w, h) {
            // No page can hold it: report, never panic after a
            // transaction was accepted. The text engine makes such
            // glyphs smaller first (`new_scaled_id`), so this is a guard.
            self.stats.oversized += 1;
            return;
        }
        let bpp = if color { 4 } else { 1 };
        let slot = loop {
            if let Some(slot) = self.alloc(color, w, h, false) {
                break slot;
            }
            if !self.evict_one(color) {
                self.stats.over_budget_pages += 1;
                // `fits` holds, so a fresh page takes it.
                break self
                    .alloc(color, w, h, true)
                    .expect("a raster that fits a page fits a fresh page");
            }
        };
        let page_size = self.page_size;
        let pages = if color {
            &mut self.color
        } else {
            &mut self.alpha
        };
        blit(
            &mut pages[slot.1 as usize],
            page_size,
            bpp,
            slot.2,
            slot.3,
            w,
            h,
            data,
        );
        let e = &mut self.entries[id.0 as usize];
        e.alloc = Some(slot.0);
        e.page = slot.1;
        e.x = slot.2;
        e.y = slot.3;
        e.resident = true;
        let quad = self.gpu[id.0 as usize].quad;
        self.gpu[id.0 as usize] = RasterGpu {
            xy: slot.2 as u32 | (slot.3 as u32) << 16,
            wh: w | h << 16,
            page: slot.1 as u32 | (color as u32) << 16,
            quad,
        };
        self.gpu_dirty.add(id.0 as usize..id.0 as usize + 1);
    }

    /// Evicts the least-recently-used unpinned resident raster of the
    /// given set. False when every resident raster is pinned.
    fn evict_one(&mut self, color: bool) -> bool {
        let epoch = self.epoch;
        let victim = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.resident && e.color == color && e.alloc.is_some() && e.last_used != epoch
            })
            .min_by_key(|(_, e)| e.last_used.wrapping_sub(epoch))
            .map(|(i, _)| i);
        let Some(i) = victim else { return false };
        let e = &mut self.entries[i];
        let pages = if color {
            &mut self.color
        } else {
            &mut self.alpha
        };
        pages[e.page as usize]
            .alloc
            .deallocate(e.alloc.take().unwrap());
        e.resident = false;
        self.stats.evictions += 1;
        true
    }

    /// Returns (alloc, page, x, y). `force` grows past the page cap.
    fn alloc(
        &mut self,
        color: bool,
        w: u32,
        h: u32,
        force: bool,
    ) -> Option<(AllocId, u16, u16, u16)> {
        let (pages, max, bpp) = if color {
            (&mut self.color, self.max_color, 4)
        } else {
            (&mut self.alpha, self.max_alpha, 1)
        };
        let size = size2((w + GUTTER * 2) as i32, (h + GUTTER * 2) as i32);
        for (i, page) in pages.iter_mut().enumerate() {
            if let Some(a) = page.alloc.allocate(size) {
                self.stats.allocations += 1;
                let r = a.rectangle;
                return Some((
                    a.id,
                    i as u16,
                    (r.min.x + GUTTER as i32) as u16,
                    (r.min.y + GUTTER as i32) as u16,
                ));
            }
        }
        if pages.len() >= max && !force {
            return None;
        }
        pages.push(Page::new(self.page_size, bpp));
        if color {
            self.stats.color_pages += 1;
        } else {
            self.stats.alpha_pages += 1;
        }
        let i = pages.len() - 1;
        let a = pages[i].alloc.allocate(size)?;
        self.stats.allocations += 1;
        let r = a.rectangle;
        Some((
            a.id,
            i as u16,
            (r.min.x + GUTTER as i32) as u16,
            (r.min.y + GUTTER as i32) as u16,
        ))
    }

    pub fn alpha_pages(&self) -> usize {
        self.alpha.len()
    }

    pub fn color_pages(&self) -> usize {
        self.color.len()
    }

    /// CPU mirror and pending dirty rect of a page.
    pub fn page_bytes(&self, color: bool, page: usize) -> (&[u8], Option<RectPx>) {
        let p = if color {
            &self.color[page]
        } else {
            &self.alpha[page]
        };
        (&p.data, p.dirty)
    }

    /// Union of all blitted content on a page.
    pub fn page_used(&self, color: bool, page: usize) -> Option<RectPx> {
        let p = if color {
            &self.color[page]
        } else {
            &self.alpha[page]
        };
        p.used
    }

    pub fn clear_page_dirty(&mut self) {
        for p in self.alpha.iter_mut().chain(self.color.iter_mut()) {
            p.dirty = None;
        }
    }

    /// GPU residency rows, indexed by `RasterId`.
    pub fn gpu_rows(&self) -> &[RasterGpu] {
        &self.gpu
    }

    /// Residency rows changed since the last call.
    pub fn take_gpu_dirty(&mut self) -> Vec<std::ops::Range<usize>> {
        self.gpu_dirty.take()
    }

    /// `take_gpu_dirty` into a reused buffer (replaced).
    pub fn take_gpu_dirty_into(&mut self, out: &mut Vec<std::ops::Range<usize>>) {
        self.gpu_dirty.take_into(out);
    }
}

#[allow(clippy::too_many_arguments)]
fn blit(page: &mut Page, page_size: u32, bpp: u32, x: u16, y: u16, w: u32, h: u32, src: &[u8]) {
    let stride = (page_size * bpp) as usize;
    let row = (w * bpp) as usize;
    let (x, y) = (x as usize, y as usize);
    for r in 0..h as usize {
        let dst = (y + r) * stride + x * bpp as usize;
        page.data[dst..dst + row].copy_from_slice(&src[r * row..r * row + row]);
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

    fn fill(atlas: &mut RasterAtlas, n: usize) -> Vec<RasterId> {
        (0..n)
            .map(|_| {
                let id = atlas.new_id(16, 16, false);
                atlas.insert(id, &[7u8; 256]);
                id
            })
            .collect()
    }

    /// 64² page, 18² slots (16 + gutter): etagere fits 6.
    const FIT: usize = 6;

    #[test]
    fn evicts_unpinned_lru_first() {
        let mut atlas = RasterAtlas::with_budget(64, 1, 1);
        let old = fill(&mut atlas, FIT);
        assert_eq!(atlas.stats.evictions, 0);
        atlas.begin_epoch();
        // Pin the last two; allocate two more: two unpinned rasters go.
        for id in &old[FIT - 2..] {
            assert!(atlas.touch(*id));
        }
        fill(&mut atlas, 2);
        assert_eq!(atlas.stats.evictions, 2);
        for id in &old[FIT - 2..] {
            assert!(atlas.entry(*id).resident, "pinned raster evicted");
        }
        assert_eq!(atlas.stats.over_budget_pages, 0);
    }

    /// When everything is pinned the atlas grows past its cap instead
    /// of dropping a visible raster.
    #[test]
    fn pinned_overflow_grows_past_cap() {
        let mut atlas = RasterAtlas::with_budget(64, 1, 1);
        let ids = fill(&mut atlas, FIT + 2);
        assert!(ids.iter().all(|id| atlas.entry(*id).resident));
        assert_eq!(atlas.alpha_pages(), 2);
        assert_eq!(atlas.stats.over_budget_pages, 1);
    }

    /// A raster no page can hold is reported, not a panic.
    #[test]
    fn oversized_raster_is_reported_not_fatal() {
        let mut atlas = RasterAtlas::new();
        let id = atlas.new_id(2047, 1, false);
        atlas.insert(id, &vec![1u8; 2047]);
        assert!(!atlas.entry(id).resident);
        assert_eq!(atlas.stats.oversized, 1);
    }

    /// Eviction keeps the id and its size; re-insert restores residency
    /// and reports the move through the GPU dirty rows.
    #[test]
    fn relocation_keeps_identity() {
        let mut atlas = RasterAtlas::with_budget(64, 1, 1);
        let ids = fill(&mut atlas, FIT);
        atlas.take_gpu_dirty();
        atlas.begin_epoch();
        fill(&mut atlas, 1);
        let gone = ids
            .iter()
            .find(|id| !atlas.entry(**id).resident)
            .copied()
            .unwrap();
        assert_eq!(atlas.entry(gone).w, 16);
        atlas.begin_epoch();
        assert!(!atlas.touch(gone));
        atlas.insert(gone, &[1u8; 256]);
        assert!(atlas.entry(gone).resident);
        let dirty = atlas.take_gpu_dirty();
        assert!(dirty.iter().any(|r| r.contains(&(gone.0 as usize))));
    }
}
