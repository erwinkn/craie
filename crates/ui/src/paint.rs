//! Paint pass: host + layout -> the scene instance stream.

use crate::geom::{Point, Rect, Size};
use crate::host::{NodeId, ROOT};
use crate::layout::{EmittedText, MeasuredText};
use crate::mutation::NodeKind;
use crate::scene::{Color, Instance};
use crate::text::ParagraphSpec;
use crate::text::parley::style::StyleProperty;
use crate::ui::Ui;

/// A clip rect in logical coordinates, optionally rounded.
#[derive(Clone, Copy)]
struct Clip {
    min: [f32; 2],
    max: [f32; 2],
    radius: f32,
}

impl Ui {
    /// Intersects `clip` with `r` (absolute logical); keeps the tighter
    /// radius.
    fn clip_intersect(clip: Clip, r: Rect, radius: f32) -> Clip {
        let min = [clip.min[0].max(r.origin.x), clip.min[1].max(r.origin.y)];
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
    fn apply_clip(items: &mut Vec<Instance>, start: usize, clip: Option<Clip>, scale: f32) {
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
    pub(crate) fn paint_impl(&mut self, viewport: Size) {
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
            let kind = node.kind;
            let style = self.host.style(id);
            if style.display == taffy::Display::None {
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
                let radius = if kind.has_box() {
                    self.host.paint[id.index()].radius
                } else {
                    0.0
                };
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
            let visible = x + data.rect.size.width > 0.0
                && y + data.rect.size.height > 0.0
                && x < viewport.width
                && y < viewport.height;
            if visible {
                match kind {
                    NodeKind::View | NodeKind::Input | NodeKind::Surface => {
                        {
                            let view = self.host.paint[id.index()];
                            let paints_bg = view.fill & 0xFF != 0
                                || (view.border_width > 0.0 && view.border_color & 0xFF != 0);
                            if paints_bg {
                                // Round edges on the physical grid so the
                                // fill lands crisp at any scale.
                                let x0 = (x * scale).round();
                                let y0 = (y * scale).round();
                                let x1 = ((x + data.rect.size.width) * scale).round();
                                let y1 = ((y + data.rect.size.height) * scale).round();
                                let start = self.scene.items.len();
                                self.scene.items.push(Instance::rect(
                                    x0,
                                    y0,
                                    x1 - x0,
                                    y1 - y0,
                                    view.fill,
                                    view.radius * scale,
                                    view.border_width * scale,
                                    view.border_color,
                                ));
                                Self::apply_clip(&mut self.scene.items, start, clip, scale);
                            }
                        }
                        if kind == NodeKind::Input {
                            self.paint_input(id, cx, cy, clip, data);
                        }
                        if kind == NodeKind::Surface {
                            self.paint_surface(id, cx, cy, clip, data);
                        }
                    }
                    NodeKind::Text => self.paint_text(id, cx, cy, data, clip),
                }
            }

            // Children are translated by the scroll offset — an O(1)
            // paint-time shift; Taffy never sees it.
            let [sx, sy] = if scrolls {
                self.host.spatial[id.index()].scroll
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
    fn paint_input(
        &mut self,
        id: NodeId,
        cx: f32,
        cy: f32,
        clip: Option<Clip>,
        data: crate::layout::LayoutData,
    ) {
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

    /// Paints a surface node: its kind's native painter emits
    /// logical-space quads inside the content box; they are scaled to
    /// physical pixels and stamped with the inherited clip.
    fn paint_surface(
        &mut self,
        id: NodeId,
        cx: f32,
        cy: f32,
        clip: Option<Clip>,
        data: crate::layout::LayoutData,
    ) {
        let Some(spec) = self.host.surfaces.get(&id.0) else {
            return;
        };
        let Some(painter) = self.surface_painters.get_mut(&spec.kind) else {
            return;
        };
        let content = Rect::new(
            cx,
            cy,
            (data.rect.size.width - data.insets[0]).max(0.0),
            (data.rect.size.height - data.insets[1]).max(0.0),
        );
        self.surface_scratch.clear();
        painter(spec, content, &mut self.surface_scratch);
        let scale = self.scale;
        let start = self.scene.items.len();
        for q in self.surface_scratch.drain(..) {
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
        let Some(p) = self.host.paragraph(id) else {
            return;
        };
        let layout = crate::layout::shape_paragraph(&mut self.text, p, wrap);
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
    fn paint_text(
        &mut self,
        id: NodeId,
        cx: f32,
        cy: f32,
        data: crate::layout::LayoutData,
        clip: Option<Clip>,
    ) {
        let slot = id.0 as usize;
        let Some(p) = self.host.paragraph(id) else {
            return;
        };
        // Signature of every span color: a change re-emits (no reshape).
        let color = p.spans.iter().fold(0x811c_9dc5u32, |h, s| {
            (h ^ s.color).wrapping_mul(0x0100_0193)
        });
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

        if let Some(e) = &mut m.emitted
            && e.scale_bits == scale_bits
            && e.origin == [cx, cy]
            && e.color == color
        {
            {
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
            None,
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
