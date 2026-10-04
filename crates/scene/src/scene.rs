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
use craie_core::geom::{Rect, Size};
use craie_core::span::SpanPool;

use crate::atlas::{RasterAtlas, RasterId};
use crate::chunk::{Chunk, ChunkWriter, PaintSlot, Placement};
use crate::prim::{Color, GlyphInstance, NO_PAINT, PathVertex, RectInstance, SegKind};
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
    /// Path mesh indices `start..start + count` (whole triangles).
    Paths { start: u32, count: u32 },
    /// Redirect drawing into a transparent offscreen target covering
    /// `bounds` (device px: x0, y0, x1, y1). `msaa`: a run of path
    /// meshes, rendered multisampled and resolved (opacity 1); opacity
    /// layers and the window stay single-sampled.
    BeginLayer {
        opacity: f32,
        bounds: [i32; 4],
        msaa: bool,
    },
    /// Composite the innermost layer into its parent target.
    EndLayer,
}

#[derive(Default)]
pub struct DrawList {
    pub cmds: Vec<DrawCmd>,
    /// Chunks with at least one drawn segment, in draw order.
    pub visible: Vec<u32>,
    pub viewport: Size,
    /// The list draws path meshes (in multisampled layers of their own:
    /// tessellated triangles need multisampling; rects and glyphs
    /// anti-alias analytically and never render multisampled).
    pub paths: bool,
}

pub struct Scene {
    pub rects: SpanPool<RectInstance>,
    pub glyphs: SpanPool<GlyphInstance>,
    pub paints: SpanPool<u32>,
    pub path_vertices: SpanPool<PathVertex>,
    pub path_indices: SpanPool<u32>,
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
    /// Draw-list build scratch (kept: a rebuild allocates nothing).
    clip_bounds: Vec<Option<Rect>>,
    open_layers: Vec<(usize, Option<Rect>)>,
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
            path_vertices: SpanPool::with_dirty_tracking(PathVertex::default()),
            path_indices: SpanPool::with_dirty_tracking(0),
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
            clip_bounds: Vec::new(),
            open_layers: Vec::new(),
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
        for v in &mut w.path_vertices {
            v.paint += base;
            v.info = (v.info & PathVertex::GRADIENT) | id;
        }
        self.rects.set(&mut c.rects, &w.rects);
        self.glyphs.set(&mut c.glyphs, &w.glyphs);
        self.path_vertices
            .set(&mut c.path_vertices, &w.path_vertices);
        let vbase = c.path_vertices.start() as u32;
        for i in &mut w.path_indices {
            *i += vbase;
        }
        self.path_indices.set(&mut c.path_indices, &w.path_indices);
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
        self.path_vertices.free(&mut c.path_vertices);
        self.path_indices.free(&mut c.path_indices);
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

    /// `take_placement_dirty` into a reused buffer (replaced).
    pub fn take_placement_dirty_into(&mut self, out: &mut Vec<std::ops::Range<usize>>) {
        self.placement_dirty.take_into(out);
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

    /// World-space bounds of a chunk in device pixels, as drawn: from
    /// the origin the shader uses (snapped to the pixel grid in a space
    /// that snaps).
    pub fn chunk_world_bounds(&self, id: u32) -> Rect {
        let c = &self.chunks[id as usize];
        let p = self.placements[id as usize];
        let mut w = self.transforms.world(p.transform);
        let mut origin = w.apply(craie_core::geom::Point::new(p.offset[0], p.offset[1]));
        if w.is_axis_aligned() && self.transforms.snaps(p.transform) {
            origin = craie_core::geom::Point::new(
                origin.x.round_ties_even(),
                origin.y.round_ties_even(),
            );
        }
        w.0[4] = origin.x;
        w.0[5] = origin.y;
        w.map_rect(&c.bounds)
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
        let mut paths = false;
        // Clip world bounds, once per clip. `None` bounds nothing (a clip
        // chain with only open-axis records).
        let mut clip_bounds = std::mem::take(&mut self.clip_bounds);
        clip_bounds.clear();
        clip_bounds.extend(
            (0..self.clips.len() as u32).map(|i| self.clips.world_bounds(&self.transforms, i)),
        );
        // Open layers: (cmd index of BeginLayer, accumulated bounds).
        let mut layers = std::mem::take(&mut self.open_layers);
        layers.clear();
        let mut skip_depth = 0usize;
        // The open run of meshes: its bounds, and its draws (emitted in a
        // multisampled layer when it closes).
        let mut run: Option<Rect> = None;
        let mut run_cmds: Vec<DrawCmd> = Vec::new();
        for item in &self.order {
            match *item {
                OrderItem::BeginLayer(l) => {
                    if skip_depth > 0 || self.layers[l as usize] <= 0.0 {
                        skip_depth += 1;
                        continue;
                    }
                    close_run(&mut cmds, &mut run, &mut run_cmds, screen);
                    layers.push((cmds.len(), None));
                    cmds.push(DrawCmd::BeginLayer {
                        opacity: self.layers[l as usize],
                        bounds: [0; 4],
                        msaa: false,
                    });
                }
                OrderItem::EndLayer => {
                    if skip_depth > 0 {
                        skip_depth -= 1;
                        continue;
                    }
                    close_run(&mut cmds, &mut run, &mut run_cmds, screen);
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
                    if c.rects.is_empty() && c.glyphs.is_empty() && c.path_indices.is_empty() {
                        continue;
                    }
                    let mut b = self.chunk_world_bounds(id);
                    let clip = self.placements[id as usize].clip;
                    if clip != NONE
                        && let Some(c) = clip_bounds[clip as usize]
                    {
                        b = b.intersect(&c);
                    }
                    if !b.intersects(&screen) {
                        continue;
                    }
                    visible.push(id);
                    if let Some(l) = layers.last_mut() {
                        l.1 = Some(l.1.map_or(b, |p| p.union(&b)));
                    }
                    for s in c.segments() {
                        let base = match s.kind {
                            SegKind::Rects => c.rects.start(),
                            SegKind::Glyphs => c.glyphs.start(),
                            SegKind::Paths => c.path_indices.start(),
                        } as u32;
                        let start = base + s.start;
                        // A run of meshes draws in a multisampled layer of
                        // its own. Other content that overlaps none of the
                        // run so far draws before the whole run (it only
                        // has to stay below the run's later meshes), so the
                        // run goes on; content that overlaps it ends it.
                        let target = if s.kind == SegKind::Paths {
                            run = Some(run.map_or(b, |rb| rb.union(&b)));
                            &mut run_cmds
                        } else {
                            // Disjoint in whole pixels, with a pixel
                            // of margin: no pixel both touch.
                            let px = |r: Rect| {
                                let (x0, y0) = (r.origin.x.floor() - 1.0, r.origin.y.floor() - 1.0);
                                Rect::new(
                                    x0,
                                    y0,
                                    r.max_x().ceil() + 1.0 - x0,
                                    r.max_y().ceil() + 1.0 - y0,
                                )
                            };
                            if run.is_some_and(|rb| px(rb).intersects(&px(b))) {
                                close_run(&mut cmds, &mut run, &mut run_cmds, screen);
                            }
                            &mut cmds
                        };
                        // Merge with the previous draw when contiguous.
                        match (s.kind, target.last_mut()) {
                            (SegKind::Rects, Some(DrawCmd::Rects { start: s0, count }))
                            | (SegKind::Glyphs, Some(DrawCmd::Glyphs { start: s0, count }))
                            | (SegKind::Paths, Some(DrawCmd::Paths { start: s0, count }))
                                if *s0 + *count == start =>
                            {
                                *count += s.len
                            }
                            (SegKind::Rects, _) => target.push(DrawCmd::Rects {
                                start,
                                count: s.len,
                            }),
                            (SegKind::Glyphs, _) => target.push(DrawCmd::Glyphs {
                                start,
                                count: s.len,
                            }),
                            (SegKind::Paths, _) => {
                                paths = true;
                                target.push(DrawCmd::Paths {
                                    start,
                                    count: s.len,
                                })
                            }
                        }
                    }
                }
            }
        }
        close_run(&mut cmds, &mut run, &mut run_cmds, screen);
        debug_assert!(layers.is_empty(), "unbalanced layer order");
        self.clip_bounds = clip_bounds;
        self.open_layers = layers;
        self.list.cmds = cmds;
        self.list.visible = visible;
        self.list.viewport = viewport;
        self.list.paths = paths;
    }

    /// Items waiting for upload across the pools and the placement
    /// table (an upper bound: overlapping writes may count twice). Zero
    /// after an unchanged frame.
    pub fn pending_upload_items(&self) -> usize {
        self.rects.dirty_items()
            + self.glyphs.dirty_items()
            + self.paints.dirty_items()
            + self.path_vertices.dirty_items()
            + self.path_indices.dirty_items()
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
                    // Mirror the shader: the chunk origin (and snapping
                    // rect edges) round to device pixels in an
                    // axis-aligned space that snaps.
                    let world = self.transforms.world(p.transform);
                    let snap = world.is_axis_aligned() && self.transforms.snaps(p.transform);
                    let mut origin =
                        world.apply(craie_core::geom::Point::new(p.offset[0], p.offset[1]));
                    if snap {
                        // Ties to even, as WGSL `round` lowers on every
                        // backend (Metal rint, SPIR-V/GLSL RoundEven).
                        origin = craie_core::geom::Point::new(
                            origin.x.round_ties_even(),
                            origin.y.round_ties_even(),
                        );
                    }
                    let mut lin = world;
                    lin.0[4] = origin.x;
                    lin.0[5] = origin.y;
                    let w = lin;
                    let snap_rect = |r: Rect| {
                        if !snap {
                            return r;
                        }
                        let x0 = r.origin.x.round_ties_even();
                        let y0 = r.origin.y.round_ties_even();
                        Rect::new(
                            x0,
                            y0,
                            r.max_x().round_ties_even() - x0,
                            r.max_y().round_ties_even() - y0,
                        )
                    };
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
                                    let b = w.map_rect(&local);
                                    let b = if r.flags & RectInstance::FLAG_SNAP != 0 {
                                        snap_rect(b)
                                    } else {
                                        b
                                    };
                                    let words = self.paints.backing();
                                    let shadow = r.flags & RectInstance::FLAG_SHADOW != 0;
                                    out.push(Resolved {
                                        kind: if shadow { 3 } else { 0 },
                                        bounds: b,
                                        color: words[r.fill as usize],
                                        aux: if shadow {
                                            // The box it is cut against, and
                                            // whether it is inset.
                                            let at = r.border as usize;
                                            words[at..at + 5].iter().fold(
                                                (r.flags & RectInstance::FLAG_INSET) as u64,
                                                |h, &w| h.wrapping_mul(0x100_0000_01B3) ^ w as u64,
                                            )
                                        } else if r.border == NO_PAINT {
                                            0
                                        } else {
                                            words[r.border as usize] as u64
                                        },
                                        params: [r.radius, r.border_width],
                                        opacity: o,
                                        clip,
                                        clip_radius,
                                        geometry: Vec::new(),
                                    });
                                }
                            }
                            SegKind::Paths => {
                                // One entry per segment: its device-space
                                // bounds; in `geometry`, per indexed vertex
                                // its device position and its place on the
                                // gradient (device px); in `aux`, every vertex's paint
                                // kind or color, each gradient's stops
                                // (offsets and colors), and the index
                                // count.
                                let is = &self.path_indices.get(c.path_indices)
                                    [s.start as usize..(s.start + s.len) as usize];
                                let vs = self.path_vertices.backing();
                                let words = self.paints.backing();
                                let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
                                let mut geometry = Vec::with_capacity(is.len() * 2);
                                let mut aux = is.len() as u64;
                                let mut mix = |v: u64| aux = aux.wrapping_mul(0x100_0000_01B3) ^ v;
                                let mut last_paint = None;
                                for &i in is {
                                    let v = vs[i as usize];
                                    let p =
                                        w.apply(craie_core::geom::Point::new(v.pos[0], v.pos[1]));
                                    lo = [lo[0].min(p.x), lo[1].min(p.y)];
                                    hi = [hi[0].max(p.x), hi[1].max(p.y)];
                                    geometry.extend([p.x, p.y]);
                                    let at = v.paint as usize;
                                    if v.info & PathVertex::GRADIENT == 0 {
                                        // Every vertex's paint: where one
                                        // paint ends and the next starts.
                                        mix(words[at] as u64);
                                        continue;
                                    }
                                    // Where the vertex falls on the gradient,
                                    // in device px: along a linear gradient,
                                    // its distance from the start and the
                                    // gradient's length; for a radial one,
                                    // its offset from the center and the
                                    // radius mapped to the screen. So the
                                    // tolerance bounds how far any gradient
                                    // feature (a hard stop too) moves on
                                    // screen, at any scale.
                                    let f = |k: usize| f32::from_bits(words[at + 1 + k]);
                                    let m = |k: usize| f32::from_bits(words[at + 5 + k]);
                                    let g = [
                                        m(0) * v.pos[0] + m(2) * v.pos[1] + m(4),
                                        m(1) * v.pos[0] + m(3) * v.pos[1] + m(5),
                                    ];
                                    let [a, b, c, d] = [w.0[0], w.0[1], w.0[2], w.0[3]];
                                    let det = a * d - b * c;
                                    let head = words[at];
                                    if head & 0xFFFF == crate::prim::gradient::LINEAR {
                                        let dir = [f(2) - f(0), f(3) - f(1)];
                                        let len2 = (dir[0] * dir[0] + dir[1] * dir[1]).max(1e-12);
                                        let t = ((g[0] - f(0)) * dir[0] + (g[1] - f(1)) * dir[1])
                                            / len2;
                                        // d t / d local, then d t / d device.
                                        let gx = (m(0) * dir[0] + m(1) * dir[1]) / len2;
                                        let gy = (m(2) * dir[0] + m(3) * dir[1]) / len2;
                                        let grad = if det.abs() > 1e-12 {
                                            ((d * gx - b * gy) / det).hypot((a * gy - c * gx) / det)
                                        } else {
                                            0.0
                                        };
                                        // Distance from the start and the
                                        // gradient's length, both in device
                                        // px (t itself where it has none).
                                        if grad > 1e-12 {
                                            geometry.extend([t / grad, 1.0 / grad]);
                                        } else {
                                            geometry.extend([t, 0.0]);
                                        }
                                    } else {
                                        // Through the full mapping from
                                        // gradient space to device px (D =
                                        // world x gradient-to-local): the
                                        // offset from the center, and the
                                        // radius along both axes. Any axis
                                        // scale moves features as far on
                                        // screen as these vectors move.
                                        let tdet = m(0) * m(3) - m(1) * m(2);
                                        if tdet.abs() > 1e-24 {
                                            // Inverse of [[m0, m2], [m1, m3]].
                                            let inv = [
                                                m(3) / tdet,
                                                -m(1) / tdet,
                                                -m(2) / tdet,
                                                m(0) / tdet,
                                            ];
                                            let dmap = |x: f32, y: f32| {
                                                let lx = inv[0] * x + inv[2] * y;
                                                let ly = inv[1] * x + inv[3] * y;
                                                [a * lx + c * ly, b * lx + d * ly]
                                            };
                                            let r = f(2);
                                            geometry.extend(dmap(g[0] - f(0), g[1] - f(1)));
                                            geometry.extend(dmap(r, 0.0));
                                            geometry.extend(dmap(0.0, r));
                                        } else {
                                            geometry.extend([
                                                g[0] - f(0),
                                                g[1] - f(1),
                                                f(2),
                                                0.0,
                                                0.0,
                                                0.0,
                                            ]);
                                        }
                                    }
                                    mix(head as u64 | 1 << 40);
                                    if last_paint == Some(v.paint) {
                                        continue;
                                    }
                                    last_paint = Some(v.paint);
                                    let n = (head >> 16) as usize;
                                    let stops = at + crate::prim::gradient::HEADER_WORDS;
                                    for k in 0..n {
                                        mix(words[stops + 2 * k] as u64);
                                        mix(words[stops + 2 * k + 1] as u64);
                                    }
                                }
                                let first = is.first().map(|&i| vs[i as usize]);
                                out.push(Resolved {
                                    kind: 2,
                                    bounds: Rect::new(lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]),
                                    color: first.map_or(0, |v| words[v.paint as usize]),
                                    aux,
                                    params: [0.0; 2],
                                    opacity: o,
                                    clip,
                                    clip_radius,
                                    geometry,
                                });
                            }
                            SegKind::Glyphs => {
                                let gs = &self.glyphs.get(c.glyphs)
                                    [s.start as usize..(s.start + s.len) as usize];
                                for g in gs {
                                    let e = self.atlas.entry(RasterId(g.raster));
                                    let size = [
                                        e.quad_w as f32 / self.scale,
                                        e.quad_h as f32 / self.scale,
                                    ];
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
                                        geometry: Vec::new(),
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

/// Ends the open run of meshes: a multisampled layer covering the run's
/// bounds (on screen, in whole pixels) around its draws; nothing when
/// none of it is on screen.
fn close_run(
    cmds: &mut Vec<DrawCmd>,
    run: &mut Option<Rect>,
    run_cmds: &mut Vec<DrawCmd>,
    screen: Rect,
) {
    let Some(b) = run.take() else { return };
    let b = b.intersect(&screen);
    if b.size.width > 0.0 && b.size.height > 0.0 {
        cmds.push(DrawCmd::BeginLayer {
            opacity: 1.0,
            bounds: [
                b.origin.x.floor() as i32,
                b.origin.y.floor() as i32,
                b.max_x().ceil() as i32,
                b.max_y().ceil() as i32,
            ],
            msaa: true,
        });
        cmds.append(run_cmds);
        cmds.push(DrawCmd::EndLayer);
    }
    run_cmds.clear();
}

/// One primitive in world space (device px), for equivalence checks.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    /// 0 rect, 1 glyph, 2 path mesh segment, 3 box shadow (bounds: its
    /// shape; aux: a hash of its box and inset flag).
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
    /// Path mesh segments: per indexed vertex, its device-space position
    /// and, for a gradient, where it falls on the gradient in device px;
    /// empty for rects and glyphs.
    pub geometry: Vec<f32>,
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
            && self.geometry.len() == o.geometry.len()
            && self
                .geometry
                .iter()
                .zip(&o.geometry)
                .all(|(a, b)| (a - b).abs() <= tol)
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
    use crate::prim::PathVertex;
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
            matches!(cmds[1], DrawCmd::BeginLayer { opacity, bounds: [0, 20, 10, 30], msaa: false } if opacity == 0.5)
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

    /// Meshes commit with indices rebased onto the vertex pool and
    /// paints onto the paint pool; contiguous mesh segments merge into
    /// one draw and flag the list; freed chunks draw nothing.
    #[test]
    fn meshes_commit_merge_and_free() {
        let mut s = scene_with(0);
        let root = 0;
        let tri = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]];
        let mut w = ChunkWriter::new();
        for id in 0..2u32 {
            let red = w.paint(0xFF00_00FF);
            w.mesh(&tri, &[0, 1, 2], red, false);
            let g = w.gradient(&crate::chunk::GradientPaint {
                kind: crate::prim::gradient::LINEAR,
                geometry: [0.0, 0.0, 1.0, 0.0],
                to_gradient: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                stops: vec![(0.0, 1), (1.0, 2)],
            });
            w.mesh(&tri, &[2, 1, 0], g, true);
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
        s.set_order(vec![OrderItem::Chunk(0), OrderItem::Chunk(1)], vec![]);
        let c1 = *s.chunk(1).unwrap();
        // Chunk 1's indices point at its own vertices in the pool; each
        // gradient vertex points at its record's first word.
        let v = c1.path_vertices.start() as u32;
        assert!(v >= 6);
        assert_eq!(
            s.path_indices.get(c1.path_indices),
            [v, v + 1, v + 2, v + 5, v + 4, v + 3]
        );
        let vs = s.path_vertices.get(c1.path_vertices);
        assert_eq!(vs[0].info, 1);
        assert_eq!(vs[3].info, 1 | PathVertex::GRADIENT);
        let paints = s.paints.get(c1.paints);
        let first = c1.paints.start() as u32;
        assert_eq!(vs[0].paint, first);
        assert_eq!(vs[3].paint, first + 1);
        assert_eq!(paints[1], crate::prim::gradient::LINEAR | 2 << 16);
        assert_eq!(paints.len(), 1 + crate::prim::gradient::HEADER_WORDS + 4);
        let mut missing = Vec::new();
        let list = s.prepare(Size::new(100.0, 100.0), &mut missing);
        // One multisampled layer around the run of meshes; index spans
        // are contiguous only when the pool packed them so: one merged
        // draw or one per chunk, 12 indices in all.
        assert!(matches!(
            list.cmds.first(),
            Some(DrawCmd::BeginLayer { opacity, msaa: true, .. }) if *opacity == 1.0
        ));
        assert_eq!(list.cmds.last(), Some(&DrawCmd::EndLayer));
        let counted: u32 = list.cmds[1..list.cmds.len() - 1]
            .iter()
            .map(|c| match c {
                DrawCmd::Paths { count, .. } => *count,
                _ => panic!("{c:?}"),
            })
            .sum();
        assert_eq!(counted, 12);
        assert!(list.paths);
        let drawn = s.resolve(&|_| 0);
        assert_eq!(drawn.len(), 2);
        assert_eq!(
            (drawn[1].kind, drawn[1].bounds),
            (2, Rect::new(0.0, 20.0, 10.0, 10.0))
        );
        assert_eq!(drawn[1].color, 0xFF00_00FF);
        s.free_chunk(0);
        s.free_chunk(1);
        let list = s.prepare(Size::new(100.0, 100.0), &mut missing);
        assert!(list.cmds.is_empty() && !list.paths);
        assert!(s.chunk(0).is_none() && s.chunk(1).is_none());
    }

    fn one_mesh(paint: Option<crate::chunk::GradientPaint>, world: Affine, offset: f32) -> Scene {
        let mut s = Scene::new();
        let root = s.transforms.alloc(world, NONE);
        s.transforms.set_order(vec![root]);
        let mut w = ChunkWriter::new();
        let tri = [[0.0, 0.0], [0.4, 8.0], [0.0, 16.0]];
        match paint {
            Some(g) => {
                let slot = w.gradient(&g);
                assert!(w.mesh(&tri, &[0, 1, 2], slot, true));
            }
            None => {
                let red = w.paint(0xFF00_00FF);
                assert!(w.mesh(&tri, &[0, 1, 2], red, false));
            }
        }
        s.commit_chunk(0, &mut w);
        s.set_placement(
            0,
            Placement {
                offset: [offset, 8.0],
                transform: root,
                clip: NONE,
            },
        );
        s.set_order(vec![OrderItem::Chunk(0)], vec![]);
        // World matrices, as `prepare` derives them.
        s.transforms.derive();
        s
    }

    /// S5A-02: culling uses the origin the shader draws at (snapped): a
    /// thin mesh placed at -0.49 draws at 0, on screen.
    #[test]
    fn meshes_cull_at_their_snapped_origin() {
        let mut s = one_mesh(None, Affine::IDENTITY, -0.49);
        let mut missing = Vec::new();
        assert_eq!(
            s.prepare(Size::new(100.0, 100.0), &mut missing).visible,
            [0]
        );
        // The bounds start at the snapped origin (0).
        let b = s.chunk_world_bounds(0);
        assert_eq!((b.origin.x, b.origin.y), (0.0, 8.0));
        assert!((b.size.width - 0.4).abs() < 1e-6);
        // Far off screen: culled.
        let mut s = one_mesh(None, Affine::IDENTITY, -5.0);
        assert!(
            s.prepare(Size::new(100.0, 100.0), &mut missing)
                .visible
                .is_empty()
        );
    }

    /// S5A-04: the rebuild oracle's view tells meshes apart by their
    /// transformed vertices and their whole paint records.
    #[test]
    fn mesh_resolve_sees_gradients_and_transforms() {
        let g = |c0: u32, c1: u32| crate::chunk::GradientPaint {
            kind: crate::prim::gradient::LINEAR,
            geometry: [0.0, 0.0, 16.0, 0.0],
            to_gradient: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            stops: vec![(0.0, c0), (1.0, c1)],
        };
        let view = |s: &Scene| s.resolve(&|_| 0);
        let same = |a: &Scene, b: &Scene| {
            let (a, b) = (view(a), view(b));
            a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| x.close_to(y, 0.01))
        };
        let base = one_mesh(Some(g(0xFF00_00FF, 0x0000_FFFF)), Affine::IDENTITY, 20.0);
        let twin = one_mesh(Some(g(0xFF00_00FF, 0x0000_FFFF)), Affine::IDENTITY, 20.0);
        assert!(same(&base, &twin));
        let recolored = one_mesh(Some(g(0x00FF_00FF, 0xFFFF_FFFF)), Affine::IDENTITY, 20.0);
        assert!(!same(&base, &recolored), "another gradient");
        let mut moved = g(0xFF00_00FF, 0x0000_FFFF);
        moved.geometry = [0.0, 0.0, 8.0, 0.0];
        assert!(
            !same(&base, &one_mesh(Some(moved), Affine::IDENTITY, 20.0)),
            "another geometry"
        );
        // Rotated 180 degrees about the mesh's center: the same bounds,
        // other vertices.
        let spin = Affine::translate(20.2, 16.0)
            .mul(&Affine::rotate(std::f32::consts::PI))
            .mul(&Affine::translate(-20.2, -16.0));
        let a = one_mesh(None, Affine::IDENTITY, 20.0);
        let b = one_mesh(None, spin, 20.0);
        let (ra, rb) = (view(&a), view(&b));
        assert!((ra[0].bounds.origin.x - rb[0].bounds.origin.x).abs() < 0.01);
        assert!(!same(&a, &b), "rotated");
    }

    /// S5A-11 and S5A-12: the view records which triangle has which
    /// paint (not only the paint sequence), and where each vertex falls
    /// in gradient space (a same-sized device mesh from another local
    /// size samples the gradient elsewhere).
    #[test]
    fn mesh_resolve_sees_paint_runs_and_gradient_space() {
        let tris = |colors: [u32; 3]| {
            let mut s = Scene::new();
            let root = s.transforms.alloc(Affine::IDENTITY, NONE);
            s.transforms.set_order(vec![root]);
            let mut w = ChunkWriter::new();
            for (k, c) in colors.into_iter().enumerate() {
                let x = k as f32 * 10.0;
                let p = w.paint(c);
                assert!(w.mesh(&[[x, 0.0], [x + 8.0, 0.0], [x, 8.0]], &[0, 1, 2], p, false));
            }
            s.commit_chunk(0, &mut w);
            s.set_placement(
                0,
                Placement {
                    offset: [0.0, 0.0],
                    transform: root,
                    clip: NONE,
                },
            );
            s.set_order(vec![OrderItem::Chunk(0)], vec![]);
            s.transforms.derive();
            s
        };
        let same = |a: &Scene, b: &Scene| {
            let (a, b) = (a.resolve(&|_| 0), b.resolve(&|_| 0));
            a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| x.close_to(y, 0.01))
        };
        let (r, b) = (0xFF00_00FF, 0x0000_FFFF);
        assert!(same(&tris([r, b, b]), &tris([r, b, b])));
        assert!(!same(&tris([r, b, b]), &tris([r, r, b])), "paint runs");

        let g = crate::chunk::GradientPaint {
            kind: crate::prim::gradient::LINEAR,
            geometry: [0.0, 0.0, 64.0, 0.0],
            to_gradient: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            stops: vec![(0.0, r), (1.0, b)],
        };
        let square = |size: f32, scale: f32| {
            let mut s = Scene::new();
            let root = s.transforms.alloc(Affine::scale(scale, scale), NONE);
            s.transforms.set_order(vec![root]);
            let mut w = ChunkWriter::new();
            let slot = w.gradient(&g);
            let v = [[0.0, 0.0], [size, 0.0], [size, size], [0.0, size]];
            assert!(w.mesh(&v, &[0, 1, 2, 0, 2, 3], slot, true));
            s.commit_chunk(0, &mut w);
            s.set_placement(
                0,
                Placement {
                    offset: [0.0, 0.0],
                    transform: root,
                    clip: NONE,
                },
            );
            s.set_order(vec![OrderItem::Chunk(0)], vec![]);
            s.transforms.derive();
            s
        };
        assert!(same(&square(64.0, 1.0), &square(64.0, 1.0)));
        // S5A-18: a gradient mapped to a tiny space (1e-5 per unit)
        // whose end moves from 0.001 to 0.002 paints other colors: the
        // view compares the dimensionless parameter, not raw values.
        let tiny = |end: f32| {
            let mut s = Scene::new();
            let root = s.transforms.alloc(Affine::IDENTITY, NONE);
            s.transforms.set_order(vec![root]);
            let mut w = ChunkWriter::new();
            let slot = w.gradient(&crate::chunk::GradientPaint {
                kind: crate::prim::gradient::LINEAR,
                geometry: [0.0, 0.0, end, 0.0],
                to_gradient: [1e-5, 0.0, 0.0, 1e-5, 0.0, 0.0],
                stops: vec![(0.0, r), (1.0, b)],
            });
            let v = [[0.0, 0.0], [64.0, 0.0], [64.0, 64.0], [0.0, 64.0]];
            assert!(w.mesh(&v, &[0, 1, 2, 0, 2, 3], slot, true));
            s.commit_chunk(0, &mut w);
            s.set_placement(
                0,
                Placement {
                    offset: [0.0, 0.0],
                    transform: root,
                    clip: NONE,
                },
            );
            s.set_order(vec![OrderItem::Chunk(0)], vec![]);
            s.transforms.derive();
            s
        };
        assert!(same(&tiny(0.001), &tiny(0.001)));
        assert!(!same(&tiny(0.001), &tiny(0.002)), "tiny gradient space");
        // S5A-20: a hard stop (red to blue at 0.5) moved 0.6 px paints a
        // pixel in the other color: the view sees it; a move of 0.001 px
        // is within the tolerance.
        let hard = |shift: f32| {
            let mut s = Scene::new();
            let root = s.transforms.alloc(Affine::IDENTITY, NONE);
            s.transforms.set_order(vec![root]);
            let mut w = ChunkWriter::new();
            let slot = w.gradient(&crate::chunk::GradientPaint {
                kind: crate::prim::gradient::LINEAR,
                geometry: [0.0, 0.0, 64.0, 0.0],
                to_gradient: [1.0, 0.0, 0.0, 1.0, shift, 0.0],
                stops: vec![(0.0, r), (0.5, r), (0.5, b), (1.0, b)],
            });
            let v = [[0.0, 0.0], [64.0, 0.0], [64.0, 64.0], [0.0, 64.0]];
            assert!(w.mesh(&v, &[0, 1, 2, 0, 2, 3], slot, true));
            s.commit_chunk(0, &mut w);
            s.set_placement(
                0,
                Placement {
                    offset: [0.0, 0.0],
                    transform: root,
                    clip: NONE,
                },
            );
            s.set_order(vec![OrderItem::Chunk(0)], vec![]);
            s.transforms.derive();
            s
        };
        assert!(same(&hard(0.0), &hard(0.001)));
        assert!(!same(&hard(0.0), &hard(-0.6)), "hard stop moved");
        // S5B-08: a radial gradient under unequal axis scales (100 and
        // 0.01): a 0.006 shift of its center moves a hard stop 0.6 px.
        let radial = |shift: f32| {
            let mut s = Scene::new();
            let root = s.transforms.alloc(Affine::IDENTITY, NONE);
            s.transforms.set_order(vec![root]);
            let mut w = ChunkWriter::new();
            let slot = w.gradient(&crate::chunk::GradientPaint {
                kind: crate::prim::gradient::RADIAL,
                geometry: [0.32 + shift, 32.0, 0.3, 0.0],
                // Local to gradient: x / 100, y * 100.
                to_gradient: [0.01, 0.0, 0.0, 100.0, 0.0, 0.0],
                stops: vec![(0.0, r), (0.5, r), (0.5, b), (1.0, b)],
            });
            let v = [[0.0, 0.0], [64.0, 0.0], [64.0, 64.0], [0.0, 64.0]];
            assert!(w.mesh(&v, &[0, 1, 2, 0, 2, 3], slot, true));
            s.commit_chunk(0, &mut w);
            s.set_placement(
                0,
                Placement {
                    offset: [0.0, 0.0],
                    transform: root,
                    clip: NONE,
                },
            );
            s.set_order(vec![OrderItem::Chunk(0)], vec![]);
            s.transforms.derive();
            s
        };
        assert!(same(&radial(0.0), &radial(0.0)));
        assert!(!same(&radial(0.0), &radial(0.006)), "radial stop moved");
        let (a, b2) = (square(64.0, 1.0), square(32.0, 2.0));
        assert!(a.resolve(&|_| 0)[0].bounds == b2.resolve(&|_| 0)[0].bounds);
        assert!(!same(&a, &b2), "gradient space");
    }

    /// S5A-19: content between meshes that overlaps none of the open
    /// run draws before it, so the run (one multisampled layer) goes on;
    /// content that overlaps it ends the run.
    #[test]
    fn mesh_runs_continue_past_disjoint_content() {
        let build = |rect_x: f32| {
            let mut s = Scene::new();
            let root = s.transforms.alloc(Affine::IDENTITY, NONE);
            s.transforms.set_order(vec![root]);
            let mut w = ChunkWriter::new();
            let place = |s: &mut Scene, id| {
                s.set_placement(
                    id,
                    Placement {
                        offset: [0.0, 0.0],
                        transform: root,
                        clip: NONE,
                    },
                )
            };
            let white = w.paint(0xFFFF_FFFF);
            assert!(w.mesh(
                &[[0.0, 0.0], [16.0, 0.0], [0.0, 16.0]],
                &[0, 1, 2],
                white,
                false
            ));
            s.commit_chunk(0, &mut w);
            let green = w.paint(0x00FF_00FF);
            w.rect(Rect::new(rect_x, 0.0, 8.0, 8.0), 0.0, green);
            s.commit_chunk(1, &mut w);
            let white = w.paint(0xFFFF_FFFF);
            assert!(w.mesh(
                &[[16.0, 0.0], [16.0, 16.0], [0.0, 16.0]],
                &[0, 1, 2],
                white,
                false
            ));
            s.commit_chunk(2, &mut w);
            for id in 0..3 {
                place(&mut s, id);
            }
            s.set_order((0..3).map(OrderItem::Chunk).collect(), vec![]);
            let mut missing = Vec::new();
            s.prepare(Size::new(100.0, 100.0), &mut missing)
                .cmds
                .clone()
        };
        let kinds = |cmds: &[DrawCmd]| -> Vec<&'static str> {
            cmds.iter()
                .map(|c| match c {
                    DrawCmd::Rects { .. } => "rects",
                    DrawCmd::Glyphs { .. } => "glyphs",
                    DrawCmd::Paths { .. } => "paths",
                    DrawCmd::BeginLayer { .. } => "begin",
                    DrawCmd::EndLayer => "end",
                })
                .collect()
        };
        // S5B-02: a rect that starts in the pixel column where the first
        // mesh ends (x 16.3 against 16) shares a pixel: it ends the run.
        let k = kinds(&build(16.3));
        assert_eq!(k.iter().filter(|&&k| k == "begin").count(), 2);
        // Disjoint (x 40..48): the rect first, then one run of both.
        let k = kinds(&build(40.0));
        assert_eq!(k.first(), Some(&"rects"));
        assert_eq!(k.iter().filter(|&&k| k == "begin").count(), 1);
        // Overlapping the first mesh (x 2..10): two runs around it.
        let k = kinds(&build(2.0));
        assert_eq!(k.iter().filter(|&&k| k == "begin").count(), 2);
        assert_eq!(k.first(), Some(&"begin"));
    }

    /// S5A-05: a mesh with a partial triangle, an index out of range, or
    /// a non-finite vertex writes nothing; the next mesh is intact.
    #[test]
    fn malformed_meshes_are_refused() {
        let mut w = ChunkWriter::new();
        let red = w.paint(0xFF00_00FF);
        let tri = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        assert!(!w.mesh(&tri, &[0, 1], red, false));
        assert!(!w.mesh(&tri, &[0, 1, 3], red, false));
        assert!(!w.mesh(
            &[[0.0, 0.0], [f32::NAN, 0.0], [0.0, 1.0]],
            &[0, 1, 2],
            red,
            false
        ));
        assert!(w.is_empty());
        assert!(w.mesh(&tri, &[0, 1, 2], red, false));
        let mut s = Scene::new();
        s.commit_chunk(0, &mut w);
        let c = *s.chunk(0).unwrap();
        let v = c.path_vertices.start() as u32;
        assert_eq!(s.path_indices.get(c.path_indices), [v, v + 1, v + 2]);
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
