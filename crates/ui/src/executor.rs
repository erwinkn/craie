//! The mutation executor: one path from a `Transaction` (decoded from
//! CRW2 or built by the Rust direct API) to host state.
//!
//! A transaction is validated whole against host state plus the effects
//! of its earlier mutations; on failure nothing applies. Application is
//! then infallible and runs no callbacks: it writes the host stores,
//! advances revisions, and queues dirty work for the frame pipeline.
//! Events produced by commands (focus, blur) are queued, not delivered.

use std::collections::HashMap;

use crate::host::{Host, MAX_NODES, NodeId};
use crate::mutation::{Command, Mutation, NIL, NodeKind, TextSpan, Transaction};
use crate::ui::Ui;
use crate::wire::WireError;

/// How far past the slots in use a create may reach.
const ID_SLACK: u32 = 4096;

fn invalid(why: &'static str) -> WireError {
    WireError::Invalid(why)
}

fn finite(v: f32) -> bool {
    v.is_finite()
}

/// Checks `spans` as a paragraph style list over `text`.
fn valid_spans(text: &str, spans: &[TextSpan]) -> Result<(), WireError> {
    let Some(first) = spans.first() else {
        return Err(invalid("paragraph without spans"));
    };
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
        if !finite(s.font_size) || s.font_size <= 0.0 || !(1..=1000).contains(&s.weight) {
            return Err(invalid("paragraph span style out of range"));
        }
        prev = Some(s.start);
    }
    Ok(())
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
    /// Ids the batch created beyond the host's slots so far.
    grown: u32,
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
        grown: 0,
    };
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
                // recycles freed ones, so a create never jumps far past
                // the slots in use. Memory grows with nodes sent, not
                // with the largest id named.
                let slots = host.slot_count() as u32 + o.grown;
                if *id >= slots + ID_SLACK {
                    return Err(invalid("create leaves a gap in node ids"));
                }
                if *id >= slots {
                    o.grown += *id + 1 - slots;
                }
                if o.live(*id) {
                    return Err(invalid("create over a live node"));
                }
                o.kinds.insert(*id, Some(*kind));
                o.parents.insert(*id, (NodeId::DETACHED.0, step));
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
            } => {
                need_live(&o, *id, "spatial on an absent node")?;
                if transform.is_some_and(|t| !t.0.iter().all(|v| v.is_finite())) {
                    return Err(invalid("non-finite transform"));
                }
                if opacity.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
                    return Err(invalid("opacity outside [0, 1]"));
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
                valid_spans(text, spans)?;
            }
            Mutation::InputConfig { id, font_size, .. } => {
                if o.kind(*id) != Some(NodeKind::Input) {
                    return Err(invalid("input config on a non-input node"));
                }
                if !finite(*font_size) || *font_size <= 0.0 {
                    return Err(invalid("input font size out of range"));
                }
            }
            Mutation::Role { id, .. }
            | Mutation::Label { id, .. }
            | Mutation::Interaction { id, .. } => {
                need_live(&o, *id, "semantics on an absent node")?;
            }
            Mutation::Surface { id, .. } | Mutation::Payload { id, .. } => {
                if o.kind(*id) != Some(NodeKind::Surface) {
                    return Err(invalid("surface data on a non-surface node"));
                }
            }
            Mutation::Command { id, cmd } => {
                need_live(&o, *id, "command on an absent node")?;
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
        }
    }
    Ok(())
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
                self.host
                    .insert_before(NodeId(*parent), NodeId(*child), NodeId(*before));
            }
            Mutation::Detach { id } => self.host.detach(NodeId(*id)),
            Mutation::Remove { id } => {
                let node = NodeId(*id);
                self.host.remove(node);
                self.forget_node_state(node);
                for slot in [&mut self.focus, &mut self.hover, &mut self.pressed] {
                    if *slot == Some(node) {
                        *slot = None;
                    }
                }
                self.pending_scrolls.retain(|(n, _, _)| *n != node);
            }
            Mutation::Layout { id, style } => {
                let node = NodeId(*id);
                let new = if *style == NIL {
                    taffy::Style::default()
                } else {
                    txn.styles[*style as usize].clone()
                };
                let old = &self.host.layout[node.index()];
                if *old == new {
                    return;
                }
                let display_changed = old.display != new.display;
                let overflow_changed = old.overflow != new.overflow;
                self.host.layout[node.index()] = new;
                self.host.revs.layout_input.bump();
                self.host.mark_layout(node);
                if display_changed {
                    self.host.revs.structure.bump();
                    self.host.dirty.semantic.push(*id);
                }
                if overflow_changed {
                    self.host.revs.clip.bump();
                    self.host.dirty.semantic.push(*id);
                }
            }
            Mutation::Spatial {
                id,
                transform,
                opacity,
            } => {
                let node = NodeId(*id);
                let s = &mut self.host.spatial[node.index()];
                let before = (s.transformed(), s.layered());
                let mut changed = false;
                if let Some(t) = transform
                    && s.transform != *t
                {
                    s.transform = *t;
                    changed = true;
                }
                if let Some(o) = opacity
                    && s.opacity != *o
                {
                    s.opacity = *o;
                    changed = true;
                }
                if !changed {
                    return;
                }
                if before != (s.transformed(), s.layered()) {
                    // A transform record or an opacity layer appears or
                    // goes: the draw topology changes.
                    self.host.revs.structure.bump();
                }
                self.host.revs.transform.bump();
                self.host.dirty.spatial.push(*id);
                self.host.dirty.semantic.push(*id);
            }
            Mutation::Paint {
                id,
                fill,
                radius,
                border,
            } => {
                let p = &mut self.host.paint[*id as usize];
                let mut geometry = false;
                let mut color = false;
                if let Some(f) = fill
                    && p.fill != *f
                {
                    // Transparent <-> visible changes whether a rect exists.
                    geometry |= (p.fill & 0xFF == 0) != (f & 0xFF == 0);
                    p.fill = *f;
                    color = true;
                }
                if let Some(r) = radius {
                    let r = r.max(0.0);
                    if p.radius != r {
                        p.radius = r;
                        geometry = true;
                    }
                }
                if let Some((c, w)) = border {
                    let w = w.max(0.0);
                    if p.border_width != w {
                        p.border_width = w;
                        geometry = true;
                    }
                    if p.border_color != *c {
                        geometry |= (p.border_color & 0xFF == 0) != (c & 0xFF == 0);
                        p.border_color = *c;
                        color = true;
                    }
                }
                if geometry {
                    self.host.dirty.content.push(*id);
                } else if color {
                    self.host.dirty.paint.push(*id);
                }
                if geometry || color {
                    self.host.revs.paint.bump();
                }
            }
            Mutation::Paragraph { id, text, spans } => {
                let node = NodeId(*id);
                let spans = &txn.spans[spans.start as usize..spans.end as usize];
                let p = &mut self.host.paragraphs[node.index()];
                let text_changed = p.text != *text;
                let metrics_changed = text_changed
                    || p.spans.len() != spans.len()
                    || p.spans.iter().zip(spans).any(|(a, b)| !a.same_metrics(b));
                let colors_changed = p.spans.iter().zip(spans).any(|(a, b)| a.color != b.color);
                if text_changed {
                    p.text.clear();
                    p.text.push_str(text);
                }
                if metrics_changed || colors_changed {
                    p.spans.clear();
                    p.spans.extend_from_slice(spans);
                }
                if text_changed {
                    self.host.revs.text_content.bump();
                    self.host.dirty.semantic.push(*id);
                }
                if metrics_changed {
                    self.host.revs.text_metrics.bump();
                    self.host.mark_text(node);
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
            } => {
                let metrics =
                    self.inputs
                        .configure(*id, *font_size, *color, placeholder, *multiline);
                if metrics {
                    self.host.mark_layout(NodeId(*id));
                    self.host.revs.text_metrics.bump();
                } else {
                    // Color only: the chunk's paint changes, no layout.
                    self.host.revs.paint.bump();
                }
                self.host.dirty.content.push(*id);
                self.host.dirty.semantic.push(*id);
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
            } => {
                let i = &mut self.host.interaction[*id as usize];
                if i.listeners != *listeners || i.focusable != *focusable {
                    i.listeners = *listeners;
                    i.focusable = *focusable;
                    self.host.revs.semantic.bump();
                    self.host.dirty.semantic.push(*id);
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
            Mutation::Payload { id, bytes } => {
                let s = self.host.surfaces.entry(*id).or_default();
                if s.payload[..] != bytes[..] {
                    s.payload.clear();
                    s.payload.extend_from_slice(bytes);
                    self.host.revs.resource.bump();
                    self.host.dirty.content.push(*id);
                }
            }
            Mutation::Command { id, cmd } => self.command(NodeId(*id), cmd),
        }
    }

    /// Drops per-node state held outside the host when a slot is created
    /// or freed: a recycled id must start clean.
    fn forget_node_state(&mut self, node: NodeId) {
        if let Some(t) = self.texts.get_mut(node.index()) {
            *t = None;
        }
        self.inputs.remove(node.0);
    }
}
