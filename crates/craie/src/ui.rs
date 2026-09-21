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

use parley::Layout as ParleyLayout;

use crate::geom::{Point, Size};
use crate::host::{Host, NodeId, NodeKind, ROOT, StyleId};
use crate::layout::{self, EmittedText, Layouts, MeasuredText};
use crate::scene::{Color, Instance, Scene};
use crate::text::TextEngine;
use crate::wire::{self, Op, Txn, WireError};

pub struct Ui {
    pub host: Host,
    pub layouts: Layouts,
    pub text: TextEngine,
    /// Retained Parley layouts for TEXT nodes, indexed by node id.
    texts: Vec<Option<MeasuredText>>,
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
            scale,
            clear: 0x1415_18FF,
            seq: 0,
            scene: Scene::default(),
        }
    }

    /// Decodes and applies one wire transaction. Returns its seq.
    pub fn apply(&mut self, buf: &[u8]) -> Result<u64, WireError> {
        let txn = wire::decode(buf)?;
        self.apply_txn(&txn);
        Ok(txn.seq)
    }

    /// Applies an already-decoded transaction.
    pub fn apply_txn(&mut self, txn: &Txn<'_>) {
        txn.apply(&mut self.host, &mut self.layouts);
        // Slot reuse must drop the previous occupant's text layout.
        for op in &txn.ops {
            if let Op::Create { id, .. } | Op::Remove { id } = op {
                let slot = *id as usize;
                if slot < self.texts.len() {
                    self.texts[slot] = None;
                }
            }
        }
        self.seq = txn.seq;
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

    /// Depth-first paint pass: one ordered instance stream. Origins
    /// accumulate through each parent's content box (logical units) and
    /// convert to physical pixels at emission.
    fn paint_impl(&mut self, viewport: Size) {
        self.scene.items.clear();
        self.scene.clear = Some(Color(self.clear));

        // (node, parent content-box origin, logical). Children are pushed
        // in reverse so pops yield document pre-order — which is also
        // painter order.
        let mut stack: Vec<(NodeId, f32, f32)> = Vec::new();
        for &root in self.host.children(ROOT).iter().rev() {
            stack.push((root, 0.0, 0.0));
        }

        let scale = self.scale;
        while let Some((id, ox, oy)) = stack.pop() {
            let Some(node) = self.host.node(id) else {
                continue;
            };
            // `display: none` collapses layout but wouldn't stop paint on
            // its own — treat it like the hidden bit here too.
            let styled_away = self.layouts.style(StyleId(node.style)).display
                == taffy::Display::None;
            if node.hidden() || styled_away {
                continue;
            }
            let data = self.layouts.data(id);
            let x = ox + data.rect.origin.x;
            let y = oy + data.rect.origin.y;
            // Children are positioned relative to this node's content box.
            let cx = x + data.content[0];
            let cy = y + data.content[1];

            // Viewport culling: the node's own paint is skipped when its
            // border box misses the viewport (logical units). Children are
            // still visited — visible overflow can paint outside.
            let visible = x + data.rect.size.width > 0.0
                && y + data.rect.size.height > 0.0
                && x < viewport.width
                && y < viewport.height;
            if visible {
                match node.kind() {
                    NodeKind::VIEW => {
                        if let Some(view) = self.host.view(id)
                            && view.color & 0xFF != 0
                        {
                            // Round edges on the physical grid so the fill
                            // lands crisp at any scale.
                            let x0 = (x * scale).round();
                            let y0 = (y * scale).round();
                            let x1 = ((x + data.rect.size.width) * scale).round();
                            let y1 = ((y + data.rect.size.height) * scale).round();
                            self.scene
                                .items
                                .push(Instance::quad(x0, y0, x1 - x0, y1 - y0, view.color));
                        }
                    }
                    NodeKind::TEXT => self.paint_text(id, cx, cy),
                    _ => {}
                }
            }

            for &child in self.host.children(id).iter().rev() {
                stack.push((child, cx, cy));
            }
        }
    }

    /// Emits one text node: replays the cached instance batch when scale,
    /// origin and color all match — no shaping, rasterization, or cache
    /// lookups — and rewrites colors in place on a color-only change.
    fn paint_text(&mut self, id: NodeId, cx: f32, cy: f32) {
        let slot = id.0 as usize;
        let Some(row) = self.host.text(id) else {
            return;
        };
        let color = row.color;
        let scale_bits = self.scale.to_bits();
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
                self.scene.items.extend_from_slice(&e.instances);
                return;
            }
        }

        // Cold emit: produce physical-pixel instances, then retain a copy
        // keyed on (scale, origin, color) for the next repaint.
        let start = self.scene.items.len();
        self.text.emit(
            &m.layout,
            Point::new(cx, cy),
            self.scale,
            Some(Color(color)),
            &mut self.scene.items,
        );
        let instances = self.scene.items[start..].to_vec();
        m.emitted = Some(EmittedText {
            scale_bits,
            origin: [cx, cy],
            color,
            instances,
        });
    }
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
}
