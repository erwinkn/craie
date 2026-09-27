//! The app-facing facade: host + layout + text + scene.
//!
//! `Ui` owns every piece of retained state on the native side. The cycle
//! is strictly pull-based:
//!
//! ```text
//! transaction --execute--> host stores (revisions, dirty queues)
//!             --layout-->  Taffy over host storage (Parley measures text)
//!             --paint-->   scene
//! ```
//!
//! Nothing runs while idle: `execute` only writes stores and queues work;
//! `render` runs only when `needs_paint` says a mutation can change
//! pixels.

use std::collections::HashMap;
use std::time::Instant;

use crate::events::{Event, Mods, UiEvent};
use craie_core::geom::{Affine, Point};

use crate::geom::{Rect, Size};
use crate::host::{Host, NodeId, Revs};
use crate::input::{Inputs, KeyAction};
use crate::layout::{self, Layouts, MeasuredText};
use crate::mutation::{Command, NodeKind, Transaction};
use crate::scene::Scene;
use crate::scene_sync::SceneSync;
use crate::surface::{self, Quad, SurfacePainter};
use crate::text::TextEngine;
use crate::wire::{self, WireError};

pub struct Ui {
    pub host: Host,
    pub layouts: Layouts,
    pub text: TextEngine,
    /// Retained owned paragraphs for TEXT nodes, indexed by node id.
    pub(crate) texts: Vec<Option<MeasuredText>>,
    /// Scratch for a paragraph's spans in host family indices.
    pub(crate) span_scratch: Vec<crate::mutation::TextSpan>,
    /// Editing state for INPUT-kind nodes, keyed by node id.
    pub inputs: Inputs,
    /// Surface painters, keyed by surface kind.
    pub(crate) surface_painters: HashMap<u32, SurfacePainter>,
    /// Reusable surface painter output.
    pub(crate) surface_scratch: Vec<Quad>,
    /// Focused node (key events and input editing).
    pub(crate) focus: Option<NodeId>,
    /// Node under the pointer: drives enter/leave synthesis.
    pub(crate) hover: Option<NodeId>,
    /// Node that grabbed the pointer on the last button press.
    pub(crate) pressed: Option<NodeId>,
    /// That press is of the primary button (the `pressed` state bit).
    pub(crate) pressed_primary: bool,
    /// Where the pointer last was, while it is in the window.
    pub(crate) last_pointer: Option<(f32, f32)>,
    /// A hover recheck waits for moving spaces to settle.
    pub(crate) hover_stale: bool,
    /// State styles (`states.rs`).
    pub(crate) states: crate::states::States,
    /// Scratch for tree walks.
    pub(crate) node_scratch: Vec<NodeId>,
    /// Last primary press (time, node, x, y) for double-click detection.
    pub(crate) last_click: Option<(Instant, NodeId, f32, f32)>,
    /// The text selection (`selection.rs`), whether a press is dragging
    /// it, and each text node's highlighted range (what chunk builds
    /// draw).
    pub(crate) text_selection: Option<crate::selection::TextSelection>,
    pub(crate) selecting: bool,
    pub(crate) selection_highlight: Vec<(NodeId, std::ops::Range<u32>)>,
    /// Generations of the selection's domain, anchor, and focus nodes
    /// when it was set, and the host revisions it was last checked at.
    pub(crate) selection_generations: [u16; 3],
    pub(crate) selection_revs: crate::host::Revs,
    /// Vector nodes' tessellated meshes (`vector.rs`).
    pub(crate) vector_meshes: crate::vector::VectorCache,
    /// Running animations (`animation.rs`).
    pub(crate) animations: crate::animation::Animations,
    /// Events accumulated for the JS side since the last `take_events`.
    pub(crate) pending_events: Vec<UiEvent>,
    /// Set when anything observable to assistive tech changed.
    pub(crate) a11y_stale: bool,
    /// A repaint is owed for reasons revisions do not record (focus,
    /// caret, scroll, editing, resize).
    pub(crate) force_paint: bool,
    /// Revisions at the last completed paint.
    painted: Option<Revs>,
    /// Viewport of the last layout pass; a change relayouts.
    pub(crate) laid_out: Option<Size>,
    /// A layout pass ran since the last paint: geometry must resync.
    relayout: bool,
    /// Scene-side bookkeeping (spaces, records, scratch).
    pub(crate) sync: SceneSync,
    /// `ScrollTo` commands waiting for the layout after their batch.
    pub(crate) pending_scrolls: Vec<(NodeId, f32, f32)>,
    /// Each node's hit-test reach, by id (`reach.rs`).
    pub(crate) reach: Vec<crate::reach::Bounds>,
    /// Display scale factor (physical / logical).
    pub scale: f32,
    /// Background clear color, 0xRRGGBBAA.
    pub clear: u32,
    /// Last applied transaction sequence, for acknowledgements.
    pub seq: u64,
    /// The host's clock, seconds. Drives settling (`settle`); the host
    /// sets it before each callback with `set_time`.
    pub(crate) time: f64,
    pub(crate) scene: Scene,
}

/// A moving space (scroll content, a transformed subtree) is at rest
/// once it has not moved for this long, in seconds. At rest it snaps to
/// the device-pixel grid; while moving it keeps fractional placement.
pub const SETTLE_SECS: f64 = 0.1;

impl Ui {
    pub fn new(scale: f32) -> Ui {
        let mut surface_painters: HashMap<u32, SurfacePainter> = HashMap::new();
        surface_painters.insert(surface::kind::BARS, Box::new(surface::paint_bars));
        let mut scene = Scene::default();
        let sync = SceneSync::new(&mut scene);
        Ui {
            host: Host::new(),
            layouts: Layouts::new(),
            text: TextEngine::new(),
            span_scratch: Vec::new(),
            texts: Vec::new(),
            inputs: Inputs::default(),
            surface_painters,
            surface_scratch: Vec::new(),
            focus: None,
            hover: None,
            pressed: None,
            pressed_primary: false,
            last_pointer: None,
            hover_stale: false,
            states: Default::default(),
            node_scratch: Vec::new(),
            last_click: None,
            text_selection: None,
            selecting: false,
            selection_highlight: Vec::new(),
            selection_generations: [0; 3],
            selection_revs: Default::default(),
            animations: Default::default(),
            vector_meshes: Default::default(),
            pending_events: Vec::new(),
            a11y_stale: true,
            force_paint: true,
            painted: None,
            laid_out: None,
            relayout: false,
            sync,
            pending_scrolls: Vec::new(),
            reach: Vec::new(),
            scale,
            clear: 0x1415_18FF,
            seq: 0,
            time: 0.0,
            scene,
        }
    }

    /// Sets the clock (seconds, monotonic). Settling compares motion
    /// times against it.
    pub fn set_time(&mut self, secs: f64) {
        self.time = secs;
    }

    /// Snaps the spaces that have rested for `SETTLE_SECS` (the settle
    /// signal). Returns whether any did; they then need a paint.
    pub fn settle(&mut self) -> bool {
        let settled = self.settle_moving();
        self.force_paint |= settled;
        // The node under a still pointer, once nothing moves.
        let rehovered = self.hover_stale && self.rehover();
        settled || rehovered
    }

    /// When the next moving space comes to rest (clock seconds), if any
    /// is moving. The host wakes then and calls `settle`.
    pub fn next_settle(&self) -> Option<f64> {
        self.sync
            .moving
            .iter()
            .map(|&rec| self.sync.moved_at[rec as usize])
            .min_by(f64::total_cmp)
            .map(|t| t + SETTLE_SECS)
    }

    /// Decodes and applies one CRW2 transaction. Returns its seq. A
    /// transaction that fails validation changes nothing.
    pub fn apply(&mut self, buf: &[u8]) -> Result<u64, WireError> {
        let txn = wire::decode(buf)?;
        self.execute(&txn)?;
        self.refresh_selection();
        self.a11y_stale = true;
        Ok(txn.seq)
    }

    /// Validates and applies a direct-API transaction (same executor as
    /// `apply`).
    pub fn apply_txn(&mut self, txn: &Transaction<'_>) -> Result<(), WireError> {
        self.execute(txn)?;
        self.refresh_selection();
        self.a11y_stale = true;
        Ok(())
    }

    /// Registers a native painter for surface `kind`, replacing any
    /// previous one. Surfaces of an unregistered kind paint only their
    /// box.
    pub fn register_surface(&mut self, kind: u32, painter: SurfacePainter) {
        self.surface_painters.insert(kind, painter);
        // Surfaces of this kind drew with the previous painter (or none).
        for (&id, s) in &self.host.surfaces {
            if s.kind == kind {
                self.host.dirty.content.push(id);
            }
        }
        self.force_paint = true;
    }

    /// UI commands arriving in a transaction.
    pub(crate) fn command(&mut self, id: NodeId, cmd: &Command<'_>) {
        match cmd {
            Command::Focus => self.set_focus(Some(id)),
            Command::Blur => {
                if self.focus == Some(id) {
                    self.set_focus(None);
                }
            }
            Command::SetText(text) => {
                self.inputs.set_text(id.0, text);
                // JS set it: no `onChangeText` echo.
                self.inputs.mark_notified(id.0);
                self.host.revs.text_content.bump();
                self.host.mark_layout(id);
                self.host.dirty.content.push(id.0);
                self.host.dirty.semantic.push(id.0);
            }
            Command::ScrollTo(x, y) => {
                // The batch may change the extent: clamp against the
                // layout that follows it.
                self.pending_scrolls.push((id, *x, *y));
                self.force_paint = true;
            }
            Command::InsertText(text) => {
                // The focused input, if it is this node or inside it: the
                // user's paste, answered by JS, so it notifies.
                if let Some(f) = self.focus
                    && self.host.kind(f) == Some(NodeKind::Input)
                    && self.ancestors(f).any(|n| n == id)
                {
                    self.inputs
                        .act(&mut self.text, f.0, &KeyAction::Replace(text.to_string()));
                    self.input_changed(f);
                    self.emit_change(f);
                }
            }
            Command::WriteClipboard(text) => self.inputs.clipboard.set(text),
        }
    }

    /// Events queued for the JS side since the last call.
    pub fn take_events(&mut self) -> Vec<UiEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// An outbound event for `id`, stamped with the node's generation so
    /// the JS side can drop events for a recycled id.
    pub(crate) fn event(&self, kind: u8, id: NodeId) -> UiEvent {
        let mut e = UiEvent::new(kind, id.0);
        e.generation = self.host.node(id).map_or(0, |n| n.generation);
        e
    }

    /// True once after mutations that assistive tech can observe;
    /// `take_a11y_stale` consumes the flag.
    pub fn take_a11y_stale(&mut self) -> bool {
        self.host.dirty.semantic.clear();
        std::mem::take(&mut self.a11y_stale)
    }

    /// Applies an assistive-technology action request on the UI thread.
    pub fn a11y_action(&mut self, request: &accesskit::ActionRequest) {
        use accesskit::{Action, ActionData};
        let Some(id) = crate::a11y::nid(request.target_node) else {
            return;
        };
        if self.host.node(id).is_none() {
            return;
        }
        match request.action {
            Action::Focus => self.set_focus(Some(id)),
            Action::Blur => {
                if self.focus == Some(id) {
                    self.set_focus(None);
                }
            }
            // Equivalent to a tap at the node's center: runs the real
            // pointer path so listeners and capture behave identically.
            Action::Click => {
                let r = self.abs_rect(id);
                let x = r.origin.x + r.size.width / 2.0;
                let y = r.origin.y + r.size.height / 2.0;
                self.dispatch(&Event::PointerDown {
                    x,
                    y,
                    button: crate::events::Button::Primary,
                    mods: Mods::default(),
                });
                self.dispatch(&Event::PointerUp {
                    x,
                    y,
                    button: crate::events::Button::Primary,
                });
            }
            Action::ScrollUp => {
                self.scroll_by(id, 0.0, -self.page_step(id));
            }
            Action::ScrollDown => {
                self.scroll_by(id, 0.0, self.page_step(id));
            }
            Action::ScrollLeft => {
                self.scroll_by(id, -self.page_step(id), 0.0);
            }
            Action::ScrollRight => {
                self.scroll_by(id, self.page_step(id), 0.0);
            }
            Action::SetScrollOffset => {
                if let Some(ActionData::SetScrollOffset(p)) = &request.data {
                    self.scroll_to(id, p.x as f32, p.y as f32);
                }
            }
            Action::ReplaceSelectedText => {
                if let Some(ActionData::Value(v)) = &request.data
                    && self.host.kind(id) == Some(NodeKind::Input)
                {
                    self.inputs.set_text(id.0, v);
                    self.emit_change(id);
                    self.host.mark_layout(id);
                    self.host.dirty.content.push(id.0);
                }
            }
            Action::ScrollIntoView => self.scroll_into_view(id),
            _ => {}
        }
    }

    /// One "page" scroll step: the node's own extent, or 48pt.
    fn page_step(&self, id: NodeId) -> f32 {
        let h = self.layouts.data(id).rect.size.height;
        if h > 0.0 { h } else { 48.0 }
    }

    /// Adjusts each scrollable ancestor so `id`'s bounds sit inside its
    /// clip box.
    fn scroll_into_view(&mut self, id: NodeId) {
        let r = self.abs_rect(id);
        let mut cur = self.host.parent(id);
        while cur.is_node() {
            let style = self.host.style(cur);
            let overflow = style.overflow();
            let sx = overflow.x == taffy::Overflow::Scroll;
            let sy = overflow.y == taffy::Overflow::Scroll;
            if sx || sy {
                let c = self.abs_rect(cur);
                let cur_off = self.host.spatial[cur.index()].scroll;
                let mut next = cur_off;
                if sy {
                    if r.origin.y < c.origin.y {
                        next[1] -= c.origin.y - r.origin.y;
                    } else if r.origin.y + r.size.height > c.origin.y + c.size.height {
                        next[1] += r.origin.y + r.size.height - (c.origin.y + c.size.height);
                    }
                }
                if sx {
                    if r.origin.x < c.origin.x {
                        next[0] -= c.origin.x - r.origin.x;
                    } else if r.origin.x + r.size.width > c.origin.x + c.size.width {
                        next[0] += r.origin.x + r.size.width - (c.origin.x + c.size.width);
                    }
                }
                if next != cur_off {
                    self.scroll_to(cur, next[0], next[1]);
                }
            }
            cur = self.host.parent(cur);
        }
    }

    /// Sets a scroll offset, clamped to the layout extent and to the axes
    /// the node's style scrolls; returns the applied offset when it
    /// changed.
    pub fn scroll_to(&mut self, id: NodeId, x: f32, y: f32) -> Option<[f32; 2]> {
        self.host.node(id)?;
        let style = self.host.style(id);
        let overflow = style.overflow();
        let x_ok = overflow.x == taffy::Overflow::Scroll;
        let y_ok = overflow.y == taffy::Overflow::Scroll;
        if !x_ok && !y_ok {
            return None;
        }
        let extent = self.layouts.data(id).scroll_extent;
        let cur = self.host.spatial[id.index()].scroll;
        let next = [
            if x_ok {
                x.clamp(0.0, extent[0])
            } else {
                cur[0]
            },
            if y_ok {
                y.clamp(0.0, extent[1])
            } else {
                cur[1]
            },
        ];
        if cur == next {
            return None;
        }
        self.host.spatial[id.index()].scroll = next;
        self.host.touch(id);
        self.host.revs.transform.bump();
        self.host.dirty.spatial.push(id.0);
        self.a11y_stale = true;
        Some(next)
    }

    pub(crate) fn scroll_by(&mut self, id: NodeId, dx: f32, dy: f32) -> Option<[f32; 2]> {
        let cur = self.host.spatial.get(id.index())?.scroll;
        self.scroll_to(id, cur[0] + dx, cur[1] + dy)
    }

    /// Current scroll offset of a node (zero when it does not scroll).
    pub fn scroll_offset(&self, id: NodeId) -> [f32; 2] {
        self.host
            .spatial
            .get(id.index())
            .map_or([0.0; 2], |s| s.scroll)
    }

    /// The focused node, if any.
    pub fn focused(&self) -> Option<NodeId> {
        self.focus
    }

    /// True when applied mutations or native interaction can change
    /// pixels.
    pub fn needs_paint(&self) -> bool {
        // Queued work counts even when no revision moved (commands,
        // native edits).
        let d = &self.host.dirty;
        if self.force_paint
            || self.animating()
            || !d.layout.is_empty()
            || !d.content.is_empty()
            || !d.paint.is_empty()
            || !d.spatial.is_empty()
            || !self.pending_scrolls.is_empty()
            || !self.states.queue.is_empty()
        {
            return true;
        }
        let Some(p) = self.painted else { return true };
        let r = self.host.revs;
        // Semantic-only changes (roles, labels, listeners) draw nothing.
        Revs {
            semantic: p.semantic,
            ..r
        } != p
    }

    /// Drops every layout cache and forces a repaint: used on resize,
    /// where wrap widths change without any mutation arriving.
    pub fn invalidate_layout(&mut self) {
        self.layouts.clear_all_caches(self.host.slot_count());
        self.laid_out = None;
        self.force_paint = true;
    }

    /// Recomputes layout for every root at `size` (logical units) and
    /// rebuilds the scene. Cheap on a clean tree, but callers should
    /// still gate on `needs_paint`.
    pub fn render(&mut self, size: Size) -> &Scene {
        self.update_env(size);
        self.restyle();
        self.run_animations(size);
        self.layout(size);
        self.sync_lists(size);
        // The frame's geometry is final: refresh the hit-test index here,
        // so the next event does not pay for it (`dispatch` still
        // refreshes whatever changed since).
        self.refresh_reach();
        // Geometry that assistive technology reports moved in this frame
        // (a layout pass, an animation sample): the tree is stale even
        // with no commit.
        if !self.layouts.moved.is_empty()
            || !self.layouts.resized.is_empty()
            || !self.host.dirty.spatial.is_empty()
        {
            self.a11y_stale = true;
        }
        let moved = self.relayout || !self.host.dirty.spatial.is_empty();
        self.paint(size);
        // Geometry moved under a still pointer: hover follows. A change
        // restyles, and the host sees `needs_paint` and draws again.
        if moved || self.hover_stale {
            self.rehover();
        }
        &self.scene
    }

    /// Layout phase only: Taffy over the host at `size` (logical units).
    /// Runs only when layout inputs, structure, text metrics, or the
    /// viewport changed; returns whether it ran.
    pub fn layout(&mut self, size: Size) -> bool {
        if self.host.dirty.layout.is_empty() && self.laid_out == Some(size) {
            self.apply_pending_scrolls();
            return false;
        }
        self.layout_timed(size);
        true
    }

    fn apply_pending_scrolls(&mut self) {
        for (id, x, y) in std::mem::take(&mut self.pending_scrolls) {
            if self.host.is_live(id) {
                self.scroll_to(id, x, y);
            }
        }
    }

    /// Unconditional layout pass, instrumented: returns (root-layout ms,
    /// finalize ms).
    pub fn layout_timed(&mut self, size: Size) -> (f64, f64) {
        self.laid_out = Some(size);
        self.relayout = true;
        let total = self.compute_layout(size);
        // Anchored scrollers first; explicit scroll commands win.
        self.restore_anchors();
        self.apply_pending_scrolls();
        // Content that shrank below a scroll offset pulls the offset back
        // into range, as browsers do.
        for id in self.layouts.extents.take() {
            let node = NodeId(id);
            if self.host.is_live(node) {
                let [x, y] = self.host.spatial[node.index()].scroll;
                self.scroll_to(node, x, y);
            }
        }
        total
    }

    /// Taffy over every root at `size`, and nothing else: no anchoring,
    /// no scroll commands, no offset clamping (the animation probe runs
    /// it; `layout_timed` adds those).
    pub(crate) fn compute_layout(&mut self, size: Size) -> (f64, f64) {
        let mut total = (0.0, 0.0);
        self.layouts.passes += 1;
        let roots: Vec<NodeId> = self.host.children(crate::host::ROOT).to_vec();
        layout::invalidate(&mut self.host, &mut self.layouts);
        for root in roots {
            let (c, r) = layout::compute_timed(
                &mut self.host,
                &mut self.layouts,
                &mut self.text,
                &mut self.texts,
                &mut self.inputs,
                root,
                size,
            );
            total.0 += c;
            total.1 += r;
        }
        total
    }

    /// Paint phase only: brings the scene up to date with the current
    /// layout. `viewport` is logical, for culling.
    pub fn paint(&mut self, viewport: Size) {
        let layout_ran = std::mem::take(&mut self.relayout);
        if self.selection_revs != self.host.revs {
            self.refresh_selection();
        }
        self.sync_scene(viewport, layout_ran);
        self.force_paint = false;
        self.painted = Some(self.host.revs);
    }

    /// Cost counters across layout, text, and the scene.
    pub fn counters(&self) -> craie_core::counters::Counters {
        let mut c = self.scene.counters;
        c.layout_passes = self.layouts.passes;
        c.layout_nodes = self.layouts.cache_misses;
        c.shapes = self.text.shapes;
        c.rasters = self.text.cache.stats.rasters;
        c.transforms_written = self.scene.transforms.writes;
        c.copied_bytes = self.host.copied_bytes;
        c
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
    }

    /// Retained paragraph of a text node (valid after `render`).
    pub fn text_layout(&self, id: NodeId) -> Option<&crate::text::paragraph::Paragraph> {
        self.texts
            .get(id.index())
            .and_then(|m| m.as_ref())
            .map(|m| &m.layout)
    }

    /// Maps a node's border-box coordinates to window coordinates
    /// (logical): layout positions, scroll offsets, and transforms of the
    /// node and every ancestor.
    pub fn node_to_window(&self, id: NodeId) -> Affine {
        let mut chain: Vec<NodeId> = Vec::new();
        let mut cur = id;
        while cur.is_node() && self.host.is_live(cur) {
            chain.push(cur);
            cur = self.host.parent(cur);
        }
        let mut m = Affine::IDENTITY;
        let mut parent: Option<NodeId> = None;
        for &n in chain.iter().rev() {
            if let Some(p) = parent {
                let [sx, sy] = self.scroll_offset_if_scrolls(p);
                m = m.mul(&Affine::translate(-sx, -sy));
            }
            let d = self.layouts.data(n);
            let t = self.host.spatial[n.index()].transform;
            m = m
                .mul(&Affine::translate(d.rect.origin.x, d.rect.origin.y))
                .mul(&t.about(Point::new(
                    d.rect.size.width / 2.0,
                    d.rect.size.height / 2.0,
                )));
            parent = Some(n);
        }
        m
    }

    /// The scroll offset a container applies to its children.
    pub(crate) fn scroll_offset_if_scrolls(&self, id: NodeId) -> [f32; 2] {
        let style = self.host.style(id);
        let overflow = style.overflow();
        if overflow.x == taffy::Overflow::Scroll || overflow.y == taffy::Overflow::Scroll {
            self.host.spatial[id.index()].scroll
        } else {
            [0.0; 2]
        }
    }

    /// The node's window-space bounding box (logical), transforms and
    /// scroll offsets applied.
    pub fn abs_rect(&self, id: NodeId) -> Rect {
        let d = self.layouts.data(id);
        self.node_to_window(id).map_rect(&Rect::new(
            0.0,
            0.0,
            d.rect.size.width,
            d.rect.size.height,
        ))
    }
}
