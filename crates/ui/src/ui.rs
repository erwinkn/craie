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
use crate::geom::{Rect, Size};
use crate::host::{Host, NodeId, Revs};
use crate::input::Inputs;
use crate::layout::{self, Layouts, MeasuredText};
use crate::mutation::{Command, NodeKind, Transaction};
use crate::scene::{Color, Scene};
use crate::surface::{self, Quad, SurfacePainter};
use crate::text::TextEngine;
use crate::text::parley::Layout as ParleyLayout;
use crate::wire::{self, WireError};

pub struct Ui {
    pub host: Host,
    pub layouts: Layouts,
    pub text: TextEngine,
    /// Retained Parley layouts for TEXT nodes, indexed by node id.
    pub(crate) texts: Vec<Option<MeasuredText>>,
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
    /// Last primary press (time, node, x, y) for double-click detection.
    pub(crate) last_click: Option<(Instant, NodeId, f32, f32)>,
    /// Events accumulated for the JS side since the last `take_events`.
    pub(crate) pending_events: Vec<UiEvent>,
    /// Set when anything observable to assistive tech changed.
    pub(crate) a11y_stale: bool,
    /// A repaint is owed for reasons revisions do not record (focus,
    /// caret, scroll, editing, resize).
    pub(crate) force_paint: bool,
    /// Revisions at the last completed paint.
    painted: Option<Revs>,
    /// Display scale factor (physical / logical).
    pub scale: f32,
    /// Background clear color, 0xRRGGBBAA.
    pub clear: u32,
    /// Last applied transaction sequence, for acknowledgements.
    pub seq: u64,
    pub(crate) scene: Scene,
}

impl Ui {
    pub fn new(scale: f32) -> Ui {
        let mut surface_painters: HashMap<u32, SurfacePainter> = HashMap::new();
        surface_painters.insert(surface::kind::BARS, Box::new(surface::paint_bars));
        Ui {
            host: Host::new(),
            layouts: Layouts::new(),
            text: TextEngine::new(),
            texts: Vec::new(),
            inputs: Inputs::default(),
            surface_painters,
            surface_scratch: Vec::new(),
            focus: None,
            hover: None,
            pressed: None,
            last_click: None,
            pending_events: Vec::new(),
            a11y_stale: true,
            force_paint: true,
            painted: None,
            scale,
            clear: 0x1415_18FF,
            seq: 0,
            scene: Scene::default(),
        }
    }

    /// Decodes and applies one CRW2 transaction. Returns its seq. A
    /// transaction that fails validation changes nothing.
    pub fn apply(&mut self, buf: &[u8]) -> Result<u64, WireError> {
        let txn = wire::decode(buf)?;
        self.execute(&txn)?;
        self.a11y_stale = true;
        Ok(txn.seq)
    }

    /// Validates and applies a direct-API transaction (same executor as
    /// `apply`).
    pub fn apply_txn(&mut self, txn: &Transaction<'_>) -> Result<(), WireError> {
        self.execute(txn)?;
        self.a11y_stale = true;
        Ok(())
    }

    /// Registers a native painter for surface `kind`, replacing any
    /// previous one. Surfaces of an unregistered kind paint only their
    /// box.
    pub fn register_surface(&mut self, kind: u32, painter: SurfacePainter) {
        self.surface_painters.insert(kind, painter);
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
                self.host.mark_layout(id);
                self.host.dirty.content.push(id.0);
                self.host.dirty.semantic.push(id.0);
            }
            Command::ScrollTo(x, y) => {
                self.scroll_to(id, *x, *y);
            }
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
            let sx = style.overflow.x == taffy::Overflow::Scroll;
            let sy = style.overflow.y == taffy::Overflow::Scroll;
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
        let x_ok = style.overflow.x == taffy::Overflow::Scroll;
        let y_ok = style.overflow.y == taffy::Overflow::Scroll;
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
        if self.force_paint {
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
        self.force_paint = true;
    }

    /// Recomputes layout for every root at `size` (logical units) and
    /// rebuilds the scene. Cheap on a clean tree, but callers should
    /// still gate on `needs_paint`.
    pub fn render(&mut self, size: Size) -> &Scene {
        self.layout(size);
        self.paint(size);
        &self.scene
    }

    /// Layout phase only: Taffy over the host at `size` (logical units).
    pub fn layout(&mut self, size: Size) {
        self.layout_timed(size);
    }

    /// `layout` instrumented: returns (root-layout ms, finalize ms).
    pub fn layout_timed(&mut self, size: Size) -> (f64, f64) {
        let mut total = (0.0, 0.0);
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

    /// Paint phase only: rebuilds the scene at the current layout.
    /// `viewport` is the logical viewport for culling.
    pub fn paint(&mut self, viewport: Size) {
        // Advances the glyph cache's LRU clock: glyphs emitted this pass
        // are ineligible for eviction.
        self.text.cache.begin_frame();
        self.paint_impl(viewport);
        self.force_paint = false;
        self.painted = Some(self.host.revs);
        self.host.dirty.content.clear();
        self.host.dirty.paint.clear();
        self.host.dirty.spatial.clear();
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Retained text layout for a node (valid after `render`).
    pub fn text_layout(&self, id: NodeId) -> Option<&ParleyLayout<Color>> {
        self.texts
            .get(id.index())
            .and_then(|m| m.as_ref())
            .map(|m| &m.layout)
    }

    /// The node's absolute border box (logical), scroll offsets applied.
    pub fn abs_rect(&self, id: NodeId) -> Rect {
        let data = self.layouts.data(id);
        let mut x = data.rect.origin.x;
        let mut y = data.rect.origin.y;
        let mut cur = id;
        loop {
            let p = self.host.parent(cur);
            if !p.is_node() {
                break;
            }
            let pd = self.layouts.data(p);
            let [sx, sy] = self.host.spatial[p.index()].scroll;
            x += pd.rect.origin.x - sx;
            y += pd.rect.origin.y - sy;
            cur = p;
        }
        Rect::new(x, y, data.rect.size.width, data.rect.size.height)
    }
}
