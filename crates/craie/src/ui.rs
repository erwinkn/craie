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
        let roots: Vec<NodeId> = self.host.siblings(self.host.first_child(ROOT)).collect();
        for root in roots {
            layout::compute(
                &mut self.host,
                &mut self.layouts,
                &mut self.text,
                &mut self.texts,
                self.scale,
                root,
                size,
            );
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

        // (node, parent content-box origin in absolute px)
        let mut stack: Vec<(NodeId, f32, f32)> = Vec::new();
        let roots: Vec<NodeId> = self.host.siblings(self.host.first_child(ROOT)).collect();
        for &r in roots.iter().rev() {
            stack.push((r, 0.0, 0.0));
        }

        while let Some((id, ox, oy)) = stack.pop() {
            let Some(node) = self.host.node(id) else {
                continue;
            };
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

            // Children in reverse so the stack pops in tree order.
            let first = self.host.first_child(id);
            for c in self
                .host
                .siblings(first)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                stack.push((c, cx, cy));
            }
        }
    }
}
