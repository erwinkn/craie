//! The retained scene: primitive pools, chunks and their placements,
//! transform and clip tables, draw order, layers, and raster residency.
//!
//! The owner (craie-ui) writes chunks when their content changes, patches
//! placements, paints, and transform records when only those change, and
//! replaces the draw order when structure changes. `prepare` derives the
//! frame's draw list (culled, merged, with isolated opacity layers) and
//! caches it until something it depends on changes.

use craie_core::counters::Counters;
use craie_core::dirty::DirtyRanges;
use craie_core::geom::{Affine, Rect, Size};
use craie_core::span::SpanPool;

use crate::atlas::{RasterAtlas, RasterId};
use crate::chunk::{Chunk, ChunkWriter, PaintSlot, Placement};
use crate::prim::{Color, GlyphInstance, NO_PAINT, RectInstance, SegKind};
use crate::space::{Clips, NONE, Transforms};

/// One entry of the draw order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderItem {
    Chunk(u32),
    /// Starts an isolated group composited with the layer's opacity.
    BeginLayer(u32),
    EndLayer,
}

/// One command of a frame's draw list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawCmd {
    /// Instances `start..start + count` of the rect pool.
    Rects { start: u32, count: u32 },
    /// Instances `start..start + count` of the glyph pool.
    Glyphs { start: u32, count: u32 },
    /// Redirect drawing into a transparent offscreen target covering
    /// `bounds` (device px: x0, y0, x1, y1).
    BeginLayer { opacity: f32, bounds: [i32; 4] },
    /// Composite the innermost layer into its parent target.
    EndLayer,
}

#[derive(Default)]
pub struct DrawList {
    pub cmds: Vec<DrawCmd>,
    /// Chunks with at least one drawn segment, in draw order.
    pub visible: Vec<u32>,
    pub viewport: Size,
}

pub struct Scene {
    pub rects: SpanPool<RectInstance>,
    pub glyphs: SpanPool<GlyphInstance>,
    pub paints: SpanPool<u32>,
    chunks: Vec<Chunk>,
    placements: Vec<Placement>,
    placement_dirty: DirtyRanges,
    pub transforms: Transforms,
    pub clips: Clips,
    order: Vec<OrderItem>,
    layers: Vec<f32>,
    pub atlas: RasterAtlas,
    pub clear: Color,
    /// Display scale (device px per logical unit).
    pub scale: f32,
    list: DrawList,
    list_valid: bool,
    /// (world, clip) revisions the draw list was culled against.
    list_revs: (u64, u64),
    /// Bumped whenever a placement changes (visibility input).
    pub placement_rev: u64,
    pub counters: Counters,
}

impl Default for Scene {
    fn default() -> Scene {
        Scene::new()
    }
}

impl Scene {
    pub fn new() -> Scene {
        Scene {
            rects: SpanPool::with_dirty_tracking(RectInstance::default()),
            glyphs: SpanPool::with_dirty_tracking(GlyphInstance::default()),
            paints: SpanPool::with_dirty_tracking(0),
            chunks: Vec::new(),
            placements: Vec::new(),
            placement_dirty: DirtyRanges::default(),
            transforms: Transforms::default(),
            clips: Clips::default(),
            order: Vec::new(),
            layers: Vec::new(),
            atlas: RasterAtlas::new(),
            clear: Color::BLACK,
            scale: 1.0,
            list: DrawList::default(),
            list_valid: false,
            list_revs: (u64::MAX, u64::MAX),
            placement_rev: 0,
            counters: Counters::default(),
        }
    }

    fn ensure(&mut self, id: u32) {
        let n = id as usize + 1;
        if self.chunks.len() < n {
            self.chunks.resize(n, Chunk::default());
            self.placements.resize(n, Placement::default());
        }
    }

    pub fn chunk(&self, id: u32) -> Option<&Chunk> {
        self.chunks.get(id as usize).filter(|c| c.live)
    }

    pub fn placement(&self, id: u32) -> Placement {
        self.placements
            .get(id as usize)
            .copied()
            .unwrap_or_default()
    }

    pub fn chunk_capacity(&self) -> usize {
        self.chunks.len()
    }

    /// Replaces chunk `id`'s primitives with the writer's contents.
    /// Paint slots become absolute paint indices; `chunk` fields become
    /// `id`. The writer is left cleared.
    pub fn commit_chunk(&mut self, id: u32, w: &mut ChunkWriter) {
        self.ensure(id);
        let mut c = self.chunks[id as usize];
        self.paints.set(&mut c.paints, &w.paints);
        let base = c.paints.start() as u32;
        for r in &mut w.rects {
            r.fill += base;
            if r.border != NO_PAINT {
                r.border += base;
            }
            r.chunk = id;
        }
        for g in &mut w.glyphs {
            g.paint += base;
            g.chunk = id;
        }
        self.rects.set(&mut c.rects, &w.rects);
        self.glyphs.set(&mut c.glyphs, &w.glyphs);
        c.segments = [None; crate::chunk::MAX_SEGMENTS];
        for (i, s) in w.segments.iter().enumerate() {
            c.segments[i] = Some(*s);
        }
        c.bounds = w.bounds.unwrap_or(Rect::ZERO);
        c.live = true;
        self.chunks[id as usize] = c;
        w.clear();
        self.counters.chunks_built += 1;
        self.list_valid = false;
    }

    /// Drops chunk `id` and returns its pool ranges.
    pub fn free_chunk(&mut self, id: u32) {
        let Some(c) = self.chunks.get_mut(id as usize) else {
            return;
        };
        if !c.live {
            return;
        }
        let mut c = *c;
        self.rects.free(&mut c.rects);
        self.glyphs.free(&mut c.glyphs);
        self.paints.free(&mut c.paints);
        self.chunks[id as usize] = Chunk::default();
        self.list_valid = false;
    }

    /// Writes a chunk's placement; no-op when unchanged.
    pub fn set_placement(&mut self, id: u32, p: Placement) {
        self.ensure(id);
        if self.placements[id as usize] != p {
            self.placements[id as usize] = p;
            self.placement_dirty.add(id as usize..id as usize + 1);
            self.placement_rev += 1;
            self.list_valid = false;
        }
    }

    /// Patches one paint record of a chunk (a color change touches no
    /// primitive).
    pub fn set_paint(&mut self, id: u32, slot: PaintSlot, color: u32) {
        let Some(c) = self.chunk(id) else { return };
        let span = c.paints;
        if (slot.0 as usize) < span.len() && self.paints.get(span)[slot.0 as usize] != color {
            self.paints.set_at(span, slot.0 as usize, color);
            self.counters.paints_patched += 1;
        }
    }

    pub fn paint(&self, id: u32, slot: PaintSlot) -> Option<u32> {
        let c = self.chunk(id)?;
        self.paints.get(c.paints).get(slot.0 as usize).copied()
    }

    /// Replaces the draw order and the layer table (opacity per layer).
    pub fn set_order(&mut self, order: Vec<OrderItem>, layers: Vec<f32>) {
        self.order = order;
        self.layers = layers;
        self.counters.draw_orders += 1;
        self.list_valid = false;
    }

    pub fn order(&self) -> &[OrderItem] {
        &self.order
    }

    pub fn set_layer_opacity(&mut self, layer: u32, opacity: f32) {
        if self.layers[layer as usize] != opacity {
            self.layers[layer as usize] = opacity;
            self.list_valid = false;
        }
    }

    /// Chunk placement rows (GPU mirror) and their pending dirty ranges.
    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    pub fn take_placement_dirty(&mut self) -> Vec<std::ops::Range<usize>> {
        self.placement_dirty.take()
    }

    /// Starts a frame's residency epoch. Rasters emitted this frame are
    /// pinned as they are used; `prepare` pins every raster of every
    /// visible chunk before it re-rasterizes missing ones, so a visible
    /// glyph never drops. Eviction picks the least recently used of the
    /// rest, which keeps the budget unless the visible set alone
    /// exceeds it.
    pub fn begin_frame(&mut self) {
        self.atlas.begin_epoch();
    }

    /// World-space bounds of a chunk in device pixels.
    pub fn chunk_world_bounds(&self, id: u32) -> Rect {
        let c = &self.chunks[id as usize];
        let p = self.placements[id as usize];
        let w = self.transforms.world(p.transform);
        let local = Rect::new(
            c.bounds.origin.x + p.offset[0],
            c.bounds.origin.y + p.offset[1],
            c.bounds.size.width,
            c.bounds.size.height,
        );
        w.map_rect(&local)
    }

    /// Derives world matrices and clip rows, then (re)builds the draw
    /// list when anything it depends on changed. Pins the rasters of
    /// every visible chunk and appends the non-resident ones to
    /// `missing` — the caller re-rasterizes them before drawing.
    pub fn prepare(&mut self, viewport: Size, missing: &mut Vec<RasterId>) -> &DrawList {
        self.transforms.derive();
        self.clips.derive(&self.transforms);
        // Culling depends on world matrices and clips: a revision the list
        // was not built from invalidates it, whoever derived first.
        let revs = (self.transforms.world_rev, self.clips.rev);
        if !self.list_valid || self.list.viewport != viewport || self.list_revs != revs {
            self.build_list(viewport);
            self.list_revs = revs;
        }
        for i in 0..self.list.visible.len() {
            let c = self.chunks[self.list.visible[i] as usize];
            for g in self.glyphs.get(c.glyphs) {
                let id = RasterId(g.raster);
                if !self.atlas.touch(id) {
                    missing.push(id);
                }
            }
        }
        missing.sort_unstable_by_key(|r| r.0);
        missing.dedup();
        &self.list
    }

    pub fn draw_list(&self) -> &DrawList {
        &self.list
    }

    fn build_list(&mut self, viewport: Size) {
        self.list_valid = true;
        let screen = Rect::new(0.0, 0.0, viewport.width, viewport.height);
        let mut cmds = std::mem::take(&mut self.list.cmds);
        let mut visible = std::mem::take(&mut self.list.visible);
        cmds.clear();
        visible.clear();
        // Clip world bounds, once per clip.
        let clip_bounds: Vec<Rect> = (0..self.clips.len() as u32)
            .map(|i| {
                self.clips
                    .world_bounds(&self.transforms, i)
                    .unwrap_or(Rect::ZERO)
            })
            .collect();
        // Open layers: (cmd index of BeginLayer, accumulated bounds).
        let mut layers: Vec<(usize, Option<Rect>)> = Vec::new();
        let mut skip_depth = 0usize;
        for item in &self.order {
            match *item {
                OrderItem::BeginLayer(l) => {
                    if skip_depth > 0 || self.layers[l as usize] <= 0.0 {
                        skip_depth += 1;
                        continue;
                    }
                    layers.push((cmds.len(), None));
                    cmds.push(DrawCmd::BeginLayer {
                        opacity: self.layers[l as usize],
                        bounds: [0; 4],
                    });
                }
                OrderItem::EndLayer => {
                    if skip_depth > 0 {
                        skip_depth -= 1;
                        continue;
                    }
                    let (at, bounds) = layers.pop().expect("unbalanced layer order");
                    match bounds
                        .map(|b| b.intersect(&screen))
                        .filter(|b| b.size.width > 0.0 && b.size.height > 0.0)
                    {
                        Some(b) => {
                            let px = [
                                b.origin.x.floor() as i32,
                                b.origin.y.floor() as i32,
                                b.max_x().ceil() as i32,
                                b.max_y().ceil() as i32,
                            ];
                            if let DrawCmd::BeginLayer { bounds, .. } = &mut cmds[at] {
                                *bounds = px;
                            }
                            cmds.push(DrawCmd::EndLayer);
                            if let Some(parent) = layers.last_mut() {
                                parent.1 = Some(parent.1.map_or(b, |p| p.union(&b)));
                            }
                        }
                        // Nothing visible inside: drop the layer.
                        None => cmds.truncate(at),
                    }
                }
                OrderItem::Chunk(id) => {
                    if skip_depth > 0 {
                        continue;
                    }
                    let Some(c) = self.chunks.get(id as usize).filter(|c| c.live) else {
                        continue;
                    };
                    if c.rects.is_empty() && c.glyphs.is_empty() {
                        continue;
                    }
                    let mut b = self.chunk_world_bounds(id);
                    let clip = self.placements[id as usize].clip;
                    if clip != NONE {
                        b = b.intersect(&clip_bounds[clip as usize]);
                    }
                    if !b.intersects(&screen) {
                        continue;
                    }
                    visible.push(id);
                    if let Some(l) = layers.last_mut() {
                        l.1 = Some(l.1.map_or(b, |p| p.union(&b)));
                    }
                    for s in c.segments() {
                        let (base, kind) = match s.kind {
                            SegKind::Rects => (c.rects.start() as u32, 0),
                            SegKind::Glyphs => (c.glyphs.start() as u32, 1),
                        };
                        let start = base + s.start;
                        // Merge with the previous draw when contiguous.
                        match (kind, cmds.last_mut()) {
                            (0, Some(DrawCmd::Rects { start: s0, count }))
                                if *s0 + *count == start =>
                            {
                                *count += s.len
                            }
                            (1, Some(DrawCmd::Glyphs { start: s0, count }))
                                if *s0 + *count == start =>
                            {
                                *count += s.len
                            }
                            (0, _) => cmds.push(DrawCmd::Rects {
                                start,
                                count: s.len,
                            }),
                            _ => cmds.push(DrawCmd::Glyphs {
                                start,
                                count: s.len,
                            }),
                        }
                    }
                }
            }
        }
        debug_assert!(layers.is_empty(), "unbalanced layer order");
        self.list.cmds = cmds;
        self.list.visible = visible;
        self.list.viewport = viewport;
    }

    /// Items waiting for upload across the pools and the placement
    /// table (an upper bound: overlapping writes may count twice). Zero
    /// after an unchanged frame.
    pub fn pending_upload_items(&self) -> usize {
        self.rects.dirty_items()
            + self.glyphs.dirty_items()
            + self.paints.dirty_items()
            + self.placement_dirty.items_upper()
    }

    /// Every primitive in draw order, resolved to device space: the
    /// harness compares an incrementally updated scene against a clean
    /// rebuild through this view. `raster_key` maps a raster to a
    /// stable identity (ids differ between two scenes).
    pub fn resolve(&self, raster_key: &dyn Fn(RasterId) -> u64) -> Vec<Resolved> {
        self.resolve_where(raster_key, &|_| true)
    }

    /// `resolve` restricted to the chunks the last draw list draws.
    pub fn resolve_drawn(&self, raster_key: &dyn Fn(RasterId) -> u64) -> Vec<Resolved> {
        let mut drawn = vec![false; self.chunks.len()];
        for &id in &self.list.visible {
            drawn[id as usize] = true;
        }
        self.resolve_where(raster_key, &|id| drawn[id as usize])
    }

    fn resolve_where(
        &self,
        raster_key: &dyn Fn(RasterId) -> u64,
        keep: &dyn Fn(u32) -> bool,
    ) -> Vec<Resolved> {
        let mut out = Vec::new();
        let mut opacity: Vec<f32> = vec![1.0];
        for item in &self.order {
            match *item {
                OrderItem::BeginLayer(l) => {
                    let o = *opacity.last().unwrap() * self.layers[l as usize];
                    opacity.push(o);
                }
                OrderItem::EndLayer => {
                    opacity.pop();
                }
                OrderItem::Chunk(id) => {
                    let Some(c) = self.chunk(id) else { continue };
                    if !keep(id) {
                        continue;
                    }
                    let o = *opacity.last().unwrap();
                    if o <= 0.0 {
                        continue;
                    }
                    let p = self.placements[id as usize];
                    let w = self
                        .transforms
                        .world(p.transform)
                        .mul(&Affine::translate(p.offset[0], p.offset[1]));
                    let clip = if p.clip == NONE {
                        None
                    } else {
                        self.clips.world_bounds(&self.transforms, p.clip)
                    };
                    let clip_radius = if p.clip == NONE {
                        0.0
                    } else {
                        self.clips.get(p.clip).radius
                    };
                    for s in c.segments() {
                        match s.kind {
                            SegKind::Rects => {
                                let rs = &self.rects.get(c.rects)
                                    [s.start as usize..(s.start + s.len) as usize];
                                for r in rs {
                                    let local =
                                        Rect::new(r.rect[0], r.rect[1], r.rect[2], r.rect[3]);
                                    out.push(Resolved {
                                        kind: 0,
                                        bounds: w.map_rect(&local),
                                        color: self.paints.backing()[r.fill as usize],
                                        aux: if r.border == NO_PAINT {
                                            0
                                        } else {
                                            self.paints.backing()[r.border as usize] as u64
                                        },
                                        params: [r.radius, r.border_width],
                                        opacity: o,
                                        clip,
                                        clip_radius,
                                    });
                                }
                            }
                            SegKind::Glyphs => {
                                let gs = &self.glyphs.get(c.glyphs)
                                    [s.start as usize..(s.start + s.len) as usize];
                                for g in gs {
                                    let e = self.atlas.entry(RasterId(g.raster));
                                    let size = [e.w as f32 / self.scale, e.h as f32 / self.scale];
                                    let local = Rect::new(g.pos[0], g.pos[1], size[0], size[1]);
                                    out.push(Resolved {
                                        kind: 1,
                                        bounds: w.map_rect(&local),
                                        color: self.paints.backing()[g.paint as usize],
                                        aux: raster_key(RasterId(g.raster)),
                                        params: [0.0; 2],
                                        opacity: o,
                                        clip,
                                        clip_radius,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

/// One primitive in world space (device px), for equivalence checks.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    /// 0 rect, 1 glyph.
    pub kind: u8,
    pub bounds: Rect,
    pub color: u32,
    /// Rect: border color. Glyph: stable raster key.
    pub aux: u64,
    pub params: [f32; 2],
    pub opacity: f32,
    pub clip: Option<Rect>,
    /// Corner radius of the innermost clip.
    pub clip_radius: f32,
}

impl Resolved {
    /// Equal within `tol` device pixels.
    pub fn close_to(&self, o: &Resolved, tol: f32) -> bool {
        let near = |a: Rect, b: Rect| {
            (a.origin.x - b.origin.x).abs() <= tol
                && (a.origin.y - b.origin.y).abs() <= tol
                && (a.size.width - b.size.width).abs() <= tol
                && (a.size.height - b.size.height).abs() <= tol
        };
        self.kind == o.kind
            && self.color == o.color
            && self.aux == o.aux
            && self.params == o.params
            && (self.opacity - o.opacity).abs() <= 1e-6
            && self.clip_radius == o.clip_radius
            && near(self.bounds, o.bounds)
            && match (self.clip, o.clip) {
                (None, None) => true,
                (Some(a), Some(b)) => near(a, b),
                _ => false,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkWriter;
    use craie_core::geom::Affine;

    fn scene_with(n: u32) -> Scene {
        let mut s = Scene::new();
        let root = s.transforms.alloc(Affine::IDENTITY, NONE);
        s.transforms.set_order(vec![root]);
        let mut w = ChunkWriter::new();
        for id in 0..n {
            let fill = w.paint(0xFF00_00FF);
            w.rect(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0, fill);
            s.commit_chunk(id, &mut w);
            s.set_placement(
                id,
                Placement {
                    offset: [0.0, id as f32 * 20.0],
                    transform: root,
                    clip: NONE,
                },
            );
        }
        s.set_order((0..n).map(OrderItem::Chunk).collect(), vec![]);
        s
    }

    #[test]
    fn contiguous_chunks_merge_into_one_draw() {
        let mut s = scene_with(4);
        let mut missing = Vec::new();
        let list = s.prepare(Size::new(100.0, 100.0), &mut missing);
        assert_eq!(list.cmds, vec![DrawCmd::Rects { start: 0, count: 4 }]);
        assert_eq!(list.visible, vec![0, 1, 2, 3]);
    }

    #[test]
    fn offscreen_chunks_are_culled() {
        let mut s = scene_with(10);
        let mut missing = Vec::new();
        // 10 px tall rows every 20 px: rows 0..=2 intersect a 50 px view.
        let list = s.prepare(Size::new(100.0, 50.0), &mut missing);
        assert_eq!(list.visible, vec![0, 1, 2]);
    }

    #[test]
    fn layers_wrap_their_range_and_empty_layers_drop() {
        let mut s = scene_with(3);
        s.set_order(
            vec![
                OrderItem::Chunk(0),
                OrderItem::BeginLayer(0),
                OrderItem::Chunk(1),
                OrderItem::EndLayer,
                OrderItem::BeginLayer(1),
                OrderItem::EndLayer,
                OrderItem::Chunk(2),
            ],
            vec![0.5, 0.5],
        );
        let mut missing = Vec::new();
        let cmds = s
            .prepare(Size::new(100.0, 100.0), &mut missing)
            .cmds
            .clone();
        assert_eq!(cmds.len(), 5);
        assert!(
            matches!(cmds[1], DrawCmd::BeginLayer { opacity, bounds: [0, 20, 10, 30] } if opacity == 0.5)
        );
        assert_eq!(cmds[3], DrawCmd::EndLayer);
        // Zero opacity skips the subtree entirely.
        s.set_layer_opacity(0, 0.0);
        let cmds = s
            .prepare(Size::new(100.0, 100.0), &mut missing)
            .cmds
            .clone();
        assert!(!cmds.iter().any(|c| matches!(c, DrawCmd::BeginLayer { .. })));
    }

    #[test]
    fn paint_patch_touches_no_primitive() {
        let mut s = scene_with(1);
        s.rects.take_dirty();
        s.paints.take_dirty();
        s.set_paint(0, PaintSlot(0), 0x00FF_00FF);
        assert!(s.rects.take_dirty().is_empty());
        assert_eq!(s.paints.take_dirty().len(), 1);
        assert_eq!(s.paint(0, PaintSlot(0)), Some(0x00FF_00FF));
        assert_eq!(s.counters.paints_patched, 1);
    }

    #[test]
    fn unchanged_frame_rebuilds_nothing() {
        let mut s = scene_with(3);
        let mut missing = Vec::new();
        s.prepare(Size::new(100.0, 100.0), &mut missing);
        s.rects.take_dirty();
        s.paints.take_dirty();
        s.glyphs.take_dirty();
        s.take_placement_dirty();
        let built = s.counters.chunks_built;
        s.prepare(Size::new(100.0, 100.0), &mut missing);
        assert_eq!(s.pending_upload_items(), 0);
        assert_eq!(s.counters.chunks_built, built);
    }
}
