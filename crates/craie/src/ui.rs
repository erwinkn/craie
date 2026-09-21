//! The app-facing facade: host + styles + layout + text + scene.
//!
//! `Ui` owns every piece of retained state on the native side. The cycle
//! is strictly pull-based:
//!
//! ```text
//! wire bytes --apply--> host mutations (dirty flags)
//!             --layout--> Taffy over host storage (Parley measures text)
//!             --paint-->  flat Scene (quad + glyph rows)
//! ```
//!
//! Nothing runs while idle: `apply` only flips bits, `render` is invoked
//! only when `needs_paint` says a mutation can change pixels.

use std::collections::HashMap;
use std::time::Instant;

use parley::Layout as ParleyLayout;
use parley::style::StyleProperty;

use crate::custom::{CustomData, Painter, Quad};
use crate::events::{self, Event, Key, KeyInput, Mods, UiEvent, mask, out_kind};
use crate::geom::{Point, Rect, Size};
use crate::host::{Host, NodeId, NodeKind, ROOT, StyleId};
use crate::input::{Inputs, KeyAction};
use crate::layout::{self, EmittedText, Layouts, MeasuredText};
use crate::scene::{Color, Instance, Scene};
use crate::text::{ParagraphSpec, TextEngine};
use crate::wire::{self, Command, Op, Txn, WireError};

/// A clip rect in logical coordinates, optionally rounded.
#[derive(Clone, Copy)]
struct Clip {
    min: [f32; 2],
    max: [f32; 2],
    radius: f32,
}

pub struct Ui {
    pub host: Host,
    pub layouts: Layouts,
    pub text: TextEngine,
    /// Retained Parley layouts for TEXT nodes, indexed by node id.
    texts: Vec<Option<MeasuredText>>,
    /// Editing state for INPUT-kind nodes, keyed by node id.
    pub inputs: Inputs,
    /// Scroll offsets (logical points) for scrollable nodes.
    scroll: HashMap<u32, [f32; 2]>,
    /// Payloads for CUSTOM-kind nodes, keyed by node id.
    custom: HashMap<u32, CustomData>,
    /// Registered custom painters, keyed by payload tag.
    painters: HashMap<u32, Painter>,
    /// Reusable painter output buffer — no alloc per node per paint.
    custom_scratch: Vec<Quad>,
    /// Focused node (for key events and input editing).
    focus: Option<NodeId>,
    /// Node under the pointer — drives enter/leave synthesis.
    hover: Option<NodeId>,
    /// Node that grabbed the pointer on the last button press (drag
    /// capture for text selection).
    pressed: Option<NodeId>,
    /// Last primary press (time, node, x, y) for double-click detection.
    last_click: Option<(Instant, NodeId, f32, f32)>,
    /// Events accumulated for the JS side since the last `take_events`.
    pending_events: Vec<UiEvent>,
    /// Accessibility names (`accessibilityLabel`), keyed by node id.
    labels: HashMap<u32, Box<str>>,
    /// Set when anything observable to assistive tech changed.
    a11y_stale: bool,
    /// Display scale factor (physical / logical).
    pub scale: f32,
    /// Background clear color, 0xRRGGBBAA.
    pub clear: u32,
    /// Last applied transaction sequence, for acknowledgements.
    pub seq: u64,
    scene: Scene,
}

impl Ui {
    pub fn new(scale: f32) -> Ui {
        Ui {
            host: Host::new(),
            layouts: Layouts::new(),
            text: TextEngine::new(),
            texts: Vec::new(),
            inputs: Inputs::default(),
            scroll: HashMap::new(),
            custom: HashMap::new(),
            painters: HashMap::new(),
            custom_scratch: Vec::new(),
            focus: None,
            hover: None,
            pressed: None,
            last_click: None,
            pending_events: Vec::new(),
            labels: HashMap::new(),
            a11y_stale: true,
            scale,
            clear: 0x1415_18FF,
            seq: 0,
            scene: Scene::default(),
        }
    }

    /// Decodes and applies one wire transaction. Returns its seq.
    /// A transaction that fails validation changes nothing.
    pub fn apply(&mut self, buf: &[u8]) -> Result<u64, WireError> {
        let txn = wire::decode(buf)?;
        self.apply_txn(&txn)?;
        Ok(txn.seq)
    }

    /// Applies an already-decoded transaction.
    pub fn apply_txn(&mut self, txn: &Txn<'_>) -> Result<(), WireError> {
        txn.apply(&mut self.host, &mut self.layouts)?;
        // Slot reuse must drop the previous occupant's text layout.
        for op in &txn.ops {
            match op {
                Op::Create { id, .. } => {
                    let slot = *id as usize;
                    if slot < self.texts.len() {
                        self.texts[slot] = None;
                    }
                    // A recycled slot drops the previous occupant's
                    // editing and scroll state.
                    self.inputs.remove(*id);
                    self.scroll.remove(id);
                    self.custom.remove(id);
                    self.labels.remove(id);
                }
                Op::Remove { id } => {
                    let slot = *id as usize;
                    if slot < self.texts.len() {
                        self.texts[slot] = None;
                    }
                    self.inputs.remove(*id);
                    self.scroll.remove(id);
                    self.custom.remove(id);
                    self.labels.remove(id);
                    let gone = Some(NodeId(*id));
                    if self.focus == gone {
                        self.focus = None;
                    }
                    if self.hover == gone {
                        self.hover = None;
                    }
                    if self.pressed == gone {
                        self.pressed = None;
                    }
                }
                Op::InputProps {
                    id,
                    font_size,
                    color,
                    placeholder,
                    multiline,
                } => {
                    self.inputs
                        .configure(*id, *font_size, *color, placeholder, *multiline);
                    self.host.mark_dirty(NodeId(*id), crate::host::NodeFlags::LAYOUT);
                }
                Op::Command { id, cmd } => self.command(NodeId(*id), cmd),
                Op::Custom {
                    id,
                    tag,
                    data,
                    text,
                } => {
                    self.custom.insert(
                        *id,
                        CustomData {
                            tag: *tag,
                            data: *data,
                            text: (*text).into(),
                        },
                    );
                    self.host.force_paint();
                }
                Op::Label { id, text } => {
                    if text.is_empty() {
                        self.labels.remove(id);
                    } else {
                        self.labels.insert(*id, (*text).into());
                    }
                }
                _ => {}
            }
        }
        self.seq = txn.seq;
        self.a11y_stale = true;
        Ok(())
    }

    /// Registers the painter for custom payload `tag`. Custom nodes with
    /// an unregistered tag paint only their ViewRow background.
    pub fn register_painter(&mut self, tag: u32, painter: Painter) {
        self.painters.insert(tag, painter);
        self.host.force_paint();
    }

    /// UI commands arriving over the wire.
    fn command(&mut self, id: NodeId, cmd: &Command<'_>) {
        match cmd {
            Command::Focus => self.set_focus(Some(id)),
            Command::Blur => {
                if self.focus == Some(id) {
                    self.set_focus(None);
                }
            }
            Command::SetInputText(text) => {
                self.inputs.set_text(id.0, text);
                self.host.mark_dirty(id, crate::host::NodeFlags::LAYOUT);
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

    /// The node's `accessibilityLabel`, if any.
    pub(crate) fn a11y_label(&self, id: NodeId) -> Option<&str> {
        self.labels.get(&id.0).map(|s| &**s)
    }

    /// True once after mutations that assistive tech can observe;
    /// `take_a11y_stale` consumes the flag.
    pub fn take_a11y_stale(&mut self) -> bool {
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
                    && self.host.node(id).map(|n| n.kind()) == Some(NodeKind::INPUT)
                {
                    self.inputs.set_text(id.0, v);
                    self.emit_change(id);
                    self.host.mark_dirty(id, crate::host::NodeFlags::LAYOUT);
                }
            }
            Action::ScrollIntoView => self.scroll_into_view(id),
            _ => {}
        }
    }

    /// One "page" scroll step — the node's own extent, or 48pt.
    fn page_step(&self, id: NodeId) -> f32 {
        let h = self.layouts.data(id).rect.size.height;
        if h > 0.0 { h } else { 48.0 }
    }

    /// Adjusts each scrollable ancestor so `id`'s bounds sit inside its
    /// clip box.
    fn scroll_into_view(&mut self, id: NodeId) {
        let r = self.abs_rect(id);
        let mut cur = self.host.parent(id);
        while !cur.is_nil() && cur != NodeId::DETACHED {
            let Some(node) = self.host.node(cur) else { break };
            let style = self.layouts.style(StyleId(node.style));
            let sx = style.overflow.x == taffy::Overflow::Scroll;
            let sy = style.overflow.y == taffy::Overflow::Scroll;
            if sx || sy {
                let c = self.abs_rect(cur);
                let cur_off = self.scroll.get(&cur.0).copied().unwrap_or([0.0, 0.0]);
                let mut next = cur_off;
                if sy {
                    if r.origin.y < c.origin.y {
                        next[1] -= c.origin.y - r.origin.y;
                    } else if r.origin.y + r.size.height > c.origin.y + c.size.height {
                        next[1] +=
                            r.origin.y + r.size.height - (c.origin.y + c.size.height);
                    }
                }
                if sx {
                    if r.origin.x < c.origin.x {
                        next[0] -= c.origin.x - r.origin.x;
                    } else if r.origin.x + r.size.width > c.origin.x + c.size.width {
                        next[0] +=
                            r.origin.x + r.size.width - (c.origin.x + c.size.width);
                    }
                }
                if next != cur_off {
                    self.scroll_to(cur, next[0], next[1]);
                }
            }
            cur = self.host.parent(cur);
        }
    }

    /// The focused node, if any.
    pub fn focused(&self) -> Option<NodeId> {
        self.focus
    }

    /// True when applied mutations can change pixels.
    pub fn needs_paint(&self) -> bool {
        self.host.paint_dirty()
    }

    /// Drops every layout cache and forces a repaint — used on resize,
    /// where wrap widths change without any mutation arriving.
    pub fn invalidate_layout(&mut self) {
        self.layouts.clear_all_caches(self.host.slot_count());
        self.host.force_paint();
    }

    /// Recomputes layout for every root at `size` (logical units) and
    /// rebuilds the flat scene. Cheap on a clean tree — Taffy caches skip
    /// untouched subtrees — but callers should still gate on
    /// `needs_paint`.
    pub fn render(&mut self, size: Size) -> &Scene {
        self.layout(size);
        self.paint(size);
        self.host.clear_paint_dirty();
        &self.scene
    }

    /// Layout phase only: Taffy over the host at `size` (logical units).
    /// Exposed so benchmarks time layout independently of paint.
    pub fn layout(&mut self, size: Size) {
        // Layout never mutates topology, so indexed iteration is safe.
        for i in 0..self.host.child_count(ROOT) {
            let root = self.host.child_at(ROOT, i);
            layout::compute(
                &mut self.host,
                &mut self.layouts,
                &mut self.text,
                &mut self.texts,
                &mut self.inputs,
                root,
                size,
            );
        }
    }

    /// `layout` instrumented: returns (root-layout ms, rounding ms).
    pub fn layout_timed(&mut self, size: Size) -> (f64, f64) {
        let mut total = (0.0, 0.0);
        for i in 0..self.host.child_count(ROOT) {
            let root = self.host.child_at(ROOT, i);
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

    /// Paint phase only: rebuilds the flat scene at the current layout.
    /// `viewport` is the logical viewport for culling.
    pub fn paint(&mut self, viewport: Size) {
        // Advances the glyph cache's LRU clock — glyphs emitted this
        // pass are ineligible for eviction.
        self.text.cache.begin_frame();
        self.paint_impl(viewport);
        self.host.clear_paint_dirty();
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Retained text layout for a node (valid after `render`).
    pub fn text_layout(&self, id: NodeId) -> Option<&ParleyLayout<Color>> {
        self.texts
            .get(id.0 as usize)
            .and_then(|m| m.as_ref())
            .map(|m| &m.layout)
    }

    /// Intersects `clip` with `r` (absolute logical); keeps the tighter
    /// radius.
    fn clip_intersect(clip: Clip, r: Rect, radius: f32) -> Clip {
        let min = [
            clip.min[0].max(r.origin.x),
            clip.min[1].max(r.origin.y),
        ];
        let max = [
            clip.max[0].min(r.origin.x + r.size.width),
            clip.max[1].min(r.origin.y + r.size.height),
        ];
        Clip {
            min,
            max,
            radius: radius.max(if max[0] > min[0] && max[1] > min[1] {
                // The tighter rect owns the visible corner curve.
                if r.origin.x >= clip.min[0]
                    && r.origin.y >= clip.min[1]
                    && r.origin.x + r.size.width <= clip.max[0]
                    && r.origin.y + r.size.height <= clip.max[1]
                {
                    radius
                } else {
                    clip.radius.min(radius)
                }
            } else {
                0.0
            }),
        }
    }

    /// Stamps the active clip onto a range of emitted instances.
    /// Free function so callers can hold disjoint `Ui` field borrows.
    fn apply_clip(
        items: &mut Vec<Instance>,
        start: usize,
        clip: Option<Clip>,
        scale: f32,
    ) {
        let Some(clip) = clip else { return };
        for inst in &mut items[start..] {
            inst.clip_min = [clip.min[0] * scale, clip.min[1] * scale];
            inst.clip_max = [clip.max[0] * scale, clip.max[1] * scale];
            inst.params[2] = clip.radius * scale;
            if clip.radius > 0.0 {
                inst.flags |= Instance::FLAG_ROUND_CLIP;
            }
        }
    }

    /// Depth-first paint pass: one ordered instance stream. Origins
    /// accumulate through each parent's border box minus its scroll
    /// offset (logical units) and convert to physical pixels at emission.
    /// A clip rect flows down the stack: `overflow` clip/hidden/scroll
    /// constrains the subtree to the node's padding box.
    fn paint_impl(&mut self, viewport: Size) {
        self.scene.items.clear();
        self.scene.clear = Some(Color(self.clear));

        // (node, parent border-box origin, clip). Children are pushed in
        // reverse so pops yield document pre-order — which is also
        // painter order.
        let mut stack: Vec<(NodeId, f32, f32, Option<Clip>)> = Vec::new();
        for &root in self.host.children(ROOT).iter().rev() {
            stack.push((root, 0.0, 0.0, None));
        }

        let scale = self.scale;
        while let Some((id, ox, oy, clip)) = stack.pop() {
            let Some(node) = self.host.node(id) else {
                continue;
            };
            let style = self.layouts.style(StyleId(node.style));
            // `display: none` collapses layout but wouldn't stop paint on
            // its own — treat it like the hidden bit here too.
            let styled_away = style.display == taffy::Display::None;
            if node.hidden() || styled_away {
                continue;
            }
            let clips = style.overflow.x != taffy::Overflow::Visible
                || style.overflow.y != taffy::Overflow::Visible;
            let scrolls = style.overflow.x == taffy::Overflow::Scroll
                || style.overflow.y == taffy::Overflow::Scroll;
            let data = self.layouts.data(id);
            let x = ox + data.rect.origin.x;
            let y = oy + data.rect.origin.y;
            // Taffy locations are relative to the parent's BORDER box, so
            // children accumulate (x, y) — adding the content offset here
            // would count the parent's border+padding twice. The content
            // offset applies only to this node's own content (text).
            let cx = x + data.content[0];
            let cy = y + data.content[1];

            // This node's subtree clip: parent clip ∩ own padding box
            // when the style clips. The node's own paint isn't clipped
            // by its own box.
            let child_clip = if clips {
                let pad = Rect::new(
                    x + data.clip_box.origin.x,
                    y + data.clip_box.origin.y,
                    data.clip_box.size.width,
                    data.clip_box.size.height,
                );
                let radius = self.host.view(id).map(|v| v.radius).unwrap_or(0.0);
                Some(match clip {
                    Some(c) => Self::clip_intersect(c, pad, radius),
                    None => Clip {
                        min: [pad.origin.x, pad.origin.y],
                        max: [
                            pad.origin.x + pad.size.width,
                            pad.origin.y + pad.size.height,
                        ],
                        radius,
                    },
                })
            } else {
                clip
            };

            // Viewport culling: the node's own paint is skipped when its
            // border box misses the viewport (logical units). Children are
            // still visited — visible overflow can paint outside.
            let kind = node.kind();
            let visible = x + data.rect.size.width > 0.0
                && y + data.rect.size.height > 0.0
                && x < viewport.width
                && y < viewport.height;
            if visible {
                match kind {
                    NodeKind::VIEW | NodeKind::INPUT | NodeKind::CUSTOM => {
                        if let Some(view) = self.host.view(id) {
                            let paints_bg = view.color & 0xFF != 0
                                || (view.border_w > 0.0 && view.border_color & 0xFF != 0);
                            if paints_bg {
                                // Round edges on the physical grid so the
                                // fill lands crisp at any scale.
                                let x0 = (x * scale).round();
                                let y0 = (y * scale).round();
                                let x1 =
                                    ((x + data.rect.size.width) * scale).round();
                                let y1 =
                                    ((y + data.rect.size.height) * scale).round();
                                let start = self.scene.items.len();
                                self.scene.items.push(Instance::rect(
                                    x0,
                                    y0,
                                    x1 - x0,
                                    y1 - y0,
                                    view.color,
                                    view.radius * scale,
                                    view.border_w * scale,
                                    view.border_color,
                                ));
                                Self::apply_clip(&mut self.scene.items, start, clip, scale);
                            }
                        }
                        if kind == NodeKind::INPUT {
                            self.paint_input(id, cx, cy, clip, data);
                        }
                        if kind == NodeKind::CUSTOM {
                            self.paint_custom(id, cx, cy, clip, data);
                        }
                    }
                    NodeKind::TEXT => self.paint_text(id, cx, cy, data, clip),
                    _ => {}
                }
            }

            // Children are translated by the scroll offset — an O(1)
            // paint-time shift; Taffy never sees it.
            let [sx, sy] = if scrolls {
                self.scroll.get(&id.0).copied().unwrap_or([0.0, 0.0])
            } else {
                [0.0, 0.0]
            };
            for &child in self.host.children(id).iter().rev() {
                stack.push((child, x - sx, y - sy, child_clip));
            }
        }
    }

    /// Paints a text input: selection highlights, the editor's text (or
    /// placeholder when empty), and the caret when focused.
    fn paint_input(&mut self, id: NodeId, cx: f32, cy: f32, clip: Option<Clip>, data: crate::layout::LayoutData) {
        let focused = self.focus == Some(id);
        let content_w = (data.rect.size.width - data.insets[0]).max(0.0);
        let scale = self.scale;
        let scene = &mut self.scene;
        let text = &mut self.text;
        let Some(state) = self.inputs.get_mut(id.0) else {
            return;
        };
        // Keep the editor wrapped at the final content width.
        state.editor.set_width(Some(content_w));
        let fill = state.selection_color;
        let text_color = state.color;
        let font_size = state.font_size;
        let empty = state.editor.raw_text().is_empty();

        if focused {
            // Selection highlights behind the glyphs.
            let start = scene.items.len();
            state.editor.selection_geometry_with(|bb, _| {
                scene.items.push(Instance::quad(
                    (cx + bb.x0 as f32) * scale,
                    (cy + bb.y0 as f32) * scale,
                    (bb.x1 - bb.x0).max(0.0) as f32 * scale,
                    (bb.y1 - bb.y0).max(0.0) as f32 * scale,
                    fill,
                ));
            });
            Self::apply_clip(&mut scene.items, start, clip, scale);
        }

        let origin = Point::new(cx, cy);
        let start = scene.items.len();
        if empty && !state.placeholder.is_empty() {
            // Placeholder: half-alpha fill, never retained.
            let alpha = (text_color & 0xFF) / 2;
            let ph_color = (text_color & !0xFF) | alpha;
            let defaults = [
                StyleProperty::FontSize(font_size),
                StyleProperty::Brush(Color(ph_color)),
            ];
            let spec = ParagraphSpec {
                text: &state.placeholder,
                defaults: &defaults,
                spans: &[],
            };
            let layout = text.layout_paragraph(&spec, Some(content_w));
            text.emit(&layout, origin, scale, None, &mut scene.items);
        } else {
            let layout = state.editor.layout(&mut text.font_cx, &mut text.layout_cx);
            text.emit(layout, origin, scale, None, &mut scene.items);
        }
        Self::apply_clip(&mut scene.items, start, clip, scale);

        if focused && let Some(cursor) = state.editor.cursor_geometry(1.5) {
            let start = scene.items.len();
            scene.items.push(Instance::quad(
                (cx + cursor.x0 as f32) * scale,
                (cy + cursor.y0 as f32) * scale,
                ((cursor.x1 - cursor.x0).max(1.0) as f32) * scale,
                (cursor.y1 - cursor.y0).max(0.0) as f32 * scale,
                text_color,
            ));
            Self::apply_clip(&mut scene.items, start, clip, scale);
        }
    }

    /// Paints a custom node: the registered painter emits logical-space
    /// quads inside the node's content box; they are scaled to physical
    /// pixels and stamped with the inherited clip.
    fn paint_custom(
        &mut self,
        id: NodeId,
        cx: f32,
        cy: f32,
        clip: Option<Clip>,
        data: crate::layout::LayoutData,
    ) {
        let Some(spec) = self.custom.get(&id.0) else {
            return;
        };
        let Some(painter) = self.painters.get_mut(&spec.tag) else {
            return;
        };
        let content = Rect::new(
            cx,
            cy,
            (data.rect.size.width - data.insets[0]).max(0.0),
            (data.rect.size.height - data.insets[1]).max(0.0),
        );
        self.custom_scratch.clear();
        painter(spec, content, &mut self.custom_scratch);
        let scale = self.scale;
        let start = self.scene.items.len();
        for q in self.custom_scratch.drain(..) {
            self.scene.items.push(Instance::rect(
                q.x * scale,
                q.y * scale,
                q.w * scale,
                q.h * scale,
                q.color,
                q.radius * scale,
                q.border_w * scale,
                q.border_color,
            ));
        }
        Self::apply_clip(&mut self.scene.items, start, clip, scale);
    }

    /// Rebuilds a text node's retained Parley layout at `wrap` (logical
    /// content width, `None` = unwrapped). Shared by the Taffy measure
    /// path and the paint-time validation below.
    fn relayout_text(&mut self, id: NodeId, wrap: Option<f32>) {
        let Some(row) = self.host.text(id) else {
            return;
        };
        let defaults = [StyleProperty::FontSize(row.font_size)];
        let layout = self.text.layout_paragraph(
            &ParagraphSpec {
                text: &row.text,
                defaults: &defaults,
                spans: &[],
            },
            wrap,
        );
        let slot = id.0 as usize;
        if slot >= self.texts.len() {
            self.texts.resize_with(slot + 1, || None);
        }
        self.texts[slot] = Some(MeasuredText {
            layout,
            wrap_bits: wrap.map(f32::to_bits).unwrap_or(u32::MAX),
            emitted: None,
        });
    }

    /// Emits one text node: replays the cached instance batch when scale,
    /// origin and color all match — no shaping, rasterization, or cache
    /// lookups — and rewrites colors in place on a color-only change.
    ///
    /// The retained `MeasuredText` is validated against the node's FINAL
    /// content width: Taffy may have measured the leaf under a provisional
    /// constraint (min/max-content probing) that never reached paint.
    fn paint_text(&mut self, id: NodeId, cx: f32, cy: f32, data: crate::layout::LayoutData, clip: Option<Clip>) {
        let slot = id.0 as usize;
        let Some(row) = self.host.text(id) else {
            return;
        };
        let color = row.color;
        let scale_bits = self.scale.to_bits();

        // The content width Taffy wrapped this leaf at. A retained layout
        // produced under a different constraint is stale — rewrap now.
        let content_w = (data.rect.size.width - data.insets[0]).max(0.0);
        let wrap_bits = content_w.to_bits();
        let stale = match self.texts.get(slot).and_then(Option::as_ref) {
            Some(m) => m.wrap_bits != wrap_bits,
            None => true,
        };
        if stale {
            self.relayout_text(id, Some(content_w));
        }
        let Some(m) = self.texts.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };

        if let Some(e) = &mut m.emitted {
            if e.scale_bits == scale_bits && e.origin == [cx, cy] {
                if e.color != color {
                    for inst in &mut e.instances {
                        if inst.flags & Instance::FLAG_COLOR == 0 {
                            inst.color = color;
                        }
                    }
                    e.color = color;
                }
                let start = self.scene.items.len();
                self.scene.items.extend_from_slice(&e.instances);
                Self::apply_clip(&mut self.scene.items, start, clip, self.scale);
                return;
            }
        }

        // Cold emit: produce physical-pixel instances, then retain a copy
        // keyed on (scale, origin, color) for the next repaint. The clip
        // is NOT cached — it is restamped on every replay.
        let start = self.scene.items.len();
        self.text.emit(
            &m.layout,
            Point::new(cx, cy),
            self.scale,
            Some(Color(color)),
            &mut self.scene.items,
        );
        let instances = self.scene.items[start..].to_vec();
        Self::apply_clip(&mut self.scene.items, start, clip, self.scale);
        m.emitted = Some(EmittedText {
            scale_bits,
            origin: [cx, cy],
            color,
            instances,
        });
    }
}

// ------------------------------------------------------------- dispatch

/// Ancestor chain of `id`, deepest first (inclusive).
impl Ui {
    fn path_to(&self, id: NodeId) -> Vec<NodeId> {
        let mut path = Vec::new();
        let mut cur = id;
        loop {
            path.push(cur);
            let p = self.host.parent(cur);
            if p.is_nil() || p == NodeId::DETACHED {
                break;
            }
            cur = p;
        }
        path
    }

    /// Emits `event` to every node on `path` whose listener mask covers
    /// its kind.
    fn emit_path(&mut self, path: &[NodeId], mut event: UiEvent) {
        let bit = events::mask_for(event.kind);
        for &id in path {
            if self.host.props(id).listeners & bit != 0 {
                event.node = id.0;
                self.pending_events.push(event.clone());
            }
        }
    }

    /// The node's absolute border box (logical), scroll offsets applied.
    pub fn abs_rect(&self, id: NodeId) -> Rect {
        let data = self.layouts.data(id);
        let mut x = data.rect.origin.x;
        let mut y = data.rect.origin.y;
        let mut cur = id;
        loop {
            let p = self.host.parent(cur);
            if p.is_nil() || p == NodeId::DETACHED {
                break;
            }
            let pd = self.layouts.data(p);
            let [sx, sy] = self.scroll.get(&p.0).copied().unwrap_or([0.0, 0.0]);
            x += pd.rect.origin.x - sx;
            y += pd.rect.origin.y - sy;
            cur = p;
        }
        Rect::new(x, y, data.rect.size.width, data.rect.size.height)
    }

    /// The absolute content-box origin of a node (logical).
    fn content_origin(&self, id: NodeId) -> (f32, f32) {
        let r = self.abs_rect(id);
        let data = self.layouts.data(id);
        (r.origin.x + data.content[0], r.origin.y + data.content[1])
    }

    /// Deepest node containing (x, y) logical, honoring clip chains,
    /// scroll offsets, hidden/display:none.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        for i in (0..self.host.child_count(ROOT)).rev() {
            let root = self.host.child_at(ROOT, i);
            if let Some(hit) =
                self.hit_node(root, 0.0, 0.0, x, y, [f32::MIN, f32::MIN, f32::MAX, f32::MAX])
            {
                return Some(hit);
            }
        }
        None
    }

    fn hit_node(
        &self,
        id: NodeId,
        ox: f32,
        oy: f32,
        x: f32,
        y: f32,
        clip: [f32; 4],
    ) -> Option<NodeId> {
        if x < clip[0] || y < clip[1] || x >= clip[2] || y >= clip[3] {
            return None;
        }
        let node = self.host.node(id)?;
        let style = self.layouts.style(StyleId(node.style));
        if node.hidden() || style.display == taffy::Display::None {
            return None;
        }
        let data = self.layouts.data(id);
        let ax = ox + data.rect.origin.x;
        let ay = oy + data.rect.origin.y;

        let clips = style.overflow.x != taffy::Overflow::Visible
            || style.overflow.y != taffy::Overflow::Visible;
        let child_clip = if clips {
            [
                clip[0].max(ax + data.clip_box.origin.x),
                clip[1].max(ay + data.clip_box.origin.y),
                clip[2].min(ax + data.clip_box.origin.x + data.clip_box.size.width),
                clip[3].min(ay + data.clip_box.origin.y + data.clip_box.size.height),
            ]
        } else {
            clip
        };
        let scrolls = style.overflow.x == taffy::Overflow::Scroll
            || style.overflow.y == taffy::Overflow::Scroll;
        let [sx, sy] = if scrolls {
            self.scroll.get(&id.0).copied().unwrap_or([0.0, 0.0])
        } else {
            [0.0, 0.0]
        };

        // Children paint above their parent and later siblings paint
        // above earlier ones — test them last-to-first.
        for &child in self.host.children(id).iter().rev() {
            if let Some(hit) = self.hit_node(child, ax - sx, ay - sy, x, y, child_clip) {
                return Some(hit);
            }
        }
        if x >= ax && y >= ay && x < ax + data.rect.size.width && y < ay + data.rect.size.height {
            return Some(id);
        }
        None
    }

    /// Focusable nodes in document order (for Tab traversal).
    fn focusables(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.host.children(ROOT).iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.host.node(id) else { continue };
            if node.hidden() {
                continue;
            }
            if node.kind() == NodeKind::INPUT || self.host.props(id).focusable {
                out.push(id);
            }
            for &child in self.host.children(id).iter().rev() {
                stack.push(child);
            }
        }
        out
    }

    /// Moves focus; emits blur/focus events, manages IME composition.
    pub fn set_focus(&mut self, next: Option<NodeId>) {
        if self.focus == next {
            return;
        }
        if let Some(old) = self.focus {
            if self.host.node(old).map(|n| n.kind()) == Some(NodeKind::INPUT) {
                self.inputs.finish_compose(&mut self.text, old.0);
            }
            if self.host.props(old).listeners & mask::FOCUS != 0 {
                self.pending_events.push(UiEvent::new(out_kind::BLUR, old.0));
            }
        }
        self.focus = next.filter(|&id| self.host.node(id).is_some());
        if let Some(id) = self.focus
            && self.host.props(id).listeners & mask::FOCUS != 0
        {
            self.pending_events.push(UiEvent::new(out_kind::FOCUS, id.0));
        }
        self.a11y_stale = true;
        self.host.force_paint();
    }

    /// Sets a scroll offset, clamped to the layout extent and to the
    /// axes the node's style actually scrolls; returns the applied
    /// offset when it changed.
    pub fn scroll_to(&mut self, id: NodeId, x: f32, y: f32) -> Option<[f32; 2]> {
        let style = self
            .host
            .node(id)
            .map(|n| self.layouts.style(StyleId(n.style)));
        let x_ok = style.is_some_and(|s| s.overflow.x == taffy::Overflow::Scroll);
        let y_ok = style.is_some_and(|s| s.overflow.y == taffy::Overflow::Scroll);
        if !x_ok && !y_ok {
            return None;
        }
        let extent = self.layouts.data(id).scroll_extent;
        let cur0 = self.scroll.get(&id.0).copied().unwrap_or([0.0, 0.0]);
        let next = [
            if x_ok { x.clamp(0.0, extent[0]) } else { cur0[0] },
            if y_ok { y.clamp(0.0, extent[1]) } else { cur0[1] },
        ];
        let cur = self.scroll.entry(id.0).or_insert([0.0, 0.0]);
        if *cur == next {
            return None;
        }
        *cur = next;
        self.a11y_stale = true;
        self.host.force_paint();
        Some(next)
    }

    fn scroll_by(&mut self, id: NodeId, dx: f32, dy: f32) -> Option<[f32; 2]> {
        let cur = self.scroll.get(&id.0).copied().unwrap_or([0.0, 0.0]);
        self.scroll_to(id, cur[0] + dx, cur[1] + dy)
    }

    /// The nearest ancestor of `id` (inclusive) whose style scrolls on
    /// the requested axis.
    fn scrollable_ancestor(&self, id: NodeId, dy: bool) -> Option<NodeId> {
        let mut cur = Some(id);
        while let Some(id) = cur {
            let node = self.host.node(id)?;
            let style = self.layouts.style(StyleId(node.style));
            let scrolls = if dy {
                style.overflow.y == taffy::Overflow::Scroll
            } else {
                style.overflow.x == taffy::Overflow::Scroll
            };
            if scrolls {
                return Some(id);
            }
            let p = self.host.parent(id);
            cur = (!p.is_nil() && p != NodeId::DETACHED).then_some(p);
        }
        None
    }

    /// Handles one normalized platform event: native consumption
    /// (scroll, editing, focus) plus JS emission into `pending_events`.
    pub fn dispatch(&mut self, ev: &Event) {
        match ev {
            Event::PointerMove { x, y } => self.pointer_move(*x, *y),
            Event::PointerDown { x, y, button, mods } => {
                self.pointer_down(*x, *y, *button, *mods)
            }
            Event::PointerUp { x, y, button } => {
                // Pointer capture: while a button was held the press
                // target owns the release, wherever the pointer is.
                let target = self.pressed.take().or_else(|| self.hit_test(*x, *y));
                if let Some(hit) = target {
                    self.emit_pointer(hit, out_kind::POINTER_UP, *x, *y, *button, Mods::default());
                }
            }
            Event::Wheel { x, y, dx, dy } => {
                let hit = self.hit_test(*x, *y);
                if let Some(hit) = hit {
                    // Consume vertically or horizontally scrollable
                    // ancestors first.
                    let target = self
                        .scrollable_ancestor(hit, true)
                        .or_else(|| self.scrollable_ancestor(hit, false));
                    if let Some(id) = target
                        && let Some(off) = self.scroll_by(id, *dx, *dy)
                        && self.host.props(id).listeners & mask::SCROLL != 0
                    {
                        let mut e = UiEvent::new(out_kind::SCROLL, id.0);
                        e.a = off[0];
                        e.b = off[1];
                        self.pending_events.push(e);
                    }
                    let path = self.path_to(hit);
                    let mut e = UiEvent::new(out_kind::WHEEL, hit.0);
                    e.x = *x;
                    e.y = *y;
                    e.a = *dx;
                    e.b = *dy;
                    self.emit_path(&path, e);
                }
            }
            Event::KeyDown(k) => self.key_down(k),
            Event::KeyUp(k) => {
                if let Some(f) = self.focus {
                    let path = self.path_to(f);
                    let mut e = UiEvent::new(out_kind::KEY_UP, f.0);
                    e.key = k.key.code();
                    e.text = k.char.clone().unwrap_or_default();
                    self.emit_path(&path, e);
                }
            }
            Event::ImePreedit { text, cursor } => {
                if let Some(f) = self.focus {
                    self.inputs.set_compose(&mut self.text, f.0, text, *cursor);
                    self.host.force_paint();
                }
            }
            Event::ImeCommit(s) => {
                if let Some(f) = self.focus {
                    self.inputs.commit(&mut self.text, f.0, s);
                    self.host.force_paint();
                    self.emit_change(f);
                }
            }
            Event::ImeDone => {
                if let Some(f) = self.focus {
                    self.inputs.finish_compose(&mut self.text, f.0);
                }
            }
            Event::Focus(gained) => {
                if !gained {
                    self.hover = None;
                    self.pressed = None;
                }
            }
        }
    }

    fn pointer_move(&mut self, x: f32, y: f32) {
        // Drag capture: a press inside an input extends its selection.
        if let Some(pid) = self.pressed
            && self.host.node(pid).map(|n| n.kind()) == Some(NodeKind::INPUT)
        {
            let (cx, cy) = self.content_origin(pid);
            self.inputs.act(
                &mut self.text,
                pid.0,
                &KeyAction::ExtendTo(x - cx, y - cy),
            );
            self.host.force_paint();
        }
        let hit = self.hit_test(x, y);
        if hit != self.hover {
            // Synthesize leave/enter on the symmetric difference of the
            // ancestor chains.
            let old: Vec<NodeId> = self
                .hover
                .map(|h| self.path_to(h))
                .unwrap_or_default();
            let new: Vec<NodeId> = hit.map(|h| self.path_to(h)).unwrap_or_default();
            for &id in &old {
                if !new.contains(&id)
                    && self.host.props(id).listeners & mask::POINTER_ENTER_LEAVE != 0
                {
                    let mut e = UiEvent::new(out_kind::POINTER_LEAVE, id.0);
                    e.x = x;
                    e.y = y;
                    self.pending_events.push(e);
                }
            }
            for &id in &new {
                if !old.contains(&id)
                    && self.host.props(id).listeners & mask::POINTER_ENTER_LEAVE != 0
                {
                    let mut e = UiEvent::new(out_kind::POINTER_ENTER, id.0);
                    e.x = x;
                    e.y = y;
                    self.pending_events.push(e);
                }
            }
            self.hover = hit;
        }
        // Pointer capture: a pressed node keeps receiving moves outside
        // its bounds (text selection drag, slider behaviors).
        if let Some(target) = self.pressed.or(hit) {
            self.emit_pointer(
                target,
                out_kind::POINTER_MOVE,
                x,
                y,
                crate::events::Button::Primary,
                Mods::default(),
            );
        }
    }

    fn pointer_down(&mut self, x: f32, y: f32, button: crate::events::Button, mods: Mods) {
        let hit = self.hit_test(x, y);
        self.pressed = hit;

        // Focus: nearest focusable/input ancestor of the hit; clicking
        // non-focusable space blurs.
        let focus_target = hit.and_then(|h| {
            self.path_to(h).into_iter().find(|&id| {
                self.host.node(id).map(|n| n.kind()) == Some(NodeKind::INPUT)
                    || self.host.props(id).focusable
            })
        });
        self.set_focus(focus_target);

        // Input hit: caret/selection.
        if let Some(id) = hit
            && self.host.node(id).map(|n| n.kind()) == Some(NodeKind::INPUT)
            && button == crate::events::Button::Primary
        {
            let (cx, cy) = self.content_origin(id);
            let (lx, ly) = (x - cx, y - cy);
            let action = if mods.shift {
                KeyAction::ExtendTo(lx, ly)
            } else {
                let double = self
                    .last_click
                    .map(|(t, n, px, py)| {
                        n == id && t.elapsed().as_millis() < 500
                            && (px - x).abs() < 4.0
                            && (py - y).abs() < 4.0
                    })
                    .unwrap_or(false);
                if double {
                    KeyAction::SelectWordAt(lx, ly)
                } else {
                    KeyAction::MoveTo(lx, ly)
                }
            };
            self.last_click = Some((Instant::now(), id, x, y));
            self.inputs.act(&mut self.text, id.0, &action);
            self.host.force_paint();
        }
        if let Some(hit) = hit {
            self.emit_pointer(hit, out_kind::POINTER_DOWN, x, y, button, mods);
        }
    }

    fn emit_pointer(
        &mut self,
        hit: NodeId,
        kind: u8,
        x: f32,
        y: f32,
        button: crate::events::Button,
        mods: Mods,
    ) {
        let path = self.path_to(hit);
        let button_code = match button {
            crate::events::Button::Primary => 1,
            crate::events::Button::Secondary => 2,
            crate::events::Button::Middle => 3,
            crate::events::Button::Other(b) => b as u32,
        };
        let key = (mods.shift as u32)
            | ((mods.ctrl as u32) << 1)
            | ((mods.alt as u32) << 2)
            | ((mods.meta as u32) << 3)
            | (button_code << 8);
        // `a`/`b` carry coordinates relative to each receiving node's
        // border box — the offsets behaviors (sliders, drags) need.
        let bit = events::mask_for(kind);
        for &id in &path {
            if self.host.props(id).listeners & bit == 0 {
                continue;
            }
            let rect = self.abs_rect(id);
            let mut e = UiEvent::new(kind, id.0);
            e.x = x;
            e.y = y;
            e.a = x - rect.origin.x;
            e.b = y - rect.origin.y;
            e.key = key;
            self.pending_events.push(e);
        }
    }

    fn key_down(&mut self, k: &KeyInput) {
        // Tab traversal beats everything.
        if k.key == Key::Tab && !k.mods.ctrl && !k.mods.meta {
            let focusables = self.focusables();
            if !focusables.is_empty() {
                let next = match self.focus {
                    None => focusables[0],
                    Some(f) => {
                        let i = focusables.iter().position(|&n| n == f);
                        match (i, k.mods.shift) {
                            (Some(i), true) => {
                                focusables[(i + focusables.len() - 1) % focusables.len()]
                            }
                            (Some(i), false) => focusables[(i + 1) % focusables.len()],
                            (None, _) => focusables[0],
                        }
                    }
                };
                self.set_focus(Some(next));
            }
        }

        let focused_input = self
            .focus
            .filter(|&f| self.host.node(f).map(|n| n.kind()) == Some(NodeKind::INPUT));

        if let Some(id) = focused_input {
            if k.key == Key::Escape {
                self.set_focus(None);
            }
            let multiline = self
                .inputs
                .get(id.0)
                .map(|s| s.multiline)
                .unwrap_or(false);
            if let Some(action) = key_action(k, multiline) {
                match action {
                    KeyAction::Submit => {
                        let mut e = UiEvent::new(out_kind::SUBMIT, id.0);
                        e.text = self.inputs.text(id.0);
                        self.pending_events.push(e);
                    }
                    _ => {
                        self.inputs.act(&mut self.text, id.0, &action);
                        self.host.force_paint();
                        self.emit_change(id);
                    }
                }
            }
        }

        // Keys go to the focused node's path (or every KEY listener when
        // nothing is focused — global shortcuts).
        let mut e = UiEvent::new(out_kind::KEY_DOWN, 0);
        e.key = k.key.code();
        e.text = k.char.clone().unwrap_or_default();
        match self.focus {
            Some(f) => {
                let path = self.path_to(f);
                self.emit_path(&path, e);
            }
            None => {
                // Global delivery: every node with a key listener.
                let mut stack: Vec<NodeId> =
                    self.host.children(ROOT).iter().rev().copied().collect();
                while let Some(id) = stack.pop() {
                    if self.host.node(id).is_none() {
                        continue;
                    }
                    if self.host.props(id).listeners & mask::KEY != 0 {
                        e.node = id.0;
                        self.pending_events.push(e.clone());
                    }
                    for &child in self.host.children(id).iter().rev() {
                        stack.push(child);
                    }
                }
            }
        }
    }

    /// Emits `onChangeText` when the input buffer advanced.
    fn emit_change(&mut self, id: NodeId) {
        if let Some(text) = self.inputs.take_change(id.0) {
            self.a11y_stale = true;
            if self.host.props(id).listeners & mask::INPUT != 0 {
                let mut e = UiEvent::new(out_kind::CHANGE, id.0);
                e.text = text;
                self.pending_events.push(e);
            }
        }
    }

    /// The IME cursor area of the focused input, in logical points:
    /// absolute rect of the caret/composition region.
    pub fn ime_area(&self) -> Option<Rect> {
        let id = self.focus?;
        if self.host.node(id).map(|n| n.kind()) != Some(NodeKind::INPUT) {
            return None;
        }
        let state = self.inputs.get(id.0)?;
        let (cx, cy) = self.content_origin(id);
        let area = state.editor.ime_cursor_area();
        Some(Rect::new(
            cx + area.x0 as f32,
            cy + area.y0 as f32,
            (area.x1 - area.x0).max(0.0) as f32,
            (area.y1 - area.y0).max(0.0) as f32,
        ))
    }

    /// Whether an input node is focused — the platform enables IME.
    pub fn ime_wanted(&self) -> bool {
        self.focus
            .map(|f| self.host.node(f).map(|n| n.kind()) == Some(NodeKind::INPUT))
            .unwrap_or(false)
    }
}

/// Maps a normalized key event to an editing action. `meta` (Cmd/Super)
/// takes priority, then `alt` (word granularity), then `shift`
/// (selection). Returns `None` when the key means nothing to an input.
fn key_action(k: &KeyInput, multiline: bool) -> Option<KeyAction> {
    let m = &k.mods;
    // Printable text inserts unless a command chord is held.
    if !m.meta && !m.ctrl
        && let Some(t) = &k.text
        && !t.is_empty()
    {
        return Some(KeyAction::Insert(t.clone()));
    }
    if m.meta || m.ctrl {
        let c = k.char.as_deref().unwrap_or("");
        return Some(match (c, k.key) {
            ("a", _) => KeyAction::SelectAll,
            ("c", _) => KeyAction::Copy,
            ("x", _) => KeyAction::Cut,
            ("v", _) => KeyAction::Paste,
            ("z", _) if m.shift => KeyAction::Redo,
            ("z", _) | ("y", _) => KeyAction::Undo,
            (_, Key::Left) if m.shift => KeyAction::SelectTextStart,
            (_, Key::Left) => KeyAction::MoveTextStart,
            (_, Key::Right) if m.shift => KeyAction::SelectTextEnd,
            (_, Key::Right) => KeyAction::MoveTextEnd,
            (_, Key::Up) if m.shift => KeyAction::SelectTextStart,
            (_, Key::Up) => KeyAction::MoveTextStart,
            (_, Key::Down) if m.shift => KeyAction::SelectTextEnd,
            (_, Key::Down) => KeyAction::MoveTextEnd,
            _ => return None,
        });
    }
    let sel = m.shift;
    Some(match k.key {
        Key::Backspace if m.alt => KeyAction::BackspaceWord,
        Key::Backspace => KeyAction::Backspace,
        Key::Delete if m.alt => KeyAction::DeleteWord,
        Key::Delete => KeyAction::Delete,
        Key::Enter => {
            if multiline {
                KeyAction::Newline
            } else {
                KeyAction::Submit
            }
        }
        Key::Escape => return None, // handled by the caller as blur intent
        Key::Tab => return None,    // traversal handled above
        Key::Left if sel && m.alt => KeyAction::SelectWordLeft,
        Key::Left if sel => KeyAction::SelectLeft,
        Key::Left if m.alt => KeyAction::MoveWordLeft,
        Key::Left => KeyAction::MoveLeft,
        Key::Right if sel && m.alt => KeyAction::SelectWordRight,
        Key::Right if sel => KeyAction::SelectRight,
        Key::Right if m.alt => KeyAction::MoveWordRight,
        Key::Right => KeyAction::MoveRight,
        Key::Up if sel => KeyAction::SelectUp,
        Key::Up => KeyAction::MoveUp,
        Key::Down if sel => KeyAction::SelectDown,
        Key::Down => KeyAction::MoveDown,
        Key::Home if sel => KeyAction::SelectLineStart,
        Key::Home => KeyAction::MoveLineStart,
        Key::End if sel => KeyAction::SelectLineEnd,
        Key::End => KeyAction::MoveLineEnd,
        Key::PageUp | Key::PageDown | Key::Unknown => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Encoder;

    const KIND_VIEW: u8 = 0;
    const KIND_TEXT: u8 = 1;
    const NIL: u32 = u32::MAX;

    /// Glyph instances in the unified stream (everything that isn't a
    /// solid rect).
    fn glyphs(scene: &Scene) -> usize {
        scene
            .items
            .iter()
            .filter(|i| i.flags & Instance::FLAG_SOLID == 0)
            .count()
    }

    fn txn() -> Vec<u8> {
        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.display = taffy::Display::Flex;
        // Column container: children get the container width as definite
        // cross-axis space, which is what wraps text to the viewport.
        s.flex_direction = taffy::FlexDirection::Column;
        s.size = taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        };
        enc.style(1, &s);
        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.place(NIL, 0, NIL);
        enc.create(1, KIND_TEXT);
        enc.set_text(1, &"word ".repeat(60));
        enc.text_props(1, 20.0, 0xFFFF_FFFF);
        enc.place(0, 1, NIL);
        enc.finish(1)
    }

    /// The resize path: no mutation arrives, `invalidate_layout` alone
    /// must cause text to rewrap to the new available width.
    #[test]
    fn resize_reflows_text() {
        let mut ui = Ui::new(1.0);
        ui.apply(&txn()).unwrap();
        ui.render(Size::new(1600.0, 800.0));
        let wide = ui.text_layout(NodeId(1)).unwrap().len();

        ui.invalidate_layout();
        ui.render(Size::new(300.0, 800.0));
        let narrow = ui.text_layout(NodeId(1)).unwrap().len();

        assert!(
            narrow > wide,
            "narrower viewport must wrap to more lines: {wide} -> {narrow}"
        );
    }

    /// Hidden subtrees emit nothing but don't truncate siblings.
    #[test]
    fn hidden_skips_paint_keeps_siblings() {
        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.display = taffy::Display::Flex;
        s.size = taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        };
        enc.style(1, &s);
        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.place(NIL, 0, NIL);
        // The middle node is hidden; "after" must still paint — a hidden
        // node must not truncate its sibling chain.
        for (id, text) in [(2u32, "before"), (3, "hidden"), (4, "after")] {
            enc.create(id, KIND_TEXT);
            enc.set_text(id, text);
            enc.text_props(id, 20.0, 0xFFFF_FFFF);
            if id == 3 {
                enc.hidden(id, true);
            }
            enc.place(0, id, NIL);
        }
        let buf = enc.finish(1);
        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        let hidden_count = glyphs(ui.render(Size::new(800.0, 600.0)));
        assert!(hidden_count > 0);

        let mut enc = Encoder::new();
        enc.hidden(3, false);
        let buf = enc.finish(2);
        ui.apply(&buf).unwrap();
        let shown_count = glyphs(ui.render(Size::new(800.0, 600.0)));
        assert!(
            shown_count > hidden_count,
            "unhiding node 3 must add its glyphs: {hidden_count} -> {shown_count}"
        );
    }

    /// Nested padding+border: a child's accumulated origin must include
    /// each ancestor's insets exactly once (the layout location is
    /// relative to the parent's content box).
    #[test]
    fn nested_insets_accumulate_once() {
        let mut enc = Encoder::new();
        let mut outer = taffy::Style::default();
        outer.display = taffy::Display::Flex;
        outer.padding = taffy::Rect {
            left: taffy::LengthPercentage::length(10.0),
            right: taffy::LengthPercentage::length(0.0),
            top: taffy::LengthPercentage::length(20.0),
            bottom: taffy::LengthPercentage::length(0.0),
        };
        outer.border = taffy::Rect {
            left: taffy::LengthPercentage::length(3.0),
            right: taffy::LengthPercentage::length(0.0),
            top: taffy::LengthPercentage::length(4.0),
            bottom: taffy::LengthPercentage::length(0.0),
        };
        enc.style(1, &outer);
        let mut inner = taffy::Style::default();
        inner.display = taffy::Display::Flex;
        inner.padding = taffy::Rect {
            left: taffy::LengthPercentage::length(5.0),
            right: taffy::LengthPercentage::length(0.0),
            top: taffy::LengthPercentage::length(7.0),
            bottom: taffy::LengthPercentage::length(0.0),
        };
        enc.style(2, &inner);
        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.place(NIL, 0, NIL);
        enc.create(1, KIND_VIEW);
        enc.set_style(1, 2);
        enc.place(0, 1, NIL);
        enc.create(2, KIND_VIEW);
        enc.view_paint(2, 0xFF00_00FF);
        enc.place(1, 2, NIL);
        let buf = enc.finish(1);

        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(800.0, 600.0));

        // Inner's border box sits at the outer's content origin (13, 24).
        // The leaf's border box adds inner's content offset (5, 7).
        let leaf = ui.layouts.data(NodeId(2));
        assert_eq!(leaf.rect.origin.x, 5.0);
        assert_eq!(leaf.rect.origin.y, 7.0);
        let inner_data = ui.layouts.data(NodeId(1));
        assert_eq!(inner_data.rect.origin.x, 13.0);
        assert_eq!(inner_data.rect.origin.y, 24.0);

        // Paint-time accumulated origin: the leaf quad emits at the
        // accumulated content origin (13+5, 24+7) = (18, 31) — insets
        // counted exactly once.
        let quad = ui
            .scene()
            .items
            .iter()
            .find(|i| i.flags & Instance::FLAG_SOLID != 0)
            .expect("leaf quad emitted");
        assert_eq!(quad.position, [18.0, 31.0]);
    }

    /// `display: none` must remove the subtree from layout AND paint —
    /// the flag path and the style path are equivalent.
    #[test]
    fn display_none_skips_subtree() {
        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.display = taffy::Display::Flex;
        s.flex_direction = taffy::FlexDirection::Column;
        enc.style(1, &s);
        let mut gone = taffy::Style::default();
        gone.display = taffy::Display::None;
        enc.style(2, &gone);

        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.place(NIL, 0, NIL);
        // Hidden-by-style container with a text child.
        enc.create(1, KIND_VIEW);
        enc.set_style(1, 2);
        enc.place(0, 1, NIL);
        enc.create(2, KIND_TEXT);
        enc.set_text(2, "invisible");
        enc.place(1, 2, NIL);
        // Sibling still paints.
        enc.create(3, KIND_TEXT);
        enc.set_text(3, "visible");
        enc.place(0, 3, NIL);
        let buf = enc.finish(1);

        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        let scene = ui.render(Size::new(800.0, 600.0));
        let n = glyphs(scene);
        assert!(n > 0);
        // "invisible" (9 chars) must not emit; "visible" (7) does.
        assert!(n < 10, "{n} glyphs");
    }

    // ------------------------------------------------------ dispatch

    use crate::events::{Button, Event};

    /// A scrollable 100px container with a 300px column of children.
    /// Returns (ui, container id, child id).
    fn scroll_ui() -> (Ui, NodeId, NodeId) {
        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.display = taffy::Display::Flex;
        s.overflow = taffy::Point {
            x: taffy::Overflow::Scroll,
            y: taffy::Overflow::Scroll,
        };
        s.size = taffy::Size {
            width: taffy::Dimension::length(100.0),
            height: taffy::Dimension::length(100.0),
        };
        enc.style(1, &s);
        let mut child = taffy::Style::default();
        child.size = taffy::Size {
            width: taffy::Dimension::length(100.0),
            height: taffy::Dimension::length(300.0),
        };
        enc.style(2, &child);
        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.place(NIL, 0, NIL);
        enc.create(1, KIND_VIEW);
        enc.set_style(1, 2);
        enc.place(0, 1, NIL);
        let buf = enc.finish(1);
        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(800.0, 600.0));
        (ui, NodeId(0), NodeId(1))
    }

    /// A wheel event scrolls the nearest scrollable ancestor and emits a
    /// SCROLL event when the node subscribed.
    #[test]
    fn wheel_scrolls_and_reports() {
        let (mut ui, container, child) = scroll_ui();
        // Subscribe to scroll events on the container.
        let mut enc = Encoder::new();
        enc.props(0, mask::SCROLL, false);
        let buf = enc.finish(2);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(800.0, 600.0));

        ui.dispatch(&Event::Wheel {
            x: 10.0,
            y: 10.0,
            dx: 0.0,
            dy: 50.0,
        });
        assert_eq!(ui.scroll.get(&container.0), Some(&[0.0, 50.0]));

        let events = ui.take_events();
        let scroll = events
            .iter()
            .find(|e| e.kind == out_kind::SCROLL)
            .expect("scroll event emitted");
        assert_eq!(scroll.node, container.0);
        assert_eq!((scroll.a, scroll.b), (0.0, 50.0));

        // Scrolled: the hit under (10,10) is still inside the child (the
        // child is 300 tall) but the child must now answer at a scrolled
        // position — a hit at y=250 hits nothing outside the container's
        // clip.
        assert_eq!(ui.hit_test(10.0, 250.0), None);
        assert_eq!(ui.hit_test(10.0, 10.0), Some(child));

        // Clamp at the extent: 300 - 100 = 200.
        ui.dispatch(&Event::Wheel {
            x: 10.0,
            y: 10.0,
            dx: 0.0,
            dy: 1000.0,
        });
        assert_eq!(ui.scroll.get(&container.0), Some(&[0.0, 200.0]));
    }

    /// Clipped children are not hit outside the clip rect.
    #[test]
    fn hit_test_respects_clip() {
        let (ui, _container, _child) = scroll_ui();
        // Child is 300 tall but the container clips at 100.
        assert_eq!(ui.hit_test(50.0, 150.0), None);
    }

    /// Pointer down focuses the nearest focusable ancestor; Tab cycles
    /// the focus ring and emits blur/focus events.
    #[test]
    fn focus_click_and_tab() {
        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.size = taffy::Size {
            width: taffy::Dimension::length(50.0),
            height: taffy::Dimension::length(20.0),
        };
        enc.style(1, &s);
        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.props(0, mask::FOCUS, true);
        enc.place(NIL, 0, NIL);
        enc.create(1, KIND_VIEW);
        enc.set_style(1, 1);
        enc.props(1, mask::FOCUS, true);
        enc.place(0, 1, NIL);
        let buf = enc.finish(1);
        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(800.0, 600.0));

        let mods = Mods::default();
        ui.dispatch(&Event::PointerDown {
            x: 10.0,
            y: 10.0,
            button: Button::Primary,
            mods,
        });
        // The click landed on child 1; its ancestor chain's first
        // focusable is the child itself.
        assert_eq!(ui.focused(), Some(NodeId(1)));
        assert!(ui
            .take_events()
            .iter()
            .any(|e| e.kind == out_kind::FOCUS && e.node == 1));

        // Tab moves to the next focusable (wraps to 0, the parent is
        // earlier in document order).
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Tab,
            text: None,
            char: None,
            mods,
        }));
        assert_eq!(ui.focused(), Some(NodeId(0)));
        let events = ui.take_events();
        assert!(events.iter().any(|e| e.kind == out_kind::BLUR && e.node == 1));
        assert!(events.iter().any(|e| e.kind == out_kind::FOCUS && e.node == 0));

        // Shift+Tab wraps backward.
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Tab,
            text: None,
            char: None,
            mods: Mods {
                shift: true,
                ..Mods::default()
            },
        }));
        assert_eq!(ui.focused(), Some(NodeId(1)));
    }

    /// Typing into a focused input mutates the buffer natively and emits
    /// a CHANGE event with the committed text.
    #[test]
    fn input_typing_changes_buffer() {
        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.size = taffy::Size {
            width: taffy::Dimension::length(200.0),
            height: taffy::Dimension::length(30.0),
        };
        enc.style(1, &s);
        enc.create(0, 2); // KIND_INPUT
        enc.set_style(0, 1);
        enc.input_props(0, 16.0, 0xFFFF_FFFF, "", false);
        enc.props(0, mask::INPUT, true);
        enc.place(NIL, 0, NIL);
        let buf = enc.finish(1);
        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(800.0, 600.0));

        // Click into the field, then type.
        ui.dispatch(&Event::PointerDown {
            x: 5.0,
            y: 5.0,
            button: Button::Primary,
            mods: Mods::default(),
        });
        assert_eq!(ui.focused(), Some(NodeId(0)));
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Unknown,
            text: Some("h".into()),
            char: Some("h".into()),
            mods: Mods::default(),
        }));
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Unknown,
            text: Some("i".into()),
            char: Some("i".into()),
            mods: Mods::default(),
        }));
        assert_eq!(ui.inputs.text(0), "hi");

        let events = ui.take_events();
        let changes: Vec<&str> = events
            .iter()
            .filter(|e| e.kind == out_kind::CHANGE)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(changes, ["h", "hi"]);

        // Enter on single-line emits SUBMIT, not a newline.
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Enter,
            text: None,
            char: None,
            mods: Mods::default(),
        }));
        assert!(ui
            .take_events()
            .iter()
            .any(|e| e.kind == out_kind::SUBMIT && e.text == "hi"));
    }

    /// A registered painter receives the node's payload and content
    /// rect; its logical quads land scaled and clipped in the scene.
    #[test]
    fn custom_painter_emits_quads() {
        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.size = taffy::Size {
            width: taffy::Dimension::length(100.0),
            height: taffy::Dimension::length(40.0),
        };
        enc.style(1, &s);
        enc.create(0, 3); // KIND_CUSTOM
        enc.set_style(0, 1);
        enc.custom(0, 7, [1.0, 2.0, 3.0, 4.0], "payload");
        enc.place(NIL, 0, NIL);
        let buf = enc.finish(1);

        let mut ui = Ui::new(2.0);
        ui.register_painter(
            7,
            Box::new(|spec, rect, out| {
                assert_eq!(spec.data, [1.0, 2.0, 3.0, 4.0]);
                assert_eq!(spec.text, "payload");
                assert_eq!(rect.size.width, 100.0);
                out.push(Quad {
                    x: rect.origin.x,
                    y: rect.origin.y,
                    w: rect.size.width / 2.0,
                    h: rect.size.height,
                    color: 0xFF00_00FF,
                    ..Quad::default()
                });
            }),
        );
        ui.apply(&buf).unwrap();
        ui.render(Size::new(800.0, 600.0));

        let inst = ui
            .scene()
            .items
            .iter()
            .find(|i| i.color == 0xFF00_00FF)
            .expect("painter quad in scene");
        // scale=2: a 50x40 logical quad lands as 100x80 physical.
        assert_eq!(inst.size, [100.0, 80.0]);
        assert!(inst.flags & Instance::FLAG_SOLID != 0);
    }

    /// A custom node with no registered painter paints nothing but its
    /// background and does not stall the paint pass.
    #[test]
    fn custom_without_painter_paints_background() {
        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.size = taffy::Size {
            width: taffy::Dimension::length(50.0),
            height: taffy::Dimension::length(50.0),
        };
        enc.style(1, &s);
        enc.create(0, 3);
        enc.set_style(0, 1);
        enc.paint(0, Some(0x1122_3344), None, None);
        enc.custom(0, 99, [0.0; 4], "");
        enc.place(NIL, 0, NIL);
        let buf = enc.finish(1);

        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(800.0, 600.0));
        assert!(ui
            .scene()
            .items
            .iter()
            .any(|i| i.color == 0x1122_3344));
    }

    // ---- accessibility ----

    const KIND_INPUT: u8 = 2;

    /// The retained tree projects onto an AccessKit tree: roles, names,
    /// values, bounds, and focus all come from host state.
    #[test]
    fn a11y_tree_maps_roles_names_focus() {
        use accesskit::{Action, ActionRequest, Role, TreeId};
        use crate::a11y::aid;

        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.display = taffy::Display::Flex;
        s.flex_direction = taffy::FlexDirection::Column;
        s.size = taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        };
        enc.style(1, &s);
        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.place(NIL, 0, NIL);
        enc.label(0, "root container");
        enc.create(1, KIND_TEXT);
        enc.set_text(1, "hello");
        enc.text_props(1, 14.0, 0xFFFF_FFFF);
        enc.place(0, 1, NIL);
        enc.create(2, KIND_INPUT);
        enc.input_props(2, 14.0, 0xFFFF_FFFF, "type here", false);
        enc.place(0, 2, NIL);
        let buf = enc.finish(1);

        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(400.0, 300.0));
        let tree = ui.a11y_tree(Size::new(400.0, 300.0));
        let find = |id: u32| {
            tree.nodes
                .iter()
                .find(|(n, _)| *n == aid(NodeId(id)))
                .map(|(_, n)| n)
        };

        assert_eq!(find(0).unwrap().role(), Role::GenericContainer);
        assert_eq!(find(0).unwrap().label(), Some("root container"));
        assert_eq!(find(1).unwrap().role(), Role::Label);
        assert_eq!(find(1).unwrap().value(), Some("hello"));
        let input = find(2).unwrap();
        assert_eq!(input.role(), Role::TextInput);
        assert_eq!(input.placeholder(), Some("type here"));
        assert!(input.supports_action(Action::Focus));
        assert!(input.supports_action(Action::ReplaceSelectedText));
        // A laid-out node reports logical bounds.
        assert!(find(1).unwrap().bounds().unwrap().y1 > 0.0);

        // Focus action moves native focus.
        ui.a11y_action(&ActionRequest {
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: aid(NodeId(2)),
            data: None,
        });
        assert_eq!(ui.focused(), Some(NodeId(2)));
        let tree = ui.a11y_tree(Size::new(400.0, 300.0));
        assert_eq!(tree.focus, aid(NodeId(2)));
    }

    /// An assistive-tech Click runs the real pointer path — listeners
    /// see a down/up pair at the node's center.
    #[test]
    fn a11y_click_synthesizes_pointer() {
        use accesskit::{Action, ActionRequest, TreeId};
        use crate::a11y::aid;

        let mut enc = Encoder::new();
        let mut s = taffy::Style::default();
        s.size = taffy::Size {
            width: taffy::Dimension::length(50.0),
            height: taffy::Dimension::length(50.0),
        };
        enc.style(1, &s);
        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.props(0, mask::POINTER_DOWN | mask::POINTER_UP, false);
        enc.place(NIL, 0, NIL);
        let buf = enc.finish(1);

        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(400.0, 300.0));
        ui.a11y_action(&ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: aid(NodeId(0)),
            data: None,
        });
        let kinds: Vec<u8> = ui.take_events().iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&out_kind::POINTER_DOWN));
        assert!(kinds.contains(&out_kind::POINTER_UP));
    }

    /// ScrollIntoView walks ancestors and clamps offsets so the target
    /// lands inside the container's clip box.
    #[test]
    fn a11y_scroll_into_view() {
        use accesskit::{Action, ActionRequest, TreeId};
        use crate::a11y::aid;

        let mut enc = Encoder::new();
        let mut outer = taffy::Style::default();
        outer.display = taffy::Display::Flex;
        outer.overflow = taffy::Point {
            x: taffy::Overflow::Scroll,
            y: taffy::Overflow::Scroll,
        };
        outer.size = taffy::Size {
            width: taffy::Dimension::length(100.0),
            height: taffy::Dimension::length(100.0),
        };
        enc.style(1, &outer);
        let mut inner = taffy::Style::default();
        inner.size = taffy::Size {
            width: taffy::Dimension::length(100.0),
            height: taffy::Dimension::length(1000.0),
        };
        enc.style(2, &inner);
        let mut leaf = taffy::Style::default();
        leaf.position = taffy::Position::Absolute;
        leaf.inset = taffy::Rect {
            top: taffy::LengthPercentageAuto::length(900.0),
            left: taffy::LengthPercentageAuto::length(0.0),
            ..taffy::Rect::auto()
        };
        leaf.size = taffy::Size {
            width: taffy::Dimension::length(50.0),
            height: taffy::Dimension::length(50.0),
        };
        enc.style(3, &leaf);
        enc.create(0, KIND_VIEW);
        enc.set_style(0, 1);
        enc.place(NIL, 0, NIL);
        enc.create(1, KIND_VIEW);
        enc.set_style(1, 2);
        enc.place(0, 1, NIL);
        enc.create(2, KIND_VIEW);
        enc.set_style(2, 3);
        enc.place(1, 2, NIL);
        let buf = enc.finish(1);

        let mut ui = Ui::new(1.0);
        ui.apply(&buf).unwrap();
        ui.render(Size::new(400.0, 300.0));
        assert_eq!(ui.abs_rect(NodeId(2)).origin.y, 900.0);

        ui.a11y_action(&ActionRequest {
            action: Action::ScrollIntoView,
            target_tree: TreeId::ROOT,
            target_node: aid(NodeId(2)),
            data: None,
        });
        // Node's bottom (950) must land at the container's bottom (100).
        assert_eq!(ui.abs_rect(NodeId(2)).origin.y, 50.0);
    }
}
