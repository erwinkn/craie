//! Host + layout -> retained scene.
//!
//! Each node owns one chunk (id = node id) holding its primitives in its
//! own border-box coordinates. What changes decides the work:
//!
//! - content (text, box geometry, surface payload, input state, size):
//!   the node's chunk is rebuilt;
//! - color only: one paint record is patched;
//! - layout position: the chunk's placement offset is patched;
//! - scroll offset or spatial transform: one transform record is patched;
//! - opacity: one layer's opacity is patched;
//! - structure, `display`, `overflow`, or draw topology (a transform
//!   record or opacity layer appearing): the draw order, the transform
//!   evaluation order, and the clip records are rebuilt by one walk.
//!
//! Spaces: the window root record maps logical units to device pixels.
//! A node with a transform owns a record for itself and its subtree; a
//! scroll container owns a record for its content. Every other node
//! draws in its nearest ancestor record's space at an offset, so a scroll
//! or a transform animation patches one record and no chunk.

use craie_core::dirty::DirtyQueue;
use craie_core::geom::{Affine, Point, Rect, Size};
use craie_core::rev::Rev;
use craie_scene::{ChunkWriter, ClipRecord, NONE, OrderItem, PaintSlot, Placement, RasterId};

use crate::host::{NodeFlags, NodeId, ROOT};
use crate::layout::{LayoutData, MeasuredText};
use crate::mutation::NodeKind;
use crate::text::paragraph::{SpanStyle, TextSpec, TextStyle};
use crate::ui::Ui;

/// Derived per-node scene bookkeeping, rebuilt by the tree walk.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NodeSpace {
    /// Transform record for the node and its subtree (transformed nodes).
    pub self_rec: u32,
    /// Transform record for the node's content (scroll containers).
    pub content_rec: u32,
    /// Clip record the node imposes on its children (overflow != visible).
    pub clip_rec: u32,
    /// Opacity layer (opacity < 1).
    pub layer: u32,
    /// Border-box origin in the parent's space (logical).
    pub origin: [f32; 2],
    /// Layout facts the chunk was built from: size, content offset,
    /// insets. A change rebuilds the chunk.
    pub built: [f32; 6],
    /// Walk epoch that last claimed this node's records.
    pub claimed: u32,
    /// Walk epoch that last visited the node (dedupes partial walks).
    pub walked: u32,
    /// The context the node was last visited with: its parent's child
    /// space, offset, and clip. Partial walks resume from it.
    pub ctx: Ctx,
}

impl Default for NodeSpace {
    fn default() -> NodeSpace {
        NodeSpace {
            self_rec: NONE,
            content_rec: NONE,
            clip_rec: NONE,
            layer: NONE,
            origin: [0.0; 2],
            built: [f32::NAN; 6],
            claimed: 0,
            walked: 0,
            ctx: Ctx {
                space: NONE,
                offset: [0.0; 2],
                clip: NONE,
            },
        }
    }
}

/// Scene-side state owned by `Ui`.
pub(crate) struct SceneSync {
    pub spaces: Vec<NodeSpace>,
    /// The window root record: logical units -> device pixels.
    pub root_rec: u32,
    /// (structure, clip) revisions the draw order was built from.
    pub order_revs: Option<(Rev, Rev)>,
    pub epoch: u32,
    /// Nodes that owned records after the last topology walk.
    pub owners: Vec<u32>,
    pub writer: ChunkWriter,
    pub missing: Vec<RasterId>,
    /// Scale the text chunks were emitted at.
    pub scale: f32,
    /// Reused id buffers (content queue, built chunks, paint queue): a
    /// drain swaps buffers with its queue, so frames allocate nothing.
    pub scratch: Vec<u32>,
    pub built: Vec<u32>,
    pub paint_ids: Vec<u32>,
    pub spatial_ids: Vec<u32>,
    /// Snap-at-rest records in motion, and the clock time each last
    /// moved (by record id; NaN at rest).
    pub moving: Vec<u32>,
    pub moved_at: Vec<f64>,
    /// Nodes whose chunk build waits until their box nears the viewport.
    pub deferred: DirtyQueue,
    /// Visibility inputs the deferred set was last checked against:
    /// viewport (device px), world revision, placement revision.
    pub checked: Option<(Size, u64, u64)>,
}

impl SceneSync {
    pub fn new(scene: &mut craie_scene::Scene) -> SceneSync {
        let root_rec = scene.transforms.alloc(Affine::IDENTITY, NONE);
        SceneSync {
            spaces: Vec::new(),
            root_rec,
            order_revs: None,
            epoch: 0,
            owners: Vec::new(),
            writer: ChunkWriter::new(),
            missing: Vec::new(),
            scale: f32::NAN,
            scratch: Vec::new(),
            built: Vec::new(),
            paint_ids: Vec::new(),
            spatial_ids: Vec::new(),
            moving: Vec::new(),
            moved_at: Vec::new(),
            deferred: DirtyQueue::new(),
            checked: None,
        }
    }
}

/// Walk context: the space children draw in.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ctx {
    space: u32,
    offset: [f32; 2],
    clip: u32,
}

/// Outputs of a topology walk.
#[derive(Default)]
struct Topo {
    order: Vec<OrderItem>,
    layers: Vec<f32>,
    records: Vec<u32>,
    clips: Vec<ClipRecord>,
}

fn layout_key(d: &LayoutData) -> [f32; 6] {
    [
        d.rect.size.width,
        d.rect.size.height,
        d.content[0],
        d.content[1],
        d.insets[0],
        d.insets[1],
    ]
}

/// A node's own transform, applied about its border-box center.
fn self_local(origin: [f32; 2], t: Affine, size: Size) -> Affine {
    Affine::translate(origin[0], origin[1])
        .mul(&t.about(Point::new(size.width / 2.0, size.height / 2.0)))
}

impl Ui {
    /// Starts or continues motion of snap-at-rest record `rec`: it stops
    /// snapping until it has rested for `SETTLE_SECS`.
    fn mark_moving(&mut self, rec: u32) {
        let i = rec as usize;
        if self.sync.moved_at.len() <= i {
            self.sync.moved_at.resize(i + 1, f64::NAN);
        }
        if self.sync.moved_at[i].is_nan() {
            self.sync.moving.push(rec);
        }
        self.sync.moved_at[i] = self.time;
        self.scene.transforms.set_snap(rec, false);
    }

    /// Writes the local matrix of a snap-at-rest record; a change is
    /// motion.
    fn set_moving_local(&mut self, rec: u32, local: Affine) {
        if self.scene.transforms.get(rec).local != local {
            self.scene.transforms.set_local(rec, local);
            self.mark_moving(rec);
        }
    }

    /// Frees a snap-at-rest record and forgets its motion.
    fn free_moving(&mut self, rec: u32) {
        self.scene.transforms.free(rec);
        if let Some(t) = self.sync.moved_at.get_mut(rec as usize)
            && !t.is_nan()
        {
            *t = f64::NAN;
            self.sync.moving.retain(|&r| r != rec);
        }
    }

    /// Snaps records that rested for `SETTLE_SECS`. Returns whether any
    /// did.
    pub(crate) fn settle_moving(&mut self) -> bool {
        let mut any = false;
        let mut i = 0;
        while i < self.sync.moving.len() {
            let rec = self.sync.moving[i];
            let at = &mut self.sync.moved_at[rec as usize];
            // A microsecond of slack: a host timer set for exactly the
            // settle time must find the space settled, not spin.
            if self.time - *at >= crate::ui::SETTLE_SECS - 1e-6 {
                *at = f64::NAN;
                self.sync.moving.swap_remove(i);
                self.scene.transforms.set_snap(rec, true);
                any = true;
            } else {
                i += 1;
            }
        }
        any
    }

    fn space_mut(&mut self, id: NodeId) -> &mut NodeSpace {
        let i = id.index();
        if self.sync.spaces.len() <= i {
            self.sync.spaces.resize(i + 1, NodeSpace::default());
        }
        &mut self.sync.spaces[i]
    }

    /// Brings the scene up to date with the host and layout. `layout_ran`
    /// says whether this frame's layout pass changed anything; `viewport`
    /// is logical.
    pub(crate) fn sync_scene(&mut self, viewport: Size, layout_ran: bool) {
        self.scene.begin_frame();
        self.host.refresh_orders();
        // A frame past a settle time snaps the rested spaces itself.
        self.settle_moving();
        self.scene.scale = self.scale;
        self.scene.clear = crate::scene::Color(self.clear);

        // A scale change re-rasterizes every glyph chunk, re-tessellates
        // every vector chunk (its tolerance is in device px), replans
        // every image (its decode size is in device px), and remaps
        // the root record; other geometry in logical units is unaffected.
        if self.sync.scale != self.scale {
            self.sync.scale = self.scale;
            self.scene
                .transforms
                .set_local(self.sync.root_rec, Affine::scale(self.scale, self.scale));
            for i in 0..self.host.slot_count() {
                let id = NodeId(i as u32);
                if matches!(
                    self.host.kind(id),
                    Some(NodeKind::Text | NodeKind::Input | NodeKind::Vector | NodeKind::Image)
                ) {
                    self.host.dirty.content.push(id.0);
                }
            }
        }

        // Topology changes rebuild the order in one full walk. Otherwise
        // only nodes whose layout moved (their subtree) or resized (the
        // node alone) are revisited, then spatial changes patch records.
        let revs = (self.host.revs.structure, self.host.revs.clip);
        if self.sync.order_revs != Some(revs) {
            self.walk_tree(true);
            self.sync.order_revs = Some(revs);
            self.layouts.moved.clear();
            self.layouts.resized.clear();
        } else {
            if layout_ran {
                self.walk_changed();
            }
            self.patch_spatial();
        }
        self.host.dirty.spatial.clear();

        // Chunk content is demand-driven: a node builds when its box is
        // within half a viewport of the screen; farther nodes wait in
        // `deferred` and build in the frame they come into range.
        self.scene.transforms.derive();
        let px = Size::new(viewport.width * self.scale, viewport.height * self.scale);
        let region = Rect::new(
            -px.width / 2.0,
            -px.height / 2.0,
            px.width * 2.0,
            px.height * 2.0,
        );
        // Deferred nodes are rechecked only when a visibility input moved.
        let key = (
            px,
            self.scene.transforms.world_rev,
            self.scene.placement_rev,
        );
        if self.sync.checked != Some(key) {
            self.sync.checked = Some(key);
            for &id in self.sync.deferred.as_slice() {
                self.host.dirty.content.push(id);
            }
            self.sync.deferred.clear();
        }
        let mut ids = std::mem::take(&mut self.sync.scratch);
        self.host.dirty.content.drain_into(&mut ids);
        let mut built = std::mem::take(&mut self.sync.built);
        built.clear();
        for &id in &ids {
            let node = NodeId(id);
            if self.host.is_live(node) && !self.near(node, &region) {
                // Stale content must never draw: drop it until rebuilt.
                self.scene.free_chunk(id);
                self.sync.deferred.push(id);
                continue;
            }
            self.build_chunk(node);
            built.push(id);
        }
        let mut paint_ids = std::mem::take(&mut self.sync.paint_ids);
        self.host.dirty.paint.drain_into(&mut paint_ids);
        for &id in &paint_ids {
            if !built.contains(&id) {
                self.patch_paint(NodeId(id));
            }
        }
        self.sync.scratch = ids;
        self.sync.built = built;
        self.sync.paint_ids = paint_ids;

        let mut missing = std::mem::take(&mut self.sync.missing);
        missing.clear();
        self.scene.prepare(px, &mut missing);
        if !missing.is_empty() {
            self.text.ensure_resident(&missing, &mut self.scene.atlas);
            self.images.ensure_resident(&missing, &mut self.scene.atlas);
        }
        self.sync.missing = missing;
    }

    /// One pre-order walk. Always recomputes placements, record locals,
    /// and clip rects from layout. With `topo`, also rebuilds the draw
    /// order, the layer table, the transform evaluation order, and the
    /// clip records, and frees records nobody claims.
    fn walk_tree(&mut self, topo: bool) {
        self.sync.epoch = self.sync.epoch.wrapping_add(1);
        let mut out = Topo::default();
        if topo {
            out.records.push(self.sync.root_rec);
        }
        let roots: Vec<NodeId> = self.host.paint_order(ROOT).into_owned();
        let ctx = Ctx {
            space: self.sync.root_rec,
            offset: [0.0; 2],
            clip: NONE,
        };
        for root in roots {
            self.visit(root, ctx, topo, &mut out);
        }
        if topo {
            // Free the records of nodes that no longer claim them.
            let epoch = self.sync.epoch;
            let owners = std::mem::take(&mut self.sync.owners);
            for id in owners {
                let s = self.sync.spaces[id as usize];
                if s.claimed == epoch {
                    continue;
                }
                let s = &mut self.sync.spaces[id as usize];
                let recs = [s.self_rec, s.content_rec];
                s.self_rec = NONE;
                s.content_rec = NONE;
                for rec in recs {
                    if rec != NONE {
                        self.free_moving(rec);
                    }
                }
            }
            for (i, s) in self.sync.spaces.iter_mut().enumerate() {
                // A node outside the drawn tree holds no layer and no clip:
                // those tables were just rebuilt without it.
                if s.claimed != epoch {
                    s.layer = NONE;
                    s.clip_rec = NONE;
                }
                if s.self_rec != NONE || s.content_rec != NONE {
                    self.sync.owners.push(i as u32);
                }
            }
            self.scene.transforms.set_order(out.records);
            self.scene.clips.set_all(out.clips);
            self.scene.set_order(out.order, out.layers);
        }
    }

    /// Revisits the nodes layout moved (with their subtrees) or resized
    /// (alone), resuming from each node's stored context.
    fn walk_changed(&mut self) {
        self.sync.epoch = self.sync.epoch.wrapping_add(1);
        let epoch = self.sync.epoch;
        let mut out = Topo::default();
        for id in self.layouts.moved.take() {
            let s = self
                .sync
                .spaces
                .get(id as usize)
                .copied()
                .unwrap_or_default();
            if s.walked != epoch && s.ctx.space != NONE {
                self.visit(NodeId(id), s.ctx, false, &mut out);
            }
        }
        for id in self.layouts.resized.take() {
            let s = self
                .sync
                .spaces
                .get(id as usize)
                .copied()
                .unwrap_or_default();
            if s.walked != epoch && s.ctx.space != NONE {
                self.visit_node(NodeId(id), s.ctx, false, &mut out);
            }
        }
    }

    fn visit(&mut self, id: NodeId, ctx: Ctx, topo: bool, out: &mut Topo) {
        let Some(child_ctx) = self.visit_node(id, ctx, topo, out) else {
            return;
        };
        // Children draw in paint order (`order.rs`), sorted before the
        // frame: a sorted parent lends its order to the walk, uncopied;
        // most parents keep tree order.
        debug_assert!(!self.host.order_flags(id).contains(NodeFlags::ORDER));
        if let Some(order) = self.host.orders.get_mut(&id.0).map(std::mem::take) {
            for &child in &order {
                self.visit(child, child_ctx, topo, out);
            }
            self.host.orders.insert(id.0, order);
        } else {
            for i in 0..self.host.child_count(id) {
                let child = self.host.child_at(id, i);
                self.visit(child, child_ctx, topo, out);
            }
        }
        if topo && self.sync.spaces[id.index()].layer != NONE {
            out.order.push(OrderItem::EndLayer);
        }
    }

    /// Updates one node's placement, records, and clip; returns the
    /// context its children draw in (`None` when it draws nothing).
    fn visit_node(&mut self, id: NodeId, ctx: Ctx, topo: bool, out: &mut Topo) -> Option<Ctx> {
        self.host.node(id)?;
        let style = self.host.style(id);
        if style.display() == taffy::Display::None {
            return None;
        }
        let overflow = style.overflow();
        let clips =
            overflow.x != taffy::Overflow::Visible || overflow.y != taffy::Overflow::Visible;
        let scrolls =
            overflow.x == taffy::Overflow::Scroll || overflow.y == taffy::Overflow::Scroll;
        let data = self.layouts.data(id);
        let spatial = self.host.spatial[id.index()];
        let origin = [
            ctx.offset[0] + data.rect.origin.x,
            ctx.offset[1] + data.rect.origin.y,
        ];
        let epoch = self.sync.epoch;
        let key = layout_key(&data);

        // Record topology is decided by the walk that rebuilds the order.
        let mut s = *self.space_mut(id);
        s.walked = epoch;
        s.ctx = ctx;
        if topo {
            s.claimed = epoch;
            if spatial.transformed() {
                if s.self_rec == NONE {
                    // A transformed subtree moves by fractions: its
                    // placement stays fractional while it moves and snaps
                    // at rest (`settle`). A new record starts moving.
                    s.self_rec = self.scene.transforms.alloc(Affine::IDENTITY, NONE);
                    self.mark_moving(s.self_rec);
                }
            } else if s.self_rec != NONE {
                self.free_moving(s.self_rec);
                s.self_rec = NONE;
            }
            if scrolls {
                if s.content_rec == NONE {
                    // Scroll content moves by fractions (wheel deltas,
                    // commands): fractional while it moves, so motion
                    // never steps; snapped at rest, so text is crisp.
                    s.content_rec = self.scene.transforms.alloc(Affine::IDENTITY, NONE);
                    self.mark_moving(s.content_rec);
                }
            } else if s.content_rec != NONE {
                self.free_moving(s.content_rec);
                s.content_rec = NONE;
            }
            s.layer = if spatial.layered() {
                out.layers.push(spatial.opacity);
                out.order
                    .push(OrderItem::BeginLayer(out.layers.len() as u32 - 1));
                out.layers.len() as u32 - 1
            } else {
                NONE
            };
        } else if s.layer != NONE {
            self.scene.set_layer_opacity(s.layer, spatial.opacity);
        }
        s.origin = origin;
        if s.built != key {
            self.host.dirty.content.push(id.0);
        }

        // The node's own space.
        let (space, offset) = if s.self_rec != NONE {
            self.scene.transforms.set_parent(s.self_rec, ctx.space);
            self.set_moving_local(
                s.self_rec,
                self_local(origin, spatial.transform, data.rect.size),
            );
            if topo {
                out.records.push(s.self_rec);
            }
            (s.self_rec, [0.0, 0.0])
        } else {
            (ctx.space, origin)
        };
        self.scene.set_placement(
            id.0,
            Placement {
                offset,
                transform: space,
                clip: ctx.clip,
            },
        );
        if topo {
            out.order.push(OrderItem::Chunk(id.0));
        }

        // Children: clipped to the padding box, scrolled by the content
        // record.
        let mut child_clip = ctx.clip;
        if clips {
            let (rect, radius, open) = self.clip_shape(id, offset, &data);
            if topo {
                s.clip_rec = out.clips.len() as u32;
                out.clips.push(ClipRecord {
                    rect,
                    radius,
                    transform: space,
                    parent: ctx.clip,
                    open,
                });
            } else if s.clip_rec != NONE {
                self.scene.clips.set_rect(s.clip_rec, rect, radius, open);
            }
            child_clip = s.clip_rec;
        } else if topo {
            s.clip_rec = NONE;
        }
        let child_ctx = if s.content_rec != NONE {
            self.scene.transforms.set_parent(s.content_rec, space);
            let [sx, sy] = spatial.scroll;
            self.set_moving_local(
                s.content_rec,
                Affine::translate(offset[0] - sx, offset[1] - sy),
            );
            if topo {
                out.records.push(s.content_rec);
            }
            Ctx {
                space: s.content_rec,
                offset: [0.0, 0.0],
                clip: child_clip,
            }
        } else {
            Ctx {
                space,
                offset,
                clip: child_clip,
            }
        };
        *self.space_mut(id) = s;
        Some(child_ctx)
    }

    /// Scroll, transform, and opacity changes without topology or layout
    /// changes: patch the node's records and layer in place.
    fn patch_spatial(&mut self) {
        let mut ids = std::mem::take(&mut self.sync.spatial_ids);
        ids.clear();
        ids.extend_from_slice(self.host.dirty.spatial.as_slice());
        for &id in &ids {
            let node = NodeId(id);
            if !self.host.is_live(node) {
                continue;
            }
            let Some(s) = self.sync.spaces.get(node.index()).copied() else {
                continue;
            };
            let spatial = self.host.spatial[node.index()];
            let size = self.layouts.data(node).rect.size;
            let mut base = s.origin;
            if s.self_rec != NONE {
                self.set_moving_local(s.self_rec, self_local(s.origin, spatial.transform, size));
                base = [0.0, 0.0];
            }
            if s.content_rec != NONE {
                let [sx, sy] = spatial.scroll;
                self.set_moving_local(s.content_rec, Affine::translate(base[0] - sx, base[1] - sy));
            }
            if s.layer != NONE {
                self.scene.set_layer_opacity(s.layer, spatial.opacity);
            }
        }
        self.sync.spatial_ids = ids;
    }

    /// Whether a node's border box, in device px, intersects `region`.
    /// Nodes outside the last walk (display: none) count as near: their
    /// chunk is cheap and never drawn.
    fn near(&self, id: NodeId, region: &Rect) -> bool {
        let p = self.scene.placement(id.0);
        let d = self.layouts.data(id);
        let local = Rect::new(
            p.offset[0],
            p.offset[1],
            d.rect.size.width,
            d.rect.size.height,
        );
        let b = self.scene.transforms.world(p.transform).map_rect(&local);
        // Overflowing content (text wider than its box) stays in range
        // through the half-viewport margin.
        b.intersects(region) || (b.size.width == 0.0 && b.size.height == 0.0)
    }

    /// The clip a node imposes on its children, in its own space at
    /// `offset`: its padding box rounded by its corner radius, open along
    /// an axis whose overflow is visible.
    pub(crate) fn clip_shape(
        &self,
        id: NodeId,
        offset: [f32; 2],
        data: &LayoutData,
    ) -> (Rect, f32, [bool; 2]) {
        let style = self.host.style(id);
        let open = [
            style.overflow().x == taffy::Overflow::Visible,
            style.overflow().y == taffy::Overflow::Visible,
        ];
        let rect = Rect::new(
            offset[0] + data.clip_box.origin.x,
            offset[1] + data.clip_box.origin.y,
            data.clip_box.size.width,
            data.clip_box.size.height,
        );
        let radius = match self.host.kind(id) {
            Some(k) if k.has_box() => self.host.paint[id.index()].radius,
            _ => 0.0,
        };
        (rect, radius, open)
    }

    /// Patches the paint records of a chunk whose colors changed.
    fn patch_paint(&mut self, id: NodeId) {
        let Some(kind) = self.host.kind(id) else {
            return;
        };
        if kind == NodeKind::Text {
            let spans = &self.host.paragraphs[id.index()].spans;
            for (i, s) in spans.iter().enumerate() {
                let c = self.host.span_color(id, s);
                self.scene.set_paint(id.0, PaintSlot(i as u32), c);
            }
        } else {
            let p = self.host.paint[id.index()];
            self.scene.set_paint(id.0, PaintSlot(0), p.fill);
            self.scene.set_paint(id.0, PaintSlot(1), p.border_color);
            match kind {
                NodeKind::Input => {
                    // Slots 2 text, 3 placeholder (half alpha), 5 caret.
                    let c = self.host.current_color(id);
                    self.scene.set_paint(id.0, PaintSlot(2), c);
                    self.scene
                        .set_paint(id.0, PaintSlot(3), (c & !0xFF) | ((c & 0xFF) / 2));
                    self.scene.set_paint(id.0, PaintSlot(5), c);
                }
                NodeKind::Vector => self.patch_vector_paint(id),
                _ => {}
            }
        }
    }

    /// Rebuilds one node's chunk from its current state, or frees it when
    /// the node is gone.
    fn build_chunk(&mut self, id: NodeId) {
        let Some(kind) = self.host.kind(id) else {
            self.scene.free_chunk(id.0);
            return;
        };
        let data = self.layouts.data(id);
        self.space_mut(id).built = layout_key(&data);
        // A radius change reshapes the clip this node imposes.
        let clip_rec = self.sync.spaces[id.index()].clip_rec;
        if clip_rec != NONE && clip_rec < self.scene.clips.len() as u32 {
            let offset = self.scene.placement(id.0).offset;
            let (rect, radius, open) = self.clip_shape(id, offset, &data);
            self.scene.clips.set_rect(clip_rec, rect, radius, open);
        }
        let mut w = std::mem::take(&mut self.sync.writer);
        w.clear();
        if kind.has_box() {
            let p = self.host.paint[id.index()];
            // Slots 0 and 1 are always the fill and border: color patches
            // address them directly.
            let fill = w.paint(p.fill);
            let border = w.paint(p.border_color);
            let has_border = p.border_width > 0.0 && p.border_color & 0xFF != 0;
            if p.fill & 0xFF != 0 || has_border {
                w.rect_bordered(
                    Rect::new(0.0, 0.0, data.rect.size.width, data.rect.size.height),
                    p.radius,
                    fill,
                    has_border.then_some(border),
                    p.border_width,
                );
            }
        }
        match kind {
            NodeKind::Text => self.build_text(id, &data, &mut w),
            NodeKind::Input => self.build_input(id, &data, &mut w),
            NodeKind::Surface => self.build_surface(id, &data, &mut w),
            NodeKind::Vector => self.build_vector(id, &data, &mut w),
            NodeKind::Image => self.build_image(id, &data, &mut w),
            NodeKind::View | NodeKind::List => {}
        }
        self.scene.commit_chunk(id.0, &mut w);
        self.sync.writer = w;
    }

    fn build_text(&mut self, id: NodeId, data: &LayoutData, w: &mut ChunkWriter) {
        let Some(p) = self.host.paragraph(id) else {
            return;
        };
        for s in &p.spans {
            w.paint(self.host.span_color(id, s));
        }
        // The content width the layout wrapped this leaf at. A retained
        // paragraph shaped for another width is stale: rewrap now.
        let content_w = (data.rect.size.width - data.insets[0]).max(0.0);
        let slot = id.index();
        // Layout may skip measuring a leaf whose size is fixed, so the
        // TEXT flag (content or metrics changed) is checked here too.
        let text_dirty = self
            .host
            .node(id)
            .is_some_and(|n| n.flags.contains(crate::host::NodeFlags::TEXT));
        let retained = self.texts.get(slot).and_then(Option::as_ref);
        if !text_dirty
            && let Some(m) = retained
            && m.wrap_bits != content_w.to_bits()
        {
            // Another width: rewrap, no shaping.
            let m = self.texts[slot].as_mut().unwrap();
            self.text.rewrap(&mut m.layout, Some(content_w));
            m.wrap_bits = content_w.to_bits();
        } else if text_dirty || retained.is_none() {
            let layout = crate::layout::shape_paragraph(&mut self.text, p, Some(content_w));
            if slot >= self.texts.len() {
                self.texts.resize_with(slot + 1, || None);
            }
            self.texts[slot] = Some(MeasuredText {
                layout,
                wrap_bits: content_w.to_bits(),
            });
            if let Some(n) = self.host.node_mut(id) {
                n.flags.clear(crate::host::NodeFlags::TEXT);
            }
        }
        // Span decorations draw with the glyphs (rects in the chunk).
        let decorations: Vec<u8> = self
            .host
            .paragraph(id)
            .map(|p| p.spans.iter().map(|s| s.decoration).collect())
            .unwrap_or_default();
        let m = self.texts[slot].as_ref().unwrap();
        // A text selection's highlight, under the glyphs, from the
        // placements (paint slot after the span colors).
        if let Some(range) = self.highlight_of(id) {
            let fill = w.paint(crate::selection::SELECTION_COLOR);
            for (x, y, rw, rh) in m.layout.selection_rects(range) {
                w.rect(
                    Rect::new(data.content[0] + x, data.content[1] + y, rw, rh),
                    0.0,
                    fill,
                );
            }
        }
        self.text.emit_paragraph(
            &m.layout,
            Point::new(data.content[0], data.content[1]),
            self.scale,
            None,
            &decorations,
            &mut self.scene.atlas,
            w,
        );
    }

    /// Input chunk: box, selection highlights, text or placeholder, caret.
    /// Slots: 0 fill, 1 border, 2 text, 3 placeholder, 4 selection, 5 caret.
    fn build_input(&mut self, id: NodeId, data: &LayoutData, w: &mut ChunkWriter) {
        let focused = self.focus == Some(id);
        let content_w = (data.rect.size.width - data.insets[0]).max(0.0);
        let (cx, cy) = (data.content[0], data.content[1]);
        let scale = self.scale;
        let color = self.host.current_color(id);
        let Some(state) = self.inputs.get_mut(id.0) else {
            return;
        };
        state.set_width(content_w);
        let text_slot = w.paint(color);
        let ph_slot = w.paint((color & !0xFF) | ((color & 0xFF) / 2));
        let sel_slot = w.paint(state.selection_color);
        let caret_slot = w.paint(color);
        debug_assert_eq!(text_slot, PaintSlot(2));
        let empty = state.editor.raw_text().is_empty();
        state.editor.refresh(&mut self.text);
        if focused {
            for (x, y, rw, rh) in state.editor.selection_rects() {
                w.rect(
                    Rect::new(cx + x, cy + y, rw.max(0.0), rh.max(0.0)),
                    0.0,
                    sel_slot,
                );
            }
        }
        let origin = Point::new(cx, cy);
        if empty && !state.placeholder.is_empty() {
            let bits = content_w.to_bits();
            match &mut state.placeholder_layout {
                Some((b, p)) if *b != bits => {
                    self.text.rewrap(p, Some(content_w));
                    *b = bits;
                }
                Some(_) => {}
                None => {
                    let spans = [SpanStyle {
                        start: 0,
                        style: TextStyle {
                            size: state.font_size,
                            ..TextStyle::default()
                        },
                    }];
                    let spec = TextSpec {
                        text: &state.placeholder,
                        spans: &spans,
                    };
                    let p = self.text.layout_text(&spec, Some(content_w));
                    state.placeholder_layout = Some((bits, p));
                }
            }
            let p = &state.placeholder_layout.as_ref().unwrap().1;
            self.text.emit_paragraph(
                p,
                origin,
                scale,
                Some(ph_slot),
                &[],
                &mut self.scene.atlas,
                w,
            );
        } else {
            let p = state.editor.layout();
            self.text.emit_paragraph(
                p,
                origin,
                scale,
                Some(text_slot),
                &[],
                &mut self.scene.atlas,
                w,
            );
        }
        if focused && let Some((x, y, _, h)) = state.editor.caret_rect(1.5) {
            w.rect(Rect::new(cx + x, cy + y, 1.5, h.max(0.0)), 0.0, caret_slot);
        }
    }

    /// Surface chunk: the kind's native painter emits quads in content-box
    /// coordinates. Runs only when the payload, parameters, or size
    /// change.
    fn build_surface(&mut self, id: NodeId, data: &LayoutData, w: &mut ChunkWriter) {
        let Some(spec) = self.host.surfaces.get(&id.0) else {
            return;
        };
        let Some(painter) = self.surface_painters.get_mut(&spec.kind) else {
            return;
        };
        let content = Rect::new(
            data.content[0],
            data.content[1],
            (data.rect.size.width - data.insets[0]).max(0.0),
            (data.rect.size.height - data.insets[1]).max(0.0),
        );
        self.surface_scratch.clear();
        painter(spec, content, &mut self.surface_scratch);
        for q in self.surface_scratch.drain(..) {
            let fill = w.paint(q.color);
            let border = (q.border_w > 0.0).then(|| w.paint(q.border_color));
            w.rect_bordered(
                Rect::new(q.x, q.y, q.w, q.h),
                q.radius,
                fill,
                border,
                q.border_w,
            );
        }
    }
}
