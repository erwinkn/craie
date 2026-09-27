//! The mutation executor: one path from a `Transaction` (decoded from
//! CRW2 or built by the Rust direct API) to host state.
//!
//! A transaction is validated whole against host state plus the effects
//! of its earlier mutations; on failure nothing applies. Application is
//! then infallible and runs no callbacks: it writes the host stores,
//! advances revisions, and queues dirty work for the frame pipeline.
//! Events produced by commands (focus, blur) are queued, not delivered.

use std::collections::HashMap;

use craie_core::geom::Affine;
use craie_layout::LayoutRow;

use crate::animation::{Prop, Value};
use crate::claims::{Claim, claim_kind};
use crate::host::{Host, MAX_NODES, NodeId};
use crate::list::{IdIndex, MAX_ITEMS};
use crate::mutation::{Command, ItemDesc, Mutation, NIL, NodeKind, TextSpan, Transaction};
use crate::ui::Ui;
use crate::wire::WireError;

/// How far past the slots in use a create may reach.
const ID_SLACK: u32 = 4096;

/// Largest accepted font size, logical points. Bounds the raster work of
/// one glyph; a glyph larger than an atlas page still renders (the text
/// engine rasterizes it smaller and draws it scaled up).
pub const MAX_FONT_SIZE: f32 = 2048.0;

fn font_size_ok(v: f32) -> bool {
    finite(v) && v > 0.0 && v <= MAX_FONT_SIZE
}

/// Largest accepted list distance (overscan, fallback, template parts),
/// logical points.
const MAX_LIST_LENGTH: f32 = 1.0e6;

fn length_ok(v: f32) -> bool {
    finite(v) && (0.0..=MAX_LIST_LENGTH).contains(&v)
}

fn invalid(why: &'static str) -> WireError {
    WireError::Invalid(why)
}

fn finite(v: f32) -> bool {
    v.is_finite()
}

/// Checks `spans` as a paragraph style list over `text`.
/// Spans per paragraph: pointer events carry `span + 1` in 16 key bits.
pub const MAX_SPANS: usize = 0xFFFF;

/// Claims per node: the wire's `u16` count.
pub const MAX_CLAIMS: usize = 0xFFFF;

fn valid_spans(text: &str, spans: &[TextSpan], families: usize) -> Result<(), WireError> {
    let Some(first) = spans.first() else {
        return Err(invalid("paragraph without spans"));
    };
    // Pointer events carry span + 1 in 16 bits (`dispatch`).
    if spans.len() > MAX_SPANS {
        return Err(invalid("paragraph with too many spans"));
    }
    if first.start != 0 {
        return Err(invalid("paragraph span zero must start at 0"));
    }
    let mut prev: Option<u32> = None;
    for s in spans {
        if prev.is_some_and(|p| s.start <= p) {
            return Err(invalid("paragraph spans out of order"));
        }
        if s.start as usize > text.len() || !text.is_char_boundary(s.start as usize) {
            return Err(invalid("paragraph span off a char boundary"));
        }
        if !font_size_ok(s.font_size) || !(1..=1000).contains(&s.weight) {
            return Err(invalid("paragraph span style out of range"));
        }
        // Spacing and line height within a font size's range; only the
        // two decoration bits; a family the transaction holds.
        let bound = MAX_FONT_SIZE;
        if !(s.letter_spacing.is_finite() && s.letter_spacing.abs() <= bound)
            || !(s.line_height.is_finite() && (0.0..=4.0 * bound).contains(&s.line_height))
            || s.decoration & !3 != 0
            || (s.family != NIL && s.family as usize >= families)
        {
            return Err(invalid("paragraph span style out of range"));
        }
        prev = Some(s.start);
    }
    Ok(())
}

/// A list the batch splices, as the batch leaves it.
enum Touch {
    /// The host's list plus splices recorded (item count after them).
    /// The first splice checks identities against the host's index; a
    /// second one on the same list materializes the sequence.
    Host {
        len: u32,
        splices: Vec<(u32, u32, Vec<u32>)>,
    },
    /// The whole identity sequence and its index.
    Seq { ids: Vec<u32>, index: IdIndex },
}

impl Touch {
    fn empty() -> Touch {
        Touch::Seq {
            ids: Vec::new(),
            index: IdIndex::default(),
        }
    }

    fn len(&self) -> u32 {
        match self {
            Touch::Host { len, .. } => *len,
            Touch::Seq { ids, .. } => ids.len() as u32,
        }
    }

    /// Checks one splice (range, item limit, identities unique across the
    /// list as the batch leaves it) and records it. `fresh` is the index
    /// of the inserted identities, already free of duplicates. Sorting
    /// and binary search only: wire ids never reach a hash.
    fn splice(
        &mut self,
        base: Option<&crate::list::ListState>,
        at: u32,
        remove: u32,
        inserted: Vec<u32>,
        fresh: &IdIndex,
    ) -> Result<(), WireError> {
        let cur = self.len();
        if at > cur || remove > cur - at {
            return Err(invalid("list splice out of range"));
        }
        let next = cur as u64 - remove as u64 + inserted.len() as u64;
        if next > MAX_ITEMS as u64 {
            return Err(invalid("list longer than the item limit"));
        }
        let range = at as usize..(at + remove) as usize;
        let clash = |index: &IdIndex, removed: &IdIndex| {
            fresh
                .as_slice()
                .iter()
                .any(|&i| index.contains(i) && !removed.contains(i))
        };
        if let Touch::Host { splices, .. } = self {
            let base = base.expect("a host touch has a host list");
            if splices.is_empty() {
                // First splice: the host's items and index, unmodified.
                let removed = IdIndex::build(base.descs[range].iter().map(|d| d.id));
                if clash(&base.ids, &removed) {
                    return Err(invalid("duplicate item identity in a list"));
                }
                splices.push((at, remove, inserted));
                *self = Touch::Host {
                    len: next as u32,
                    splices: std::mem::take(splices),
                };
                return Ok(());
            }
            // Materialize: the host's sequence with the recorded splices.
            let mut ids: Vec<u32> = base.descs.iter().map(|d| d.id).collect();
            for (a, r, ins) in splices.drain(..) {
                ids.splice(a as usize..(a + r) as usize, ins);
            }
            let index = IdIndex::build(ids.iter().copied());
            *self = Touch::Seq { ids, index };
        }
        let Touch::Seq { ids, index } = self else {
            unreachable!()
        };
        let removed = IdIndex::build(ids[range.clone()].iter().copied());
        if clash(index, &removed) {
            return Err(invalid("duplicate item identity in a list"));
        }
        index.update(removed.as_slice(), fresh.as_slice());
        ids.splice(range, inserted);
        Ok(())
    }
}

/// Liveness and parent overlay over the host for validation.
struct Overlay<'h> {
    host: &'h Host,
    kinds: HashMap<u32, Option<NodeKind>>,
    /// Parent links set by the batch, with the step that set them.
    parents: HashMap<u32, (u32, usize)>,
    /// Nodes removed by the batch, with the step of the last removal. A
    /// link made before its parent's removal is an orphan (the host
    /// detaches the children of a removed node).
    removed: HashMap<u32, usize>,
    /// Creates validated so far in the batch.
    created: u32,
}

impl Overlay<'_> {
    fn kind(&self, id: u32) -> Option<NodeKind> {
        match self.kinds.get(&id) {
            Some(k) => *k,
            None => self.host.kind(NodeId(id)),
        }
    }

    fn live(&self, id: u32) -> bool {
        self.kind(id).is_some()
    }

    fn parent(&self, id: u32) -> u32 {
        let (p, set_at) = match self.parents.get(&id) {
            Some(&(p, step)) => (p, Some(step)),
            None => match self.host.node(NodeId(id)) {
                Some(n) => (n.parent, None),
                None => return NodeId::DETACHED.0,
            },
        };
        match (self.removed.get(&p), set_at) {
            // Host links predate every removal in the batch.
            (Some(_), None) => NodeId::DETACHED.0,
            (Some(&removed_at), Some(step)) if removed_at > step => NodeId::DETACHED.0,
            _ => p,
        }
    }
}

/// Validates the whole transaction. O(mutations); no host mutation.
pub fn validate(host: &Host, txn: &Transaction<'_>) -> Result<(), WireError> {
    let mut o = Overlay {
        host,
        kinds: HashMap::new(),
        parents: HashMap::new(),
        removed: HashMap::new(),
        created: 0,
    };
    // Lists the batch splices, as the batch leaves them (item counts and
    // identities).
    let mut lists: HashMap<u32, Touch> = HashMap::new();
    let need_live = |o: &Overlay, id: u32, why: &'static str| {
        if o.live(id) {
            Ok(())
        } else {
            Err(invalid(why))
        }
    };
    for (step, m) in txn.mutations.iter().enumerate() {
        match m {
            Mutation::Create { id, kind } => {
                // Ids index dense stores: a bound keeps one op from
                // growing them without limit.
                if *id >= MAX_NODES {
                    return Err(invalid("create beyond the node id limit"));
                }
                // Ids stay dense: the bridge allocates them in order and
                // recycles freed ones, so a batch never names an id far
                // past the slots in use plus the nodes it creates. Memory
                // grows with nodes sent, not with the largest id named.
                o.created += 1;
                if *id >= host.slot_count() as u32 + o.created + ID_SLACK {
                    return Err(invalid("create leaves a gap in node ids"));
                }
                if o.live(*id) {
                    return Err(invalid("create over a live node"));
                }
                o.kinds.insert(*id, Some(*kind));
                o.parents.insert(*id, (NodeId::DETACHED.0, step));
                // A node created here holds no items, whatever an earlier
                // occupant of the id held.
                lists.insert(*id, Touch::empty());
            }
            Mutation::Place {
                parent,
                child,
                before,
            } => {
                need_live(&o, *child, "place of an absent child")?;
                if *parent != NIL {
                    need_live(&o, *parent, "place under an absent parent")?;
                }
                if *before != NIL {
                    need_live(&o, *before, "place before an absent sibling")?;
                    if o.parent(*before) != *parent || before == child {
                        return Err(invalid("place before a node of another parent"));
                    }
                }
                // Walking ancestors of `parent` must never reach `child`.
                let mut cur = *parent;
                let mut hops = 0usize;
                while cur != NIL && cur != NodeId::DETACHED.0 {
                    if cur == *child {
                        return Err(invalid("place would create a cycle"));
                    }
                    cur = o.parent(cur);
                    hops += 1;
                    if hops > host.slot_count() + o.kinds.len() + 1 {
                        return Err(invalid("corrupt parent chain"));
                    }
                }
                o.parents.insert(*child, (*parent, step));
            }
            Mutation::Detach { id } => {
                need_live(&o, *id, "detach of an absent node")?;
                o.parents.insert(*id, (NodeId::DETACHED.0, step));
            }
            Mutation::Remove { id } => {
                need_live(&o, *id, "remove of an absent node")?;
                o.kinds.insert(*id, None);
                o.parents.insert(*id, (NodeId::DETACHED.0, step));
                o.removed.insert(*id, step);
                lists.insert(*id, Touch::empty());
            }
            Mutation::Layout { id, style } => {
                need_live(&o, *id, "layout on an absent node")?;
                if *style != NIL && *style as usize >= txn.styles.len() {
                    return Err(invalid("layout style out of range"));
                }
            }
            Mutation::Spatial {
                id,
                transform,
                opacity,
                ..
            } => {
                need_live(&o, *id, "spatial on an absent node")?;
                if transform.is_some_and(|t| !t.0.iter().all(|v| v.is_finite())) {
                    return Err(invalid("non-finite transform"));
                }
                if opacity.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
                    return Err(invalid("opacity outside [0, 1]"));
                }
            }
            Mutation::Layer { id, owner } => {
                need_live(&o, *id, "layer on an absent node")?;
                if *owner != NIL {
                    need_live(&o, *owner, "layer owned by an absent node")?;
                    if owner == id {
                        return Err(invalid("layer owned by itself"));
                    }
                }
            }
            Mutation::Paint {
                id, radius, border, ..
            } => {
                if !o.kind(*id).is_some_and(NodeKind::has_box) {
                    return Err(invalid("paint on a node without a box"));
                }
                if radius.is_some_and(|r| !finite(r)) || border.is_some_and(|(_, w)| !finite(w)) {
                    return Err(invalid("non-finite paint"));
                }
            }
            Mutation::Paragraph { id, text, spans } => {
                if o.kind(*id) != Some(NodeKind::Text) {
                    return Err(invalid("paragraph on a non-text node"));
                }
                let Some(spans) = txn.spans.get(spans.start as usize..spans.end as usize) else {
                    return Err(invalid("paragraph spans out of range"));
                };
                valid_spans(text, spans, txn.families.len())?;
            }
            Mutation::InputConfig { id, font_size, .. } => {
                if o.kind(*id) != Some(NodeKind::Input) {
                    return Err(invalid("input config on a non-input node"));
                }
                if !font_size_ok(*font_size) {
                    return Err(invalid("input font size out of range"));
                }
            }
            Mutation::Role { id, .. }
            | Mutation::Label { id, .. }
            | Mutation::Interaction { id, .. } => {
                need_live(&o, *id, "semantics on an absent node")?;
            }
            Mutation::Claims { id, claims, .. } => {
                // The window list (NIL) holds key claims only.
                if *id != NIL {
                    need_live(&o, *id, "claims on an absent node")?;
                } else if claims.iter().any(|c| c.kind != claim_kind::KEY) {
                    return Err(invalid("a window claim that is not a key"));
                }
                if claims.len() > MAX_CLAIMS {
                    return Err(invalid("too many claims on one node"));
                }
                if !claims.iter().all(Claim::valid) {
                    return Err(invalid("malformed claim"));
                }
            }
            Mutation::Surface { id, .. } => {
                if o.kind(*id) != Some(NodeKind::Surface) {
                    return Err(invalid("surface data on a non-surface node"));
                }
            }
            Mutation::Payload { id, bytes } => match o.kind(*id) {
                Some(NodeKind::Surface) => {}
                // A vector's payload is its asset: it must decode.
                Some(NodeKind::Vector) => {
                    if craie_vector::asset::decode(bytes).is_err() {
                        return Err(invalid("vector asset does not decode"));
                    }
                }
                _ => return Err(invalid("payload on a node without one")),
            },
            Mutation::Command { id, cmd } => {
                // The clipboard is the window's: NIL may write it.
                if !(*id == NIL && matches!(cmd, Command::WriteClipboard(_))) {
                    need_live(&o, *id, "command on an absent node")?;
                }
                match cmd {
                    Command::SetText(_) if o.kind(*id) != Some(NodeKind::Input) => {
                        return Err(invalid("set text on a non-input node"));
                    }
                    Command::ScrollTo(x, y) if !finite(*x) || !finite(*y) => {
                        return Err(invalid("non-finite scroll offset"));
                    }
                    _ => {}
                }
            }
            Mutation::ListConfig {
                id,
                overscan,
                fallback,
                templates,
            } => {
                if o.kind(*id) != Some(NodeKind::List) {
                    return Err(invalid("list config on a non-list node"));
                }
                if !length_ok(*overscan) || !length_ok(*fallback) {
                    return Err(invalid("list overscan or fallback out of range"));
                }
                for t in templates.iter() {
                    if !length_ok(t.base)
                        || !length_ok(t.inset)
                        || !(t.font_size == 0.0 || font_size_ok(t.font_size))
                    {
                        return Err(invalid("list template out of range"));
                    }
                }
            }
            Mutation::ListSplice {
                id,
                at,
                remove,
                items,
            } => {
                if o.kind(*id) != Some(NodeKind::List) {
                    return Err(invalid("list splice on a non-list node"));
                }
                // Whole descriptions with only known flag bits (the direct
                // API can hand over raw bytes too).
                if items.len() % ItemDesc::BYTES != 0 {
                    return Err(invalid("list items not whole descriptions"));
                }
                if items
                    .chunks_exact(ItemDesc::BYTES)
                    .any(|c| c[ItemDesc::BYTES - 1] & !ItemDesc::UNCHANGED != 0)
                {
                    return Err(invalid("unknown list item flags"));
                }
                let inserted: Vec<u32> = ItemDesc::iter(items).map(|d| d.id).collect();
                let fresh = IdIndex::build(inserted.iter().copied());
                if fresh.has_duplicates() {
                    return Err(invalid("duplicate item identity in a splice"));
                }
                let base = host.lists.get(*id);
                let touch = lists.entry(*id).or_insert_with(|| match base {
                    // A list created in this batch starts empty.
                    None => Touch::empty(),
                    Some(l) => Touch::Host {
                        len: l.len(),
                        splices: Vec::new(),
                    },
                });
                touch.splice(base, *at, *remove, inserted, &fresh)?;
            }
            Mutation::ListIndex { id, .. } => {
                need_live(&o, *id, "list index on an absent node")?;
            }
            Mutation::ScrollAnchor { id, .. } => {
                need_live(&o, *id, "scroll anchor on an absent node")?;
            }
            Mutation::Transition { id, transitions } => {
                need_live(&o, *id, "transition on an absent node")?;
                if transitions.len() > Prop::COUNT {
                    return Err(invalid("too many transitions"));
                }
                for (k, t) in transitions.iter().enumerate() {
                    if !t.timing.is_valid() {
                        return Err(invalid("transition timing out of range"));
                    }
                    if transitions[..k].iter().any(|u| u.prop == t.prop) {
                        return Err(invalid("transition declared twice"));
                    }
                }
            }
            Mutation::Animate {
                id,
                prop,
                value,
                timing,
            } => {
                need_live(&o, *id, "animate on an absent node")?;
                if prop.is_paint() && !o.kind(*id).is_some_and(NodeKind::has_box) {
                    return Err(invalid("paint animation on a node without a box"));
                }
                if !timing.is_valid() {
                    return Err(invalid("animation timing out of range"));
                }
                if !valid_target(*prop, value) {
                    return Err(invalid("animation target out of range"));
                }
            }
        }
    }
    Ok(())
}

/// An `Animate` target: the value kind of `prop`, finite, lengths (not
/// percents or keywords) in range.
fn valid_target(prop: Prop, value: &Value) -> bool {
    let len = |v: Option<f32>| v.is_some_and(|v| v.is_finite() && (0.0..=1e6).contains(&v));
    let lp = |l: &taffy::LengthPercentage| {
        len(match l.expand() {
            taffy::style::ExpandedLengthPercentage::Length(v) => Some(v),
            _ => None,
        })
    };
    match (prop, value) {
        (Prop::Transform, Value::Transform(t)) => t.0.iter().all(|v| v.is_finite()),
        (Prop::Opacity, Value::Opacity(o)) => (0.0..=1.0).contains(o),
        (Prop::Fill | Prop::BorderColor, Value::Color(_)) => true,
        (Prop::Width | Prop::Height, Value::Size(d)) => len(match d.expand() {
            taffy::style::ExpandedDimension::Length(v) => Some(v),
            _ => None,
        }),
        (Prop::Padding, Value::Padding(p)) => p.iter().all(lp),
        (Prop::Gap, Value::Gap(g)) => g.iter().all(lp),
        _ => false,
    }
}

impl Ui {
    /// Validates and applies one transaction atomically. On error
    /// nothing changes.
    pub fn execute(&mut self, txn: &Transaction<'_>) -> Result<(), WireError> {
        validate(&self.host, txn)?;
        for m in &txn.mutations {
            self.apply_mutation(txn, m);
        }
        self.seq = txn.seq;
        Ok(())
    }

    fn apply_mutation(&mut self, txn: &Transaction<'_>, m: &Mutation<'_>) {
        match m {
            Mutation::Create { id, kind } => {
                let node = NodeId(*id);
                self.host.create(node, *kind);
                self.forget_node_state(node);
            }
            Mutation::Place {
                parent,
                child,
                before,
            } => {
                let child = NodeId(*child);
                if self.host.parent(child) != NodeId(*parent) {
                    self.unhover(child);
                }
                self.host
                    .insert_before(NodeId(*parent), child, NodeId(*before));
            }
            Mutation::Detach { id } => {
                self.unhover(NodeId(*id));
                self.host.detach(NodeId(*id));
            }
            Mutation::Remove { id } => {
                let node = NodeId(*id);
                // Its tweens end before the slot's generation moves.
                self.end_animations_of(node, crate::animation::end_reason::REMOVED);
                self.unhover(node);
                self.host.remove(node);
                self.forget_node_state(node);
                for slot in [&mut self.focus, &mut self.pressed] {
                    if *slot == Some(node) {
                        *slot = None;
                    }
                }
                self.pending_scrolls.retain(|(n, _, _)| *n != node);
            }
            Mutation::Layout { id, style } => {
                let node = NodeId(*id);
                let mut new = if *style == NIL {
                    crate::host::default_style().to_taffy()
                } else {
                    txn.styles[*style as usize].clone()
                };
                // Animated fields: a transition tweens to the new value,
                // so the row keeps the value on screen.
                for prop in [Prop::Width, Prop::Height, Prop::Padding, Prop::Gap] {
                    let next = layout_field(&new, prop);
                    if !self.intercept(node, prop, next) {
                        let current = self.row_value(node, prop);
                        set_layout_field(&mut new, prop, current);
                    }
                }
                self.set_layout(node, LayoutRow::from(&new));
            }
            Mutation::Spatial {
                id,
                transform,
                opacity,
                z,
            } => {
                let node = NodeId(*id);
                // Not animatable: a z change reorders at once.
                if let Some(z) = z {
                    self.host.set_z(node, *z);
                }
                let transform = transform
                    .filter(|t| self.intercept(node, Prop::Transform, Value::Transform(*t)));
                let opacity =
                    opacity.filter(|o| self.intercept(node, Prop::Opacity, Value::Opacity(*o)));
                self.set_spatial(node, transform, opacity);
            }
            Mutation::Layer { id, owner } => {
                self.host.set_layer(NodeId(*id), *owner);
            }
            Mutation::Paint {
                id,
                fill,
                radius,
                border,
            } => {
                let node = NodeId(*id);
                let fill = fill.filter(|c| self.intercept(node, Prop::Fill, Value::Color(*c)));
                let border_color = border
                    .map(|(c, _)| c)
                    .filter(|c| self.intercept(node, Prop::BorderColor, Value::Color(*c)));
                self.set_paint(node, fill, *radius, border_color, border.map(|(_, w)| w));
            }
            Mutation::Transition { id, transitions } => {
                // Later changes use the new set; running tweens finish.
                if transitions.is_empty() {
                    self.host.transitions.remove(id);
                } else {
                    self.host.transitions.insert(*id, transitions.to_vec());
                }
            }
            Mutation::Animate {
                id,
                prop,
                value,
                timing,
            } => {
                let node = NodeId(*id);
                if self.start_animation(node, *prop, *value, *timing, true) {
                    // Applied at once: it ends now.
                    self.write_value(node, *prop, *value);
                    self.report_end(node, *prop, crate::animation::end_reason::FINISHED);
                }
            }
            Mutation::Paragraph { id, text, spans } => {
                let node = NodeId(*id);
                // Families in host indices.
                let mut next = std::mem::take(&mut self.span_scratch);
                next.clear();
                for s in &txn.spans[spans.start as usize..spans.end as usize] {
                    let family = match s.family {
                        NIL => NIL,
                        f => self.host.family(&txn.families[f as usize]),
                    };
                    next.push(TextSpan { family, ..*s });
                }
                let spans = &next[..];
                let p = &mut self.host.paragraphs[node.index()];
                p.revision = p.revision.wrapping_add(1);
                let text_changed = p.text != *text;
                // Fonts not resolved yet (a fresh node): resolve now,
                // even when text and spans equal the defaults.
                let metrics_changed = text_changed
                    || p.fonts.len() != spans.len()
                    || p.spans.len() != spans.len()
                    || p.spans.iter().zip(spans).any(|(a, b)| !a.same_metrics(b));
                let colors_changed = p.spans.iter().zip(spans).any(|(a, b)| a.color != b.color);
                let decorations_changed = p
                    .spans
                    .iter()
                    .zip(spans)
                    .any(|(a, b)| a.decoration != b.decoration);
                if text_changed {
                    p.text.clear();
                    p.text.push_str(text);
                    self.host.copied_bytes += text.len() as u64;
                }
                if metrics_changed || colors_changed || decorations_changed {
                    p.spans.clear();
                    p.spans.extend_from_slice(spans);
                    self.host.copied_bytes += std::mem::size_of_val(spans) as u64;
                }
                if metrics_changed {
                    // Each span's family resolves to a font once, here.
                    let p = &mut self.host.paragraphs[node.index()];
                    p.fonts.clear();
                    for s in spans {
                        let name = self
                            .host
                            .families
                            .get(s.family as usize)
                            .map_or("", String::as_str);
                        p.fonts.push(self.text.font(name, s.weight, s.italic));
                    }
                }
                self.span_scratch = next;
                if text_changed {
                    self.host.revs.text_content.bump();
                    self.host.dirty.semantic.push(*id);
                }
                if metrics_changed {
                    self.host.revs.text_metrics.bump();
                    self.host.mark_text(node);
                } else if decorations_changed {
                    // Decorations are drawn rects: a chunk rebuild, no
                    // reshape.
                    self.host.revs.paint.bump();
                    self.host.dirty.content.push(*id);
                } else if colors_changed {
                    // Color lives in the paint records: no reshape.
                    self.host.revs.paint.bump();
                    self.host.dirty.paint.push(*id);
                }
            }
            Mutation::InputConfig {
                id,
                font_size,
                color,
                placeholder,
                multiline,
                submit,
            } => {
                let metrics =
                    self.inputs
                        .configure(*id, *font_size, *color, placeholder, *multiline);
                if let Some(state) = self.inputs.get_mut(*id) {
                    state.submit = *submit;
                }
                if metrics {
                    self.host.mark_layout(NodeId(*id));
                    self.host.revs.text_metrics.bump();
                    self.host.dirty.content.push(*id);
                    self.host.dirty.semantic.push(*id);
                } else {
                    // Color only: patch the chunk's paint records.
                    self.host.revs.paint.bump();
                    self.host.dirty.paint.push(*id);
                }
            }
            Mutation::Role { id, role } => {
                let i = &mut self.host.interaction[*id as usize];
                if i.role != *role {
                    i.role = *role;
                    self.host.revs.semantic.bump();
                    self.host.dirty.semantic.push(*id);
                }
            }
            Mutation::Label { id, text } => {
                let changed = if text.is_empty() {
                    self.host.labels.remove(id).is_some()
                } else if self.host.labels.get(id).map(|l| &**l) != Some(&**text) {
                    self.host.labels.insert(*id, (**text).into());
                    self.host.copied_bytes += text.len() as u64;
                    true
                } else {
                    false
                };
                if changed {
                    self.host.revs.semantic.bump();
                    self.host.dirty.semantic.push(*id);
                }
            }
            Mutation::Interaction {
                id,
                listeners,
                focusable,
                selectable,
            } => {
                let i = &mut self.host.interaction[*id as usize];
                if i.listeners != *listeners
                    || i.focusable != *focusable
                    || i.selectable != *selectable
                {
                    i.listeners = *listeners;
                    i.focusable = *focusable;
                    i.selectable = *selectable;
                    self.host.revs.semantic.bump();
                    self.host.dirty.semantic.push(*id);
                }
            }
            Mutation::Claims {
                id,
                version,
                claims,
            } => {
                if claims.is_empty() {
                    self.host.claims.remove(id);
                } else {
                    let set = self.host.claims.entry(*id).or_default();
                    set.version = *version;
                    set.claims.clear();
                    set.claims.extend_from_slice(claims);
                }
            }
            Mutation::Surface { id, kind, params } => {
                let s = self.host.surfaces.entry(*id).or_default();
                if s.kind != *kind || s.params != *params {
                    s.kind = *kind;
                    s.params = *params;
                    self.host.revs.resource.bump();
                    self.host.dirty.content.push(*id);
                }
            }
            Mutation::Payload { id, bytes }
                if self.host.kind(NodeId(*id)) == Some(NodeKind::Vector) =>
            {
                let v = self.host.vectors.entry(*id).or_default();
                if v.bytes[..] != bytes[..] {
                    // Validated: it decodes.
                    let asset = craie_vector::asset::decode(bytes)
                        .ok()
                        .map(std::sync::Arc::new);
                    v.bytes.clear();
                    v.bytes.extend_from_slice(bytes);
                    v.asset = asset;
                    self.host.copied_bytes += bytes.len() as u64;
                    self.host.revs.resource.bump();
                    self.host.dirty.content.push(*id);
                    // The view box is its intrinsic size.
                    self.host.revs.layout_input.bump();
                    self.host.mark_layout(NodeId(*id));
                    self.vector_meshes.remove(id);
                }
            }
            Mutation::Payload { id, bytes } => {
                let s = self.host.surfaces.entry(*id).or_default();
                if s.payload[..] != bytes[..] {
                    s.payload.clear();
                    s.payload.extend_from_slice(bytes);
                    self.host.copied_bytes += bytes.len() as u64;
                    self.host.revs.resource.bump();
                    self.host.dirty.content.push(*id);
                }
            }
            Mutation::Command { id, cmd } => self.command(NodeId(*id), cmd),
            Mutation::ListConfig {
                id,
                overscan,
                fallback,
                templates,
            } => {
                self.host
                    .lists
                    .configure(*id, *overscan, *fallback, templates);
                self.host.revs.layout_input.bump();
                self.host.mark_layout(NodeId(*id));
            }
            Mutation::ListSplice {
                id,
                at,
                remove,
                items,
            } => {
                self.host.copied_bytes += items.len() as u64;
                self.host
                    .lists
                    .splice(&mut self.text, *id, *at, *remove, items);
                self.host.revs.layout_input.bump();
                self.host.mark_layout(NodeId(*id));
            }
            Mutation::ListIndex { id, index } => {
                let node = NodeId(*id);
                if self.host.list_index[node.index()] != *index {
                    self.host.list_index[node.index()] = *index;
                    self.host.revs.layout_input.bump();
                    // Invalidation walks up to the list, which places it.
                    self.host.mark_layout(node);
                    self.host.dirty.semantic.push(*id);
                }
            }
            Mutation::ScrollAnchor { id, anchor } => {
                if *anchor == crate::mutation::Anchor::default() {
                    self.host.lists.policies.remove(id);
                } else {
                    self.host.lists.policies.insert(*id, *anchor);
                }
            }
        }
    }

    /// `node` leaves its place in the tree: if the pointer is over it or
    /// inside it, the hover falls back to its parent. Its ancestors stay
    /// hovered (no second enter on the next move); the subtree gets no
    /// leave, as a removed DOM element gets no `mouseleave`.
    fn unhover(&mut self, node: NodeId) {
        if let Some(h) = self.hover
            && self.ancestors(h).any(|n| n == node)
        {
            self.hover = Some(self.host.parent(node)).filter(|p| p.is_node());
        }
    }

    /// Drops per-node state held outside the host when a slot is created
    /// or freed: a recycled id must start clean.
    fn forget_node_state(&mut self, node: NodeId) {
        if let Some(t) = self.texts.get_mut(node.index()) {
            *t = None;
        }
        self.inputs.remove(node.0);
        self.vector_meshes.remove(&node.0);
        self.animations.forget(node);
        self.layouts.forget(node);
    }

    /// Writes a node's layout row; the one writer for layout inputs
    /// (mutations and the animation driver).
    pub(crate) fn set_layout(&mut self, node: NodeId, new: LayoutRow) {
        let old = &self.host.layout[node.index()];
        if *old == new {
            return;
        }
        let display_changed = old.display() != new.display();
        let overflow_changed = old.overflow() != new.overflow();
        self.host.layout[node.index()] = new;
        self.host.revs.layout_input.bump();
        self.host.mark_layout(node);
        if display_changed {
            self.host.revs.structure.bump();
            self.host.dirty.semantic.push(node.0);
        }
        if overflow_changed {
            self.host.revs.clip.bump();
            self.host.dirty.semantic.push(node.0);
        }
    }

    /// Writes a node's spatial row (the fields given).
    pub(crate) fn set_spatial(
        &mut self,
        node: NodeId,
        transform: Option<Affine>,
        opacity: Option<f32>,
    ) {
        let s = &mut self.host.spatial[node.index()];
        let before = (s.transformed(), s.layered());
        let mut changed = false;
        let mut moved = false;
        if let Some(t) = transform
            && s.transform != t
        {
            s.transform = t;
            changed = true;
            moved = true;
        }
        if let Some(o) = opacity
            && s.opacity != o
        {
            s.opacity = o;
            changed = true;
        }
        if !changed {
            return;
        }
        let after = (s.transformed(), s.layered());
        if moved {
            self.host.touch(node);
        }
        if before != after {
            // A transform record or an opacity layer appears or goes:
            // the draw topology changes.
            self.host.revs.structure.bump();
        }
        self.host.revs.transform.bump();
        self.host.dirty.spatial.push(node.0);
        self.host.dirty.semantic.push(node.0);
    }

    /// Writes a node's box paint row (the fields given).
    pub(crate) fn set_paint(
        &mut self,
        node: NodeId,
        fill: Option<u32>,
        radius: Option<f32>,
        border_color: Option<u32>,
        border_width: Option<f32>,
    ) {
        let id = node.0;
        let p = &mut self.host.paint[node.index()];
        let mut geometry = false;
        let mut color = false;
        if let Some(f) = fill
            && p.fill != f
        {
            // Transparent <-> visible changes whether a rect exists.
            geometry |= (p.fill & 0xFF == 0) != (f & 0xFF == 0);
            p.fill = f;
            color = true;
        }
        if let Some(r) = radius {
            let r = r.max(0.0);
            if p.radius != r {
                p.radius = r;
                geometry = true;
            }
        }
        if let Some(w) = border_width {
            let w = w.max(0.0);
            if p.border_width != w {
                p.border_width = w;
                geometry = true;
            }
        }
        if let Some(c) = border_color
            && p.border_color != c
        {
            geometry |= (p.border_color & 0xFF == 0) != (c & 0xFF == 0);
            p.border_color = c;
            color = true;
        }
        if geometry {
            self.host.dirty.content.push(id);
        } else if color {
            self.host.dirty.paint.push(id);
        }
        if geometry || color {
            self.host.revs.paint.bump();
        }
    }
}

/// An animatable layout field of `style` as a declared value.
fn layout_field(style: &taffy::Style, prop: Prop) -> Value {
    match prop {
        Prop::Width => Value::Size(style.size.width),
        Prop::Height => Value::Size(style.size.height),
        Prop::Padding => Value::Padding([
            style.padding.left,
            style.padding.right,
            style.padding.top,
            style.padding.bottom,
        ]),
        _ => Value::Gap([style.gap.width, style.gap.height]),
    }
}

/// Sets `prop`'s field of `style` to `v`.
fn set_layout_field(style: &mut taffy::Style, prop: Prop, v: Value) {
    match (prop, v) {
        (Prop::Width, Value::Size(d)) => style.size.width = d,
        (Prop::Height, Value::Size(d)) => style.size.height = d,
        (Prop::Padding, Value::Padding([l, r, t, b])) => {
            style.padding = taffy::Rect {
                left: l,
                right: r,
                top: t,
                bottom: b,
            }
        }
        (Prop::Gap, Value::Gap([w, h])) => {
            style.gap = taffy::Size {
                width: w,
                height: h,
            }
        }
        _ => {}
    }
}
