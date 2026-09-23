//! Craie verification harness.
//!
//! The central invariant: an incrementally updated `Ui` equals a clean
//! rebuild of the same final state. `snapshot` serializes a `Ui`'s host
//! state into one mount transaction; `compare` checks layout, the drawn
//! scene, hit tests, and the accessibility projection between two `Ui`s.
//! `Gen` produces seeded mutation sequences in the shape a reconciler
//! emits. `drain_uploads` takes the scene's pending GPU uploads the way
//! the renderer does, without a GPU, so cost tests can count bytes.

pub mod e01;

use std::collections::BTreeMap;

use craie_core::geom::{Affine, Rect, Size};
use craie_core::rng::Rng;
use craie_scene::{RasterId, Resolved, Scene};
use craie_ui::host::{NodeId, ROOT};
use craie_ui::mutation::{Mutation, NIL, NodeKind, Role, TextSpan, Transaction};
use craie_ui::ui::Ui;

/// Host spans as transaction spans: family indices move from the host's
/// family table to the transaction's.
pub fn txn_spans(
    t: &mut Transaction<'static>,
    host: &craie_ui::host::Host,
    spans: &[TextSpan],
) -> Vec<TextSpan> {
    spans
        .iter()
        .map(|s| TextSpan {
            family: match s.family {
                NIL => NIL,
                f => t.family(host.family_name(f).to_string()),
            },
            ..*s
        })
        .collect()
}

/// Serializes `ui`'s live nodes into one mount transaction with the same
/// ids: the "clean rebuild" input. Scroll offsets need layout extents and
/// are applied by `rebuild` after the first frame.
pub fn snapshot(ui: &Ui) -> Transaction<'static> {
    let host = &ui.host;
    let mut t = Transaction::new(1);
    let default = craie_ui::host::default_style();
    for i in 0..host.slot_count() {
        let id = NodeId(i as u32);
        let Some(node) = host.node(id) else { continue };
        let kind = node.kind;
        t.create(id.0, kind);
        let style = host.style(id);
        if *style != default {
            t.layout(id.0, style);
        }
        let s = host.spatial[id.index()];
        if s.transform != Affine::IDENTITY || s.opacity != 1.0 {
            t.push(Mutation::Spatial {
                id: id.0,
                transform: (s.transform != Affine::IDENTITY).then_some(s.transform),
                opacity: (s.opacity != 1.0).then_some(s.opacity),
            });
        }
        if kind.has_box() {
            let p = host.paint[id.index()];
            if p != Default::default() {
                t.paint(
                    id.0,
                    Some(p.fill),
                    Some(p.radius),
                    Some((p.border_color, p.border_width)),
                );
            }
        }
        if let Some(p) = host.paragraph(id) {
            let spans = txn_spans(&mut t, host, &p.spans);
            t.paragraph(id.0, p.text.clone(), &spans);
        }
        let it = host.interaction(id);
        if it.role != Role::None {
            t.role(id.0, it.role);
        }
        if it.listeners != 0 || it.focusable || it.selectable {
            t.interaction_flags(id.0, it.listeners, it.focusable, it.selectable);
        }
        if let Some(tr) = host.transitions.get(&id.0) {
            t.transition(id.0, tr);
        }
        if let Some(label) = host.label(id) {
            t.label(id.0, label.to_string());
        }
        if let Some(sd) = host.surfaces.get(&id.0) {
            t.surface(id.0, sd.kind, sd.params);
            if !sd.payload.is_empty() {
                t.payload(id.0, sd.payload.clone());
            }
        }
        if let Some(l) = host.lists.get(id.0) {
            t.list_config(id.0, l.overscan, l.fallback, &l.templates);
            t.list_splice(id.0, 0, 0, &l.descs);
        }
        if host.list_index[id.index()] != NIL {
            t.list_index(id.0, host.list_index[id.index()]);
        }
        let anchor = host.lists.policy(id.0);
        if anchor != Default::default() {
            t.scroll_anchor(id.0, anchor);
        }
    }
    for &root in host.children(ROOT) {
        t.append(NIL, root.0);
    }
    for i in 0..host.slot_count() {
        let id = NodeId(i as u32);
        if host.node(id).is_none() {
            continue;
        }
        for &c in host.children(id) {
            t.append(id.0, c.0);
        }
    }
    t
}

/// A fresh `Ui` built from `ui`'s state in one transaction, rendered at
/// `viewport`, with scroll offsets restored.
pub fn rebuild(ui: &Ui, viewport: Size) -> Ui {
    let mut clean = Ui::new(ui.scale);
    clean.clear = ui.clear;
    clean.apply_txn(&snapshot(ui)).expect("snapshot must apply");
    // The selection is native state: it carries over by node id.
    clean.set_text_selection(ui.text_selection());
    // Lists remember measured rows that are no longer rendered.
    for i in 0..ui.host.slot_count() {
        if ui.host.lists.get(i as u32).is_some() {
            let (width, m) = ui.host.lists.measurements(i as u32);
            clean.host.lists.restore_measurements(i as u32, width, &m);
        }
    }
    clean.render(viewport);
    let mut scrolled = false;
    for i in 0..ui.host.slot_count() {
        let id = NodeId(i as u32);
        if ui.host.node(id).is_none() {
            continue;
        }
        let [x, y] = ui.host.spatial[id.index()].scroll;
        if x != 0.0 || y != 0.0 {
            clean.scroll_to(id, x, y);
            scrolled = true;
        }
    }
    if scrolled {
        clean.render(viewport);
    }
    // A clean build's spaces start moving; when `ui` has everything at
    // rest, so does the rebuild.
    if ui.next_settle().is_none() {
        clean.set_time(crate::SETTLED);
        clean.settle();
        clean.render(viewport);
    }
    clean
}

/// `t` as it applies without animation, to `twin` (the state it reads
/// for fields an `Animate` does not name): transitions dropped, each
/// `Animate` a plain set of its target. At rest, a `Ui` that animates
/// must equal its twin.
pub fn without_animation(t: &Transaction<'static>, twin: &Ui) -> Transaction<'static> {
    use craie_ui::animation::Value;
    let mut out = t.clone();
    out.mutations.clear();
    for m in &t.mutations {
        match m {
            Mutation::Transition { .. } => {}
            Mutation::Animate { id, value, .. } => {
                let i = *id as usize;
                match *value {
                    Value::Transform(m) => {
                        out.transform(*id, m);
                    }
                    Value::Opacity(o) => {
                        out.opacity(*id, o);
                    }
                    Value::Color(c) if m_prop(m) == craie_ui::animation::Prop::Fill => {
                        out.fill(*id, c);
                    }
                    Value::Color(c) => {
                        let w = twin.host.paint[i].border_width;
                        out.paint(*id, None, None, Some((c, w)));
                    }
                    _ => {
                        let mut style = twin.host.layout[i].clone();
                        match *value {
                            Value::Size(d) if m_prop(m) == craie_ui::animation::Prop::Width => {
                                style.size.width = d
                            }
                            Value::Size(d) => style.size.height = d,
                            Value::Padding([l, r, t, b]) => {
                                style.padding = taffy::Rect {
                                    left: l,
                                    right: r,
                                    top: t,
                                    bottom: b,
                                }
                            }
                            Value::Gap([w, h]) => {
                                style.gap = taffy::Size {
                                    width: w,
                                    height: h,
                                }
                            }
                            _ => {}
                        }
                        out.layout(*id, &style);
                    }
                }
            }
            m => {
                out.mutations.push(m.clone());
            }
        }
    }
    out
}

fn m_prop(m: &Mutation<'_>) -> craie_ui::animation::Prop {
    match m {
        Mutation::Animate { prop, .. } => *prop,
        _ => unreachable!(),
    }
}

/// A clock time past any motion in a fresh `Ui`.
pub const SETTLED: f64 = craie_ui::ui::SETTLE_SECS * 2.0;

/// The drawn scene with glyphs keyed by a stable raster identity.
pub fn drawn(ui: &Ui) -> Vec<Resolved> {
    let text = &ui.text;
    ui.scene()
        .resolve_drawn(&|r: RasterId| text.stable_key(r).unwrap_or(u64::MAX))
}

/// One difference between two `Ui`s.
#[derive(Debug)]
pub struct Mismatch(pub String);

fn near(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol || (a.is_nan() && b.is_nan())
}

fn rect_near(a: Rect, b: Rect, tol: f32) -> bool {
    near(a.origin.x, b.origin.x, tol)
        && near(a.origin.y, b.origin.y, tol)
        && near(a.size.width, b.size.width, tol)
        && near(a.size.height, b.size.height, tol)
}

/// Nodes in the displayed tree (attached, not under `display: none`).
fn displayed(ui: &Ui) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = ui.host.children(ROOT).iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        if ui.host.node(id).is_none() || ui.host.display_none(id) {
            continue;
        }
        out.push(id);
        stack.extend(ui.host.children(id).iter().rev().copied());
    }
    out
}

/// Displayed text nodes under `id` (itself included), in tree order.
fn displayed_texts(ui: &Ui, id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if ui.host.node(id).is_none() || ui.host.display_none(id) {
            continue;
        }
        if ui.host.kind(id) == Some(NodeKind::Text) {
            out.push(id);
        }
        stack.extend(ui.host.children(id).iter().rev().copied());
    }
    out
}

/// Compares layout, the drawn scene, hit tests, and semantics.
/// `tol` is in logical units for layout and device px for the scene.
pub fn compare(a: &Ui, b: &Ui, viewport: Size, tol: f32) -> Result<(), Mismatch> {
    let nodes = displayed(a);
    if nodes != displayed(b) {
        return Err(Mismatch("displayed trees differ".into()));
    }
    for &id in &nodes {
        let (da, db) = (a.layouts.data(id), b.layouts.data(id));
        if !rect_near(da.rect, db.rect, tol) || !near(da.content[0], db.content[0], tol) {
            return Err(Mismatch(format!(
                "layout of {id:?}: {:?} vs {:?}",
                da.rect, db.rect
            )));
        }
    }
    let (sa, sb) = (drawn(a), drawn(b));
    if sa.len() != sb.len() {
        return Err(Mismatch(format!(
            "drawn primitive count {} vs {}",
            sa.len(),
            sb.len()
        )));
    }
    for (i, (pa, pb)) in sa.iter().zip(&sb).enumerate() {
        if !pa.close_to(pb, tol) {
            return Err(Mismatch(format!("primitive {i}: {pa:?} vs {pb:?}")));
        }
    }
    let step = 17.0;
    let mut y = 1.0;
    while y < viewport.height {
        let mut x = 1.0;
        while x < viewport.width {
            let (ha, hb) = (a.hit_test(x, y), b.hit_test(x, y));
            if ha != hb {
                return Err(Mismatch(format!("hit at ({x}, {y}): {ha:?} vs {hb:?}")));
            }
            x += step;
        }
        y += step;
    }
    if a.selection_ranges() != b.selection_ranges() || a.selected_text() != b.selected_text() {
        return Err(Mismatch(format!(
            "selection {:?} vs {:?}",
            a.selection_ranges(),
            b.selection_ranges()
        )));
    }
    let (ta, tb) = (semantic(a, viewport), semantic(b, viewport));
    if ta.len() != tb.len() {
        return Err(Mismatch("semantic trees differ in size".into()));
    }
    for (k, (ra, la, ba)) in &ta {
        let Some((rb, lb, bb)) = tb.get(k) else {
            return Err(Mismatch(format!("semantic node {k} missing")));
        };
        if ra != rb || la != lb || !rect_near(*ba, *bb, tol) {
            return Err(Mismatch(format!(
                "semantic node {k}: {ra:?}/{la:?}/{ba:?} vs {rb:?}/{lb:?}/{bb:?}"
            )));
        }
    }
    Ok(())
}

type SemanticRow = (accesskit::Role, Option<String>, Rect);

fn semantic(ui: &Ui, viewport: Size) -> BTreeMap<u64, SemanticRow> {
    ui.a11y_tree(viewport)
        .nodes
        .into_iter()
        .map(|(id, n)| {
            let b = n.bounds().map_or(Rect::ZERO, |r| {
                Rect::new(
                    r.x0 as f32,
                    r.y0 as f32,
                    (r.x1 - r.x0) as f32,
                    (r.y1 - r.y0) as f32,
                )
            });
            (id.0, (n.role(), n.label().map(str::to_string), b))
        })
        .collect()
}

/// Takes the scene's pending uploads exactly as the renderer does and
/// returns their size in bytes. Zero after an unchanged frame.
pub fn drain_uploads(scene: &mut Scene) -> u64 {
    fn bytes<T>(ranges: Vec<std::ops::Range<usize>>) -> u64 {
        ranges
            .iter()
            .map(|r| (r.len() * size_of::<T>()) as u64)
            .sum()
    }
    let mut n = 0;
    n += bytes::<craie_scene::RectInstance>(scene.rects.take_dirty());
    n += bytes::<craie_scene::GlyphInstance>(scene.glyphs.take_dirty());
    n += bytes::<u32>(scene.paints.take_dirty());
    n += bytes::<craie_scene::Placement>(scene.take_placement_dirty());
    n += bytes::<craie_scene::WorldGpu>(scene.transforms.take_gpu_dirty());
    n += bytes::<craie_scene::ClipGpu>(scene.clips.take_gpu_dirty());
    n += bytes::<craie_scene::RasterGpu>(scene.atlas.take_gpu_dirty());
    for color in [false, true] {
        let pages = if color {
            scene.atlas.color_pages()
        } else {
            scene.atlas.alpha_pages()
        };
        for p in 0..pages {
            if let (_, Some(r)) = scene.atlas.page_bytes(color, p) {
                n += ((r.max_x - r.min_x) * (r.max_y - r.min_y)) as u64;
            }
        }
    }
    scene.atlas.clear_page_dirty();
    n
}

// ------------------------------------------------------------ generator

const WORDS: &[&str] = &[
    "retained",
    "native",
    "state",
    "chunk",
    "glyph",
    "layout",
    "React",
    "frame",
    "paint",
    "scene",
    "atlas",
    "日本語",
    "مرحبا",
    "🎨",
    "wrap",
    "the",
    "a",
    "of",
];

/// A reconciler-shaped model of the tree: which ids are live, their kind
/// and parent, so generated mutations are valid.
#[derive(Default)]
struct Model {
    kind: BTreeMap<u32, NodeKind>,
    parent: BTreeMap<u32, u32>,
    children: BTreeMap<u32, Vec<u32>>,
    free: Vec<u32>,
    next: u32,
}

impl Model {
    fn alloc(&mut self) -> u32 {
        self.free.pop().unwrap_or_else(|| {
            self.next += 1;
            self.next - 1
        })
    }

    fn containers(&self) -> Vec<u32> {
        self.kind
            .iter()
            .filter(|(id, k)| **k == NodeKind::View && self.parent.contains_key(id))
            .map(|(id, _)| *id)
            .collect()
    }

    fn attached(&self) -> Vec<u32> {
        self.parent.keys().copied().collect()
    }

    fn subtree(&self, id: u32, out: &mut Vec<u32>) {
        out.push(id);
        for &c in self.children.get(&id).map(Vec::as_slice).unwrap_or(&[]) {
            self.subtree(c, out);
        }
    }
}

/// Seeded generator of valid mutation sequences.
pub struct Gen {
    pub rng: Rng,
    /// Selection choices draw from their own stream, so `step`'s
    /// sequences stay the same with or without `select`.
    sel: Rng,
    /// Animation choices (`animate`): their own stream too.
    anim: Rng,
    model: Model,
    seq: u64,
}

fn length(v: f32) -> taffy::Dimension {
    taffy::Dimension::length(v)
}

impl Gen {
    pub fn new(seed: u64) -> Gen {
        Gen {
            rng: Rng::new(seed),
            sel: Rng::new(seed ^ 0x5E1E_C7ED),
            anim: Rng::new(seed ^ 0xA41_3A7E),
            model: Model::default(),
            seq: 0,
        }
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.rng.below(xs.len() as u32) as usize]
    }

    fn color(&mut self) -> u32 {
        let base = self.pick(&[
            0x6DC7_FF00u32,
            0xB1E1_8A00,
            0xFFB4_6D00,
            0x2A2D_3800,
            0xECEC_F000,
        ]);
        base | self.pick(&[0xFFu32, 0xFF, 0x80, 0x00])
    }

    fn style(&mut self) -> taffy::Style {
        let mut s = taffy::Style::default();
        let r = &mut self.rng;
        if r.chance(0.5) {
            s.flex_direction = taffy::FlexDirection::Column;
        }
        if r.chance(0.3) {
            s.size.width = length(20.0 + r.below(200) as f32);
        }
        if r.chance(0.3) {
            s.size.height = length(10.0 + r.below(120) as f32);
        }
        if r.chance(0.3) {
            let p = taffy::LengthPercentage::length(r.below(12) as f32);
            s.padding = taffy::Rect {
                left: p,
                right: p,
                top: p,
                bottom: p,
            };
        }
        if r.chance(0.2) {
            s.gap = taffy::Size {
                width: taffy::LengthPercentage::length(4.0),
                height: taffy::LengthPercentage::length(4.0),
            };
        }
        if r.chance(0.15) {
            let pick = |r: &mut Rng| match r.below(3) {
                0 => taffy::Overflow::Scroll,
                1 => taffy::Overflow::Hidden,
                _ => taffy::Overflow::Visible,
            };
            let (x, y) = (pick(r), pick(r));
            // At least one axis clips.
            let y = if x == taffy::Overflow::Visible && y == taffy::Overflow::Visible {
                taffy::Overflow::Hidden
            } else {
                y
            };
            s.overflow = taffy::Point { x, y };
        }
        if r.chance(0.08) {
            s.display = taffy::Display::None;
        }
        if r.chance(0.08) {
            s.position = taffy::Position::Absolute;
            s.inset.left = taffy::LengthPercentageAuto::length(r.below(60) as f32);
            s.inset.top = taffy::LengthPercentageAuto::length(r.below(60) as f32);
        }
        s
    }

    fn text(&mut self) -> String {
        let n = 1 + self.rng.below(12) as usize;
        (0..n)
            .map(|_| self.pick(WORDS))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A family for a span: mostly the default, sometimes a named or
    /// generic family (interned in `t`).
    fn family(&mut self, t: &mut Transaction<'static>) -> u32 {
        match self.rng.below(6) {
            0 => t.family("monospace"),
            1 => t.family("Noto Sans"),
            _ => NIL,
        }
    }

    fn spans(&mut self, t: &mut Transaction<'static>, text: &str) -> Vec<TextSpan> {
        let size = self.pick(&[12.0, 14.0, 17.0, 22.0]);
        let mut spans = vec![TextSpan {
            font_size: size,
            color: self.color() | 0xFF,
            family: self.family(t),
            line_height: if self.rng.chance(0.2) {
                size * 1.6
            } else {
                0.0
            },
            letter_spacing: if self.rng.chance(0.2) { 1.5 } else { 0.0 },
            ..TextSpan::default()
        }];
        if self.rng.chance(0.3)
            && let Some((at, _)) = text.char_indices().nth(text.chars().count() / 2)
            && at > 0
        {
            spans.push(TextSpan {
                start: at as u32,
                font_size: size,
                color: self.color() | 0xFF,
                weight: 700,
                italic: self.rng.chance(0.5),
                decoration: self.rng.below(4) as u8,
                family: self.family(t),
                ..TextSpan::default()
            });
        }
        spans
    }

    /// The initial mount: a root column holding a few children.
    pub fn mount(&mut self) -> Transaction<'static> {
        self.seq += 1;
        let mut t = Transaction::new(self.seq);
        let root = self.model.alloc();
        let mut s = taffy::Style::default();
        s.flex_direction = taffy::FlexDirection::Column;
        s.size = taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        };
        t.create(root, NodeKind::View)
            .layout(root, &s)
            .fill(root, 0x1415_18FF)
            .append(NIL, root);
        self.model.kind.insert(root, NodeKind::View);
        self.model.parent.insert(root, NIL);
        for _ in 0..6 {
            self.op_create(&mut t);
        }
        t
    }

    fn op_create(&mut self, t: &mut Transaction<'static>) {
        let containers = self.model.containers();
        let parent = self.pick(&containers);
        let kind = self.pick(&[
            NodeKind::View,
            NodeKind::View,
            NodeKind::Text,
            NodeKind::Surface,
        ]);
        let id = self.model.alloc();
        t.create(id, kind);
        match kind {
            NodeKind::Text => {
                let text = self.text();
                let spans = self.spans(t, &text);
                t.paragraph(id, text, &spans);
            }
            NodeKind::Surface => {
                let n = 1 + self.rng.below(8);
                let bytes: Vec<u8> = (0..n).flat_map(|_| self.rng.unit().to_le_bytes()).collect();
                let c = self.color() | 0xFF;
                t.surface(id, craie_ui::surface::kind::BARS, [c, 0, 0, 0])
                    .payload(id, bytes);
                let mut s = taffy::Style::default();
                s.size = taffy::Size {
                    width: length(80.0),
                    height: length(30.0),
                };
                t.layout(id, &s);
            }
            _ => {
                let s = self.style();
                let c = self.color();
                t.layout(id, &s).fill(id, c);
            }
        }
        let siblings = self
            .model
            .children
            .get(&parent)
            .cloned()
            .unwrap_or_default();
        let before = if !siblings.is_empty() && self.rng.chance(0.3) {
            self.pick(&siblings)
        } else {
            NIL
        };
        t.place(parent, id, before);
        self.model.kind.insert(id, kind);
        self.model.parent.insert(id, parent);
        let list = self.model.children.entry(parent).or_default();
        match list.iter().position(|&c| c == before) {
            Some(p) => list.insert(p, id),
            None => list.push(id),
        }
    }

    fn op_remove(&mut self, t: &mut Transaction<'static>) {
        let candidates: Vec<u32> = self
            .model
            .attached()
            .into_iter()
            .filter(|id| self.model.parent[id] != NIL)
            .collect();
        if candidates.is_empty() {
            return;
        }
        let top = self.pick(&candidates);
        let mut all = Vec::new();
        self.model.subtree(top, &mut all);
        // The reconciler detaches the top, then removes every node.
        t.detach(top);
        let parent = self.model.parent[&top];
        self.model
            .children
            .get_mut(&parent)
            .unwrap()
            .retain(|&c| c != top);
        for id in all {
            t.remove(id);
            self.model.kind.remove(&id);
            self.model.parent.remove(&id);
            self.model.children.remove(&id);
            self.model.free.push(id);
        }
    }

    fn op_move(&mut self, t: &mut Transaction<'static>) {
        let attached: Vec<u32> = self
            .model
            .attached()
            .into_iter()
            .filter(|id| self.model.parent[id] != NIL)
            .collect();
        if attached.is_empty() {
            return;
        }
        let id = self.pick(&attached);
        let parent = self.model.parent[&id];
        let siblings = self.model.children[&parent].clone();
        let before = self.pick(&siblings);
        if before == id {
            return;
        }
        t.place(parent, id, before);
        let list = self.model.children.get_mut(&parent).unwrap();
        list.retain(|&c| c != id);
        let p = list.iter().position(|&c| c == before).unwrap();
        list.insert(p, id);
    }

    /// One transaction of 1-4 random mutations. `ui` supplies current
    /// paragraphs for color-only changes.
    pub fn step(&mut self, ui: &Ui) -> Transaction<'static> {
        self.seq += 1;
        let mut t = Transaction::new(self.seq);
        for _ in 0..1 + self.rng.below(4) {
            let nodes = self.model.attached();
            let id = self.pick(&nodes);
            let kind = self.model.kind[&id];
            match self.rng.below(12) {
                0 | 1 => self.op_create(&mut t),
                2 => self.op_remove(&mut t),
                3 => self.op_move(&mut t),
                4 if kind != NodeKind::Text => {
                    let s = self.style();
                    t.layout(id, &s);
                }
                5 if kind.has_box() => {
                    let c = self.color();
                    t.fill(id, c);
                }
                6 if kind.has_box() => {
                    let c = self.color() | 0xFF;
                    let w = self.pick(&[0.0, 1.0, 2.0]);
                    let r = self.pick(&[0.0, 4.0, 12.0]);
                    t.paint(id, None, Some(r), Some((c, w)));
                }
                7 if kind == NodeKind::Text => {
                    let text = self.text();
                    let spans = self.spans(&mut t, &text);
                    t.paragraph(id, text, &spans);
                }
                8 => {
                    let m = match self.rng.below(4) {
                        0 => Affine::IDENTITY,
                        1 => Affine::translate(
                            self.rng.below(40) as f32 - 20.0,
                            self.rng.below(40) as f32,
                        ),
                        2 => Affine::rotate(self.rng.unit() - 0.5),
                        _ => Affine::scale(0.5 + self.rng.unit(), 0.5 + self.rng.unit()),
                    };
                    t.transform(id, m);
                }
                9 => {
                    let o = self.pick(&[1.0, 1.0, 0.5, 0.25, 0.0]);
                    t.opacity(id, o);
                }
                10 => {
                    let r = self.pick(&[Role::None, Role::Button, Role::Group, Role::Heading]);
                    t.role(id, r);
                    if self.rng.chance(0.5) {
                        t.label(id, self.pick(WORDS).to_string());
                    }
                }
                11 if kind == NodeKind::Surface => {
                    let n = 1 + self.rng.below(8);
                    let bytes: Vec<u8> =
                        (0..n).flat_map(|_| self.rng.unit().to_le_bytes()).collect();
                    t.payload(id, bytes);
                }
                _ => {
                    let c = self.color() | 0xFF;
                    match ui.host.paragraph(NodeId(id)) {
                        // Color-only paragraph change: a paint patch. Only
                        // for nodes the ui already holds (not created in
                        // this transaction).
                        Some(p) if kind == NodeKind::Text && !p.spans.is_empty() => {
                            let text = p.text.clone();
                            let spans: Vec<TextSpan> = txn_spans(&mut t, &ui.host, &p.spans)
                                .into_iter()
                                .map(|s| TextSpan { color: c, ..s })
                                .collect();
                            t.paragraph(id, text, &spans);
                        }
                        _ if kind.has_box() => {
                            t.fill(id, c);
                        }
                        _ => {}
                    }
                }
            }
        }
        t
    }

    /// Selection churn: may toggle `selectable` on an attached node (a
    /// transaction), and may set a selection natively (as a press and
    /// drag would) in a selectable node's displayed texts.
    pub fn select(&mut self, ui: &mut Ui) -> Option<Transaction<'static>> {
        let nodes = self.model.attached();
        let mut out = None;
        if self.sel.chance(0.3) {
            let id = nodes[self.sel.below(nodes.len() as u32) as usize];
            let it = ui.host.interaction(NodeId(id));
            self.seq += 1;
            let mut t = Transaction::new(self.seq);
            t.interaction_flags(id, it.listeners, it.focusable, !it.selectable);
            out = Some(t);
        }
        if self.sel.chance(0.5) {
            let domains: Vec<u32> = nodes
                .into_iter()
                .filter(|&id| ui.host.interaction(NodeId(id)).selectable)
                .collect();
            if domains.is_empty() {
                return out;
            }
            let domain = domains[self.sel.below(domains.len() as u32) as usize];
            let texts = displayed_texts(ui, NodeId(domain));
            if texts.is_empty() {
                return out;
            }
            let point = |g: &mut Rng| {
                let node = texts[g.below(texts.len() as u32) as usize];
                let text = ui.host.paragraph(node).map_or("", |p| p.text.as_str());
                let mut i = g.below(text.len() as u32 + 1) as usize;
                while !text.is_char_boundary(i) {
                    i -= 1;
                }
                craie_ui::selection::TextPoint {
                    node,
                    offset: i as u32,
                }
            };
            let (anchor, focus) = (point(&mut self.sel), point(&mut self.sel));
            ui.set_text_selection(Some(craie_ui::selection::TextSelection {
                domain: NodeId(domain),
                anchor,
                focus,
            }));
        }
        out
    }

    /// Animation churn: may declare transitions on an attached node, and
    /// may start an `Animate` on one, with short curves and springs.
    pub fn animate(&mut self, ui: &Ui) -> Option<Transaction<'static>> {
        use craie_ui::animation::{Prop, Timing, Transition, Value};
        let nodes = self.model.attached();
        let g = &mut self.anim;
        let timing = |g: &mut Rng| {
            let delay = if g.chance(0.3) { 0.05 } else { 0.0 };
            if g.chance(0.3) {
                Timing::Spring {
                    delay,
                    stiffness: 150.0 + 100.0 * g.unit(),
                    damping: 10.0 + 20.0 * g.unit(),
                    mass: 1.0,
                }
            } else {
                Timing::curve(0.05 + 0.4 * g.unit(), [0.25, 0.1, 0.25, 1.0]).with_delay(delay)
            }
        };
        let mut t = Transaction::new(self.seq + 1);
        let mut any = false;
        if g.chance(0.3) {
            let id = nodes[g.below(nodes.len() as u32) as usize];
            let mut list = Vec::new();
            for k in 0..Prop::COUNT as u8 {
                if g.chance(0.4) {
                    list.push(Transition {
                        prop: Prop::from_u8(k).unwrap(),
                        timing: timing(g),
                    });
                }
            }
            t.transition(id, &list);
            any = true;
        }
        if g.chance(0.3) {
            let id = nodes[g.below(nodes.len() as u32) as usize];
            let has_box = ui.host.kind(NodeId(id)).is_some_and(|k| k.has_box());
            let len = taffy::LengthPercentage::length;
            let (prop, value) = match g.below(6) {
                0 => (
                    Prop::Transform,
                    Value::Transform(Affine::rotate(g.unit() - 0.5)),
                ),
                1 => (Prop::Opacity, Value::Opacity(g.unit())),
                2 if has_box => (Prop::Fill, Value::Color(0x3050_70FF | (g.below(255) << 24))),
                3 => (
                    Prop::Width,
                    Value::Size(taffy::Dimension::length(20.0 + 100.0 * g.unit())),
                ),
                4 => (Prop::Padding, Value::Padding([len(g.below(8) as f32); 4])),
                _ => (Prop::Gap, Value::Gap([len(g.below(8) as f32); 2])),
            };
            let tm = timing(g);
            t.animate(id, prop, value, tm);
            any = true;
        }
        if any {
            self.seq += 1;
            Some(t)
        } else {
            None
        }
    }

    /// Picks a scrollable node and an offset to scroll to (applied
    /// natively between frames, as wheel input would).
    pub fn scroll(&mut self, ui: &Ui) -> Option<(NodeId, f32, f32)> {
        let scrollers: Vec<u32> = self
            .model
            .attached()
            .into_iter()
            .filter(|id| ui.host.style(NodeId(*id)).overflow.y == taffy::Overflow::Scroll)
            .collect();
        if scrollers.is_empty() {
            return None;
        }
        let id = self.pick(&scrollers);
        Some((NodeId(id), 0.0, self.rng.below(200) as f32))
    }
}

/// The identity index oracle: every list's index equals a fresh sorted
/// projection of its items' non-NIL identities, which are unique.
pub fn check_list_index(ui: &Ui) -> Result<(), String> {
    for i in 0..ui.host.slot_count() as u32 {
        let Some(l) = ui.host.lists.get(i) else {
            continue;
        };
        let fresh = craie_ui::list::IdIndex::build(l.descs.iter().map(|d| d.id));
        if fresh.has_duplicates() {
            return Err(format!("list {i}: duplicate identities"));
        }
        if fresh != l.ids {
            let (a, b) = (l.ids.as_slice(), fresh.as_slice());
            let k = a
                .iter()
                .zip(b)
                .position(|(x, y)| x != y)
                .unwrap_or(a.len().min(b.len()));
            return Err(format!(
                "list {i}: index ({} ids) differs from items ({} ids) at {k}: {:?} vs {:?}",
                a.len(),
                b.len(),
                a.get(k),
                b.get(k)
            ));
        }
    }
    Ok(())
}

/// Plays the React side of a virtualized list: renders a row (a text
/// node) for every item in the range the list reports, and removes rows
/// that left it. `text(i)` is item `i`'s text.
pub struct ListDriver {
    pub list: u32,
    /// Rendered rows: item index -> node id.
    pub rows: BTreeMap<u32, u32>,
    next_id: u32,
    free: Vec<u32>,
    pub font_size: f32,
    seq: u64,
}

impl ListDriver {
    /// A driver for `list`; new row ids start at `first_id`.
    pub fn new(list: u32, first_id: u32, font_size: f32) -> ListDriver {
        ListDriver {
            list,
            rows: BTreeMap::new(),
            next_id: first_id,
            free: Vec::new(),
            font_size,
            seq: 1000,
        }
    }

    /// The item template matching the rows this driver renders.
    pub fn template(&self) -> craie_ui::mutation::ItemTemplate {
        craie_ui::mutation::ItemTemplate {
            base: 0.0,
            inset: 0.0,
            font_size: self.font_size,
        }
    }

    /// Renders until the list's range is stable (at most `max` commits).
    /// Returns the number of commits made.
    pub fn settle(
        &mut self,
        ui: &mut Ui,
        view: Size,
        text: &dyn Fn(u32) -> String,
        max: usize,
    ) -> usize {
        ui.render(view);
        for n in 0..max {
            if !self.pump(ui, text) {
                return n;
            }
            ui.render(view);
        }
        max
    }

    /// Applies the list's latest range event, if any, as one commit.
    /// Returns whether it committed.
    pub fn pump(&mut self, ui: &mut Ui, text: &dyn Fn(u32) -> String) -> bool {
        let mut want: Option<(u32, u32, i64)> = None;
        for e in ui.take_events() {
            if e.kind == craie_ui::events::out_kind::LIST_RANGE && e.node == self.list {
                want = Some((e.a as u32, e.b as u32, e.x as i64));
            }
        }
        let Some((first, end, keep)) = want else {
            return false;
        };
        let wanted = |i: u32| (first..end).contains(&i) || keep == i as i64;
        self.seq += 1;
        let mut t = Transaction::new(self.seq);
        let gone: Vec<u32> = self.rows.keys().copied().filter(|&i| !wanted(i)).collect();
        for i in gone {
            let id = self.rows.remove(&i).unwrap();
            t.remove(id);
            self.free.push(id);
        }
        let mut items: Vec<u32> = (first..end).collect();
        if keep >= 0 && !(first..end).contains(&(keep as u32)) {
            items.push(keep as u32);
        }
        for i in items {
            if self.rows.contains_key(&i) {
                continue;
            }
            let id = self.free.pop().unwrap_or_else(|| {
                self.next_id += 1;
                self.next_id - 1
            });
            t.create(id, NodeKind::Text)
                .text(id, text(i), self.font_size, 0xFFFF_FFFF)
                .list_index(id, i)
                .append(self.list, id);
            self.rows.insert(i, id);
        }
        ui.apply_txn(&t).expect("list rows apply");
        true
    }

    /// Renumbers rendered rows as keyed React rows follow their items:
    /// `to(old)` is the item's new index, or None when it was removed.
    pub fn remap(&mut self, t: &mut Transaction<'_>, to: &dyn Fn(u32) -> Option<u32>) {
        let old = std::mem::take(&mut self.rows);
        for (i, id) in old {
            match to(i) {
                Some(j) => {
                    if j != i {
                        t.list_index(id, j);
                    }
                    self.rows.insert(j, id);
                }
                None => {
                    t.remove(id);
                    self.free.push(id);
                }
            }
        }
    }

    /// Renumbers rendered rows after a splice (as keyed React rows do):
    /// rows of removed items go; later rows shift by `inserted - removed`.
    pub fn spliced(&mut self, t: &mut Transaction<'_>, at: u32, removed: u32, inserted: u32) {
        let old = std::mem::take(&mut self.rows);
        for (i, id) in old {
            if i < at {
                self.rows.insert(i, id);
            } else if i < at + removed {
                t.remove(id);
                self.free.push(id);
            } else {
                let j = i + inserted - removed;
                t.list_index(id, j);
                self.rows.insert(j, id);
            }
        }
    }
}
