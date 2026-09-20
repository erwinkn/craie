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
use crate::host::{Host, NodeId, NodeKind, ROOT};
use crate::layout::{self, Layouts, MeasuredText};
use crate::scene::{Color, QuadInstance, Scene};
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

    /// Recomputes layout for every root at `size` (physical pixels) and
    /// rebuilds the flat scene. Cheap on a clean tree — Taffy caches skip
    /// untouched subtrees — but callers should still gate on
    /// `needs_paint`.
    pub fn render(&mut self, size: Size) -> &Scene {
        let mut root = self.host.first_child(ROOT);
        while !root.is_nil() {
            // Read the next sibling before compute borrows the host.
            let next = self
                .host
                .node(root)
                .map(|n| NodeId(n.next_sibling))
                .unwrap_or(NodeId::NIL);
            layout::compute(
                &mut self.host,
                &mut self.layouts,
                &mut self.text,
                &mut self.texts,
                self.scale,
                root,
                size,
            );
            root = next;
        }
        self.paint();
        self.host.clear_paint_dirty();
        &self.scene
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

    /// Depth-first paint pass: quads for views, glyph instances for text.
    /// Origins accumulate through each parent's content box so node rects
    /// stay relative.
    fn paint(&mut self) {
        self.scene.quads.clear();
        self.scene.glyphs.clear();
        self.scene.clear = Some(Color(self.clear));

        // (node, parent content-box origin in absolute px). A node's next
        // sibling shares its parent origin; its first child gets the node's
        // own content origin. Sibling-then-child keeps pre-order with no
        // per-node allocation.
        let mut stack: Vec<(NodeId, f32, f32)> = Vec::new();
        let first_root = self.host.first_child(ROOT);
        if !first_root.is_nil() {
            stack.push((first_root, 0.0, 0.0));
        }

        while let Some((id, ox, oy)) = stack.pop() {
            let Some(node) = self.host.node(id) else {
                continue;
            };
            // Siblings share this node's parent origin — push the next one
            // before the hidden check so a hidden node doesn't truncate
            // the list.
            let next = NodeId(node.next_sibling);
            if !next.is_nil() {
                stack.push((next, ox, oy));
            }
            if node.hidden() {
                continue;
            }
            let data = self.layouts.data(id);
            let x = ox + data.rect.origin.x;
            let y = oy + data.rect.origin.y;
            // Children are positioned relative to this node's content box.
            let cx = x + data.content[0];
            let cy = y + data.content[1];

            match node.kind() {
                NodeKind::VIEW => {
                    if let Some(view) = self.host.view(id)
                        && view.color & 0xFF != 0
                    {
                        self.scene.quads.push(QuadInstance {
                            position: [x, y],
                            size: [data.rect.size.width, data.rect.size.height],
                            color: view.color,
                        });
                    }
                }
                NodeKind::TEXT => {
                    if let Some(m) = self.texts.get(id.0 as usize).and_then(|m| m.as_ref()) {
                        self.text
                            .emit(&m.layout, Point::new(cx, cy), &mut self.scene.glyphs);
                    }
                }
                _ => {}
            }

            let first = self.host.first_child(id);
            if !first.is_nil() {
                stack.push((first, cx, cy));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Encoder;

    const KIND_VIEW: u8 = 0;
    const KIND_TEXT: u8 = 1;
    const NIL: u32 = u32::MAX;

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
        let hidden_count = ui.render(Size::new(800.0, 600.0)).glyphs.len();
        assert!(hidden_count > 0);

        let mut enc = Encoder::new();
        enc.hidden(3, false);
        let buf = enc.finish(2);
        ui.apply(&buf).unwrap();
        let shown_count = ui.render(Size::new(800.0, 600.0)).glyphs.len();
        assert!(
            shown_count > hidden_count,
            "unhiding node 3 must add its glyphs: {hidden_count} -> {shown_count}"
        );
    }
}
