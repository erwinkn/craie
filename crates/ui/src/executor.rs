//! The mutation executor: one path from a `Transaction` (decoded from
//! CRW2 or built by the Rust direct API) to host state.
//!
//! A transaction is validated whole against host state plus the effects
//! of its earlier mutations; on failure nothing applies. Application is
//! then infallible and runs no callbacks: it writes the host stores,
//! advances revisions, and queues dirty work for the frame pipeline.
//! Events produced by commands (focus, blur) are queued, not delivered.

use std::collections::HashMap;
use std::sync::Arc;

use craie_layout::LayoutRow;

use crate::animation::{Prop, Transition, Value};
use crate::claims::{Claim, claim_kind};
use crate::events::out_kind;
use crate::host::{Host, MAX_NODES, NodeId, SpatialPatch};
use crate::keyframes::{Animation, Trigger, frame_field};
use crate::list::{IdIndex, MAX_ITEMS};
use crate::mutation::{
    Command, Item, ItemDesc, ListOp, Mutation, NIL, NodeKind, Template, TextSpan, Transaction,
    interaction_flag,
};
use crate::states::{env_bit, state_bit, value_field};
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

/// Largest accepted jump offset: a list of `MAX_ITEMS` items of
/// `MAX_LIST_LENGTH` points each.
const MAX_OFFSET: f64 = (crate::list::MAX_ITEMS as f64) * MAX_LIST_LENGTH as f64;

/// Largest accepted retain window, in viewport heights each side.
const MAX_RETAIN: f32 = 1000.0;

/// Width bands by increasing minimum width, so a width finds its band;
/// a text template's character width a positive fraction of its size.
fn template_ok(t: &Template) -> bool {
    match t {
        Template::Fixed(size) => length_ok(*size),
        Template::Widths(bands) => {
            bands.iter().all(|b| length_ok(b.0) && length_ok(b.1))
                && bands.windows(2).all(|w| w[0].0 <= w[1].0)
        }
        Template::Text {
            base,
            inset,
            font_size,
            line_height,
            char_width,
        } => {
            length_ok(*base)
                && length_ok(*inset)
                && font_size_ok(*font_size)
                && length_ok(*line_height)
                && finite(*char_width)
                && *char_width > 0.0
                && *char_width <= 16.0
        }
    }
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

/// Variants per node and terms per variant: a table resolves in
/// O(variants × terms) on every change of a bit it reads.
pub const MAX_VARIANTS: usize = 256;
pub const MAX_TERMS: usize = 8;

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

/// A list the batch edits, as the batch leaves it.
enum Touch {
    /// The host's list plus edits recorded (item count after them).
    /// The first edit checks against the host's items and index; a
    /// second one on the same list materializes the sequence.
    Host { len: u32, edits: Vec<Edit> },
    /// The whole identity sequence and its index.
    Seq { ids: Vec<u32>, index: IdIndex },
}

/// An identity edit recorded against the host's sequence.
enum Edit {
    Splice(u32, u32, Vec<u32>),
    Move(u32, u32, u32),
}

impl Touch {
    fn empty() -> Touch {
        Touch::Seq {
            ids: Vec::new(),
            index: IdIndex::default(),
        }
    }

    fn of(base: Option<&crate::list::ListState>) -> Touch {
        match base {
            // A list created in this batch starts empty.
            None => Touch::empty(),
            Some(l) => Touch::Host {
                len: l.len(),
                edits: Vec::new(),
            },
        }
    }

    fn len(&self) -> u32 {
        match self {
            Touch::Host { len, .. } => *len,
            Touch::Seq { ids, .. } => ids.len() as u32,
        }
    }

    /// The host's sequence with the recorded edits, once.
    fn materialize(&mut self, base: Option<&crate::list::ListState>) {
        let Touch::Host { edits, .. } = self else {
            return;
        };
        let base = base.expect("a host touch has a host list");
        let mut ids: Vec<u32> = base.items.iter().map(|d| d.id).collect();
        for e in edits.drain(..) {
            match e {
                Edit::Splice(a, r, ins) => {
                    ids.splice(a as usize..(a + r) as usize, ins);
                }
                Edit::Move(f, c, t) => {
                    let moved: Vec<u32> = ids.drain(f as usize..(f + c) as usize).collect();
                    ids.splice(t as usize..t as usize, moved);
                }
            }
        }
        let index = IdIndex::build(ids.iter().copied());
        *self = Touch::Seq { ids, index };
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
        if let Touch::Host { edits, .. } = self
            && edits.is_empty()
        {
            // First edit: the host's items and index, unmodified.
            let base = base.expect("a host touch has a host list");
            let removed = IdIndex::build(base.items[range].iter().map(|d| d.id));
            if clash(&base.ids, &removed) {
                return Err(invalid("duplicate item identity in a list"));
            }
            edits.push(Edit::Splice(at, remove, inserted));
            *self = Touch::Host {
                len: next as u32,
                edits: std::mem::take(edits),
            };
            return Ok(());
        }
        self.materialize(base);
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

    /// Checks one move (both ranges inside the list) and records it.
    fn move_items(&mut self, from: u32, count: u32, to: u32) -> Result<(), WireError> {
        let cur = self.len();
        if from > cur || count > cur - from || to > cur - count {
            return Err(invalid("list move out of range"));
        }
        match self {
            Touch::Host { edits, .. } => edits.push(Edit::Move(from, count, to)),
            Touch::Seq { ids, .. } => {
                let moved: Vec<u32> = ids.drain(from as usize..(from + count) as usize).collect();
                ids.splice(to as usize..to as usize, moved);
            }
        }
        Ok(())
    }

    /// Checks one update: in range, and the same identities in the same
    /// places (an update never changes the sequence).
    fn update(
        &mut self,
        base: Option<&crate::list::ListState>,
        at: u32,
        updated: &[u32],
    ) -> Result<(), WireError> {
        let cur = self.len();
        if at > cur || updated.len() as u64 > (cur - at) as u64 {
            return Err(invalid("list update out of range"));
        }
        let range = at as usize..at as usize + updated.len();
        let same = match self {
            Touch::Host { edits, .. } if edits.is_empty() => {
                let base = base.expect("a host touch has a host list");
                base.items[range]
                    .iter()
                    .map(|d| d.id)
                    .eq(updated.iter().copied())
            }
            _ => {
                self.materialize(base);
                let Touch::Seq { ids, .. } = self else {
                    unreachable!()
                };
                ids[range] == *updated
            }
        };
        if !same {
            return Err(invalid("list update changes item identities"));
        }
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
    /// Exits the batch declares, clears or starts (one map: a batch
    /// with an exit allocates it once).
    exits: HashMap<u32, Exit>,
    /// The batch's placements by parent, with their steps: built at its
    /// first exit cut, then kept as it places, so each cut walks only
    /// its subtree. Stale entries stay (a node placed again, or freed);
    /// `parents` tells which link is current.
    placed: Option<HashMap<u32, Vec<(u32, usize)>>>,
    /// A cut's subtree, reused.
    scratch: Vec<u32>,
}

#[cfg(test)]
thread_local! {
    /// Nodes and links the cuts' subtree walks looked at, for the
    /// bulk-cut test.
    pub(crate) static WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy, PartialEq)]
enum Exit {
    Declared,
    Cleared,
    /// A detach of a node with an exit: its node never comes back.
    Started,
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

    fn has_exit(&self, id: u32) -> bool {
        match self.exits.get(&id) {
            Some(e) => *e == Exit::Declared,
            None => self.host.exits.contains_key(&id),
        }
    }

    /// An exit's root: its node never comes back.
    fn exiting(&self, id: u32) -> bool {
        match self.exits.get(&id) {
            Some(e) => *e == Exit::Started,
            None => self.host.exiting.contains(&NodeId(id)),
        }
    }

    /// Declares or clears `id`'s exit; one started stays started.
    fn declare_exit(&mut self, id: u32, declared: bool) {
        if !self.exiting(id) {
            let e = if declared {
                Exit::Declared
            } else {
                Exit::Cleared
            };
            self.exits.insert(id, e);
        }
    }

    /// Whether `id`'s ancestors reach the root level (native's test for
    /// an exit that can run).
    fn attached(&self, id: u32) -> Result<bool, WireError> {
        let mut cur = id;
        for _ in 0..self.host.slot_count() + self.kinds.len() + 1 {
            match self.parent(cur) {
                NIL => return Ok(true),
                p if p == NodeId::DETACHED.0 => return Ok(false),
                p => cur = p,
            }
        }
        Err(invalid("corrupt parent chain"))
    }

    /// Places `child` under `parent` at `step`.
    fn link(&mut self, child: u32, parent: u32, step: usize) {
        self.parents.insert(child, (parent, step));
        if let Some(placed) = &mut self.placed {
            placed.entry(parent).or_default().push((child, step));
        }
    }

    /// Frees `root` and the nodes under it, as the batch left them: the
    /// host's links it kept and its own.
    fn free_subtree(&mut self, root: u32, step: usize, lists: &mut HashMap<u32, Touch>) {
        if self.placed.is_none() {
            #[cfg(test)]
            WALKED.with(|w| w.set(w.get() + self.parents.len()));
            let mut placed: HashMap<u32, Vec<(u32, usize)>> = HashMap::new();
            for (&c, &(p, at)) in &self.parents {
                placed.entry(p).or_default().push((c, at));
            }
            self.placed = Some(placed);
        }
        let mut nodes = std::mem::take(&mut self.scratch);
        nodes.clear();
        nodes.push(root);
        let mut k = 0;
        while k < nodes.len() {
            let n = nodes[k];
            k += 1;
            let kids = self.host.children(NodeId(n));
            let placed = self.placed.as_ref().and_then(|p| p.get(&n));
            #[cfg(test)]
            WALKED.with(|w| w.set(w.get() + 1 + kids.len() + placed.map_or(0, Vec::len)));
            for &c in kids {
                if !self.parents.contains_key(&c.0) && self.parent(c.0) == n {
                    nodes.push(c.0);
                }
            }
            for &(c, at) in placed.into_iter().flatten() {
                if self.parents.get(&c) == Some(&(n, at)) && self.parent(c) == n {
                    nodes.push(c);
                }
            }
        }
        for &n in &nodes {
            self.free(n, step, lists);
        }
        self.scratch = nodes;
    }

    /// Marks `id` removed at `step`: its links are gone, and a list it
    /// was holds no items.
    fn free(&mut self, id: u32, step: usize, lists: &mut HashMap<u32, Touch>) {
        self.kinds.insert(id, None);
        self.parents.insert(id, (NodeId::DETACHED.0, step));
        self.removed.insert(id, step);
        self.exits.insert(id, Exit::Cleared);
        lists.insert(id, Touch::empty());
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

/// What validation built for apply: vector sources new to the host, by
/// source bytes (asset payloads, drawing keys; `None`: a drawing that
/// does not build, drawn as nothing), and each drawing's key by step,
/// built once and shared by the nodes drawing it.
#[derive(Default)]
pub struct Validated {
    vectors: HashMap<Arc<[u8]>, Option<craie_vector::asset::Asset>>,
    keys: HashMap<usize, Arc<[u8]>>,
}

impl Validated {
    /// A source's build, if validation made one: a drawing that failed
    /// stays for other nodes of the batch; an asset moves out.
    fn take(&mut self, source: &[u8]) -> Option<Option<craie_vector::asset::Asset>> {
        match self.vectors.get(source)? {
            None => Some(None),
            Some(_) => self.vectors.remove(source),
        }
    }
}

/// Validates the whole transaction. O(mutations); no host mutation.
pub fn validate(host: &Host, txn: &Transaction<'_>) -> Result<Validated, WireError> {
    let mut done = Validated::default();
    let mut o = Overlay {
        host,
        kinds: HashMap::new(),
        parents: HashMap::new(),
        removed: HashMap::new(),
        created: 0,
        exits: HashMap::new(),
        placed: None,
        scratch: Vec::new(),
    };
    // Lists the batch edits, as the batch leaves them (item counts and
    // identities), and their revisions: a patch with a stale base is
    // skipped, so later patches of the batch check against what applies.
    let mut lists: HashMap<u32, Touch> = HashMap::new();
    let mut revisions: HashMap<u32, u32> = HashMap::new();
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
                revisions.insert(*id, 0);
            }
            Mutation::Place {
                parent,
                child,
                before,
            } => {
                need_live(&o, *child, "place of an absent child")?;
                let exits = !o.exits.is_empty() || !host.exiting.is_empty();
                if exits && o.exiting(*child) {
                    return Err(invalid("place of an exiting node"));
                }
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
                    if exits && o.exiting(cur) {
                        return Err(invalid("place under an exiting node"));
                    }
                    cur = o.parent(cur);
                    hops += 1;
                    if hops > host.slot_count() + o.kinds.len() + 1 {
                        return Err(invalid("corrupt parent chain"));
                    }
                }
                o.link(*child, *parent, step);
            }
            Mutation::Detach { id } => {
                need_live(&o, *id, "detach of an absent node")?;
                if o.exiting(*id) {
                    return Err(invalid("detach of an exiting node"));
                }
                // An exit that runs keeps its node in its parent's list
                // (one that cannot is detached, and ends with the
                // transaction); either way the node never comes back.
                let mut stays = false;
                if o.has_exit(*id) {
                    o.exits.insert(*id, Exit::Started);
                    stays = o.kind(o.parent(*id)) != Some(NodeKind::List) && o.attached(*id)?;
                }
                if !stays {
                    o.parents.insert(*id, (NodeId::DETACHED.0, step));
                }
            }
            Mutation::Remove { id } => {
                need_live(&o, *id, "remove of an absent node")?;
                if o.exiting(*id) {
                    // An exit's root goes with its subtree.
                    o.free_subtree(*id, step, &mut lists);
                } else {
                    o.free(*id, step, &mut lists);
                }
            }
            // Idempotent: an exit that has ended left its id free.
            Mutation::EndExit { id } => {
                if o.live(*id) {
                    if !o.exiting(*id) {
                        return Err(invalid("end of an exit that never started"));
                    }
                    o.free_subtree(*id, step, &mut lists);
                }
            }
            Mutation::Layout { id, style } => {
                need_live(&o, *id, "layout on an absent node")?;
                if *style != NIL && *style as usize >= txn.styles.len() {
                    return Err(invalid("layout style out of range"));
                }
            }
            Mutation::Spatial { id, patch, .. } => {
                need_live(&o, *id, "spatial on an absent node")?;
                let mut parts = crate::host::Parts::IDENTITY;
                patch.apply(&mut parts);
                if !parts.is_finite() {
                    return Err(invalid("non-finite transform"));
                }
                if patch.opacity.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
                    return Err(invalid("opacity outside [0, 1]"));
                }
            }
            Mutation::Layer { id, owner } => {
                need_live(&o, *id, "layer on an absent node")?;
                if o.kind(*id) != Some(NodeKind::View) {
                    return Err(invalid("layer on a non-view node"));
                }
                if *owner != NIL {
                    need_live(&o, *owner, "layer owned by an absent node")?;
                    if owner == id {
                        return Err(invalid("layer owned by itself"));
                    }
                }
            }
            Mutation::Paint {
                id,
                radius,
                border,
                shadows,
                ..
            } => {
                if !o.kind(*id).is_some_and(NodeKind::has_box) {
                    return Err(invalid("paint on a node without a box"));
                }
                if radius.is_some_and(|r| !finite(r)) || border.is_some_and(|(_, w)| !finite(w)) {
                    return Err(invalid("non-finite paint"));
                }
                if shadows.is_some_and(|s| !s.valid()) {
                    return Err(invalid("shadow out of range"));
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
            Mutation::Lines { id, .. } => {
                if o.kind(*id) != Some(NodeKind::Text) {
                    return Err(invalid("a line limit on a non-text node"));
                }
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
            | Mutation::Interaction { id, .. }
            | Mutation::Trap { id, .. }
            | Mutation::Group { id, .. } => {
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
            Mutation::ImageConfig { id, .. } => {
                if o.kind(*id) != Some(NodeKind::Image) {
                    return Err(invalid("image config on a non-image node"));
                }
            }
            Mutation::Payload { id, bytes } => match o.kind(*id) {
                // An image's bytes are checked by the decoder, off the UI
                // thread: bad ones fail as an event, not a rejection.
                Some(NodeKind::Surface | NodeKind::Image) => {}
                // A vector's payload is its asset: it must decode. Its
                // magic tags it in the source table ("CRV1"; drawing
                // keys start "CRVS"), so it is checked before a lookup.
                Some(NodeKind::Vector) => {
                    let magic = craie_vector::asset::MAGIC.to_le_bytes();
                    if !bytes.starts_with(&magic) {
                        return Err(invalid("vector asset does not decode"));
                    }
                    if !host.has_vector_source(bytes) && !done.vectors.contains_key(&bytes[..]) {
                        let asset = craie_vector::asset::decode(bytes)
                            .map_err(|_| invalid("vector asset does not decode"))?;
                        done.vectors.insert(bytes[..].into(), Some(asset));
                    }
                }
                _ => return Err(invalid("payload on a node without one")),
            },
            Mutation::Drawing { id, drawing } => {
                if o.kind(*id) != Some(NodeKind::Vector) {
                    return Err(invalid("drawing on a non-vector node"));
                }
                // Past the structural limits the drawing is refused; the
                // key is bounded once they hold. A value that does not
                // parse draws nothing instead (app data: a NaN in a chart
                // must not close the window).
                drawing
                    .check()
                    .map_err(|_| invalid("vector drawing too large"))?;
                let key = drawing.key();
                let key = host
                    .vector_source_key(&key)
                    .or_else(|| done.vectors.get_key_value(&key[..]).map(|(k, _)| k.clone()))
                    .unwrap_or_else(|| key.into());
                let current = host.vectors.get(id).is_some_and(|v| v.bytes == key);
                if !current && !host.has_vector_source(&key) && !done.vectors.contains_key(&key) {
                    done.vectors.insert(key.clone(), drawing.build().ok());
                }
                done.keys.insert(step, key);
            }
            Mutation::Command { id, cmd } => {
                // The clipboard is the window's: NIL may write it.
                // Presentation is the window's alone.
                match cmd {
                    Command::Present { .. } if *id != NIL => {
                        return Err(invalid("present on a node"));
                    }
                    Command::Present { .. } => {}
                    Command::WriteClipboard(_) if *id == NIL => {}
                    _ => need_live(&o, *id, "command on an absent node")?,
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
                let touch = lists.entry(*id).or_insert_with(|| Touch::of(base));
                touch.splice(base, *at, *remove, inserted, &fresh)?;
                let rev = revisions
                    .entry(*id)
                    .or_insert_with(|| base.map_or(0, |l| l.revision));
                *rev = rev.wrapping_add(1);
            }
            Mutation::ListConfig2 {
                id,
                overscan,
                lookahead,
                retain,
                fallback,
                templates,
                ..
            } => {
                if o.kind(*id) != Some(NodeKind::List) {
                    return Err(invalid("list config on a non-list node"));
                }
                // A negative lookahead is one viewport height.
                if !length_ok(*overscan)
                    || !length_ok(*fallback)
                    || !(*lookahead < 0.0 || length_ok(*lookahead))
                    || !(finite(*retain) && (0.0..=MAX_RETAIN).contains(retain))
                {
                    return Err(invalid("list config out of range"));
                }
                if !templates.iter().all(template_ok) {
                    return Err(invalid("list template out of range"));
                }
            }
            Mutation::ListPatch {
                id,
                base,
                next,
                ops,
            } => {
                if o.kind(*id) != Some(NodeKind::List) {
                    return Err(invalid("list patch on a non-list node"));
                }
                let Some(ops) = ListOp::parse(ops) else {
                    return Err(invalid("malformed list patch"));
                };
                for op in &ops {
                    if let ListOp::Splice { items, .. } | ListOp::Update { items, .. } = op {
                        Item::check(items, MAX_LIST_LENGTH).map_err(invalid)?;
                    }
                }
                let host_list = host.lists.get(*id);
                let rev = revisions
                    .entry(*id)
                    .or_insert_with(|| host_list.map_or(0, |l| l.revision));
                // A stale base: the patch is skipped (with a resync
                // event), not an error; JS reconciles from the revision
                // native is at.
                if *base != *rev {
                    continue;
                }
                *rev = *next;
                let touch = lists.entry(*id).or_insert_with(|| Touch::of(host_list));
                for op in ops {
                    match op {
                        ListOp::Splice { at, remove, items } => {
                            let inserted: Vec<u32> = Item::iter(&items).map(|d| d.id).collect();
                            let fresh = IdIndex::build(inserted.iter().copied());
                            if fresh.has_duplicates() {
                                return Err(invalid("duplicate item identity in a splice"));
                            }
                            touch.splice(host_list, at, remove, inserted, &fresh)?;
                        }
                        ListOp::Move { from, count, to } => touch.move_items(from, count, to)?,
                        ListOp::Update { at, items } => {
                            let updated: Vec<u32> = Item::iter(&items).map(|d| d.id).collect();
                            touch.update(host_list, at, &updated)?;
                        }
                    }
                }
            }
            Mutation::ListRow { id, list, item, .. } => {
                need_live(&o, *id, "list row on an absent node")?;
                if *item != NIL && o.kind(*list) != Some(NodeKind::List) {
                    return Err(invalid("list row of a non-list node"));
                }
            }
            Mutation::ListIndex { id, .. } => {
                need_live(&o, *id, "list index on an absent node")?;
            }
            Mutation::ScrollAnchor { id, .. } => {
                need_live(&o, *id, "scroll anchor on an absent node")?;
            }
            Mutation::ListPolicy { id, policy } => {
                need_live(&o, *id, "list policy on an absent node")?;
                let p = policy;
                if !(length_ok(p.end_threshold)
                    && length_ok(p.start_inset)
                    && length_ok(p.padding_end))
                {
                    return Err(invalid("list policy out of range"));
                }
            }
            Mutation::ListCommand { id, jump, .. } => {
                if o.kind(*id) != Some(NodeKind::List) {
                    return Err(invalid("list command on a non-list node"));
                }
                if let crate::mutation::Jump::Offset(y) = jump
                    && !(y.is_finite() && y.abs() <= MAX_OFFSET)
                {
                    return Err(invalid("list command offset out of range"));
                }
            }
            Mutation::Transition { id, transitions } => {
                need_live(&o, *id, "transition on an absent node")?;
                valid_transitions(transitions)?;
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
            Mutation::Animation {
                id,
                trigger,
                animations,
                ..
            } => {
                need_live(&o, *id, "animation on an absent node")?;
                let boxed = o.kind(*id).is_some_and(NodeKind::has_box);
                let exit = *trigger == Trigger::Exit;
                valid_animations(animations, boxed, exit)?;
                if exit {
                    o.declare_exit(*id, !animations.is_empty());
                }
            }
            Mutation::States { id, bits } => {
                need_live(&o, *id, "states on an absent node")?;
                if bits & state_bit::INPUT != 0 {
                    return Err(invalid("states sets an input bit"));
                }
            }
            Mutation::Variants { id, variants } => {
                need_live(&o, *id, "variants on an absent node")?;
                let boxed = o.kind(*id).is_some_and(NodeKind::has_box);
                if variants.len() > MAX_VARIANTS {
                    return Err(invalid("too many variants on one node"));
                }
                for v in variants.iter() {
                    if v.terms.len() > MAX_TERMS {
                        return Err(invalid("too many terms in one variant"));
                    }
                    if !v.values.valid() || v.env & !env_bit::ALL != 0 {
                        return Err(invalid("variant value out of range"));
                    }
                    if !boxed && v.values.mask & value_field::BOX != 0 {
                        return Err(invalid("box variant on a node without a box"));
                    }
                    for t in &v.terms {
                        need_live(&o, t.scope, "variant on an absent scope")?;
                    }
                    valid_transitions(v.transitions.as_deref().unwrap_or_default())?;
                    valid_animations(&v.animations, boxed, false)?;
                }
                let blocks = variants.iter().filter(|v| !v.animations.is_empty());
                for (k, v) in blocks.clone().enumerate() {
                    if blocks.clone().take(k).any(|u| u.block == v.block) {
                        return Err(invalid("variant block declared twice"));
                    }
                }
            }
            Mutation::Environment {
                narrow_max,
                compact_max,
            } => {
                if !(finite(*narrow_max) && finite(*compact_max)) {
                    return Err(invalid("non-finite breakpoint"));
                }
            }
            Mutation::Color { id, .. } => {
                need_live(&o, *id, "color on an absent node")?;
            }
        }
    }
    Ok(done)
}

/// Transitions: at most one per property, timings in range.
fn valid_transitions(transitions: &[Transition]) -> Result<(), WireError> {
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
    Ok(())
}

/// Keyframe animations: a bounded list, in range, indices ascending, box
/// colors only on a node with a box. An exit's (`exit`) end, and only an
/// exit's may set the size.
fn valid_animations(animations: &[Animation], boxed: bool, exit: bool) -> Result<(), WireError> {
    if animations.len() > crate::keyframes::MAX_ANIMATIONS {
        return Err(invalid("too many animations in one list"));
    }
    for (k, a) in animations.iter().enumerate() {
        a.check().map_err(invalid)?;
        if k > 0 && animations[k - 1].index >= a.index {
            return Err(invalid("animation indices out of order"));
        }
        if !boxed && a.keyframes.mask() & value_field::BOX != 0 {
            return Err(invalid("paint animation on a node without a box"));
        }
        if exit && a.infinite() {
            return Err(invalid("an exit that never ends"));
        }
        if !exit && a.keyframes.mask() & frame_field::SIZE != 0 {
            return Err(invalid("size keyframes outside an exit"));
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
        (Prop::Translate, Value::Translate(t)) => t.iter().all(|v| v.is_finite()),
        (Prop::Rotate, Value::Rotate(r)) => r.is_finite(),
        (Prop::Scale, Value::Scale(s)) => s.iter().all(|v| v.is_finite()),
        (Prop::Opacity, Value::Opacity(o)) => (0.0..=1.0).contains(o),
        (Prop::Fill | Prop::BorderColor | Prop::Color, Value::Color(_)) => true,
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
        let mut done = validate(&self.host, txn)?;
        if !self.states.env_reported {
            self.report_env();
        }
        self.motion.begin();
        self.begin_traps();
        for (step, m) in txn.mutations.iter().enumerate() {
            self.apply_mutation(txn, step, m, &mut done);
        }
        self.settle_exits();
        self.seq = txn.seq;
        self.restyle();
        self.settle_traps();
        self.cancel_blocked_presses();
        // The focus and the presses settling moved (a key compare when
        // none did).
        self.restyle();
        // Last: what hides an exit may be a variant on the state the
        // steps above moved (`_focusWithin` losing the focus).
        self.end_hidden_exits();
        Ok(())
    }

    fn apply_mutation(
        &mut self,
        txn: &Transaction<'_>,
        step: usize,
        m: &Mutation<'_>,
        done: &mut Validated,
    ) {
        match m {
            Mutation::Create { id, kind } => {
                let node = NodeId(*id);
                self.host.create(node, *kind);
                self.forget_node_state(node);
                self.motion.born(node);
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
                // Its text, inputs and drawings may inherit another color
                // now.
                if !self.host.colors.is_empty() && !self.host.colors.contains_key(&child.0) {
                    self.repaint_inheritors(child, None);
                }
            }
            Mutation::Detach { id } => self.detach_node(NodeId(*id)),
            Mutation::Remove { id } => {
                if !self.cut_exit(NodeId(*id)) {
                    self.remove_node(NodeId(*id));
                }
            }
            Mutation::EndExit { id } => {
                self.cut_exit(NodeId(*id));
            }
            // A node with a variant table: its own ops set the base, and
            // the restyle at the end of the transaction declares.
            Mutation::Layout { id, style } => {
                let new = if *style == NIL {
                    crate::host::default_style().to_taffy()
                } else {
                    txn.styles[*style as usize].clone()
                };
                match self.base_mut(*id) {
                    Some(b) => b.layout = LayoutRow::from(&new),
                    None => self.declare_layout(NodeId(*id), new),
                }
            }
            Mutation::Spatial { id, patch, z } => {
                // Not animatable: a z change reorders at once.
                if let Some(z) = z {
                    self.host.set_z(NodeId(*id), *z);
                }
                if !patch.is_empty() {
                    match self.base_mut(*id) {
                        Some(b) => {
                            patch.apply(&mut b.parts);
                            b.opacity = patch.opacity.unwrap_or(b.opacity);
                        }
                        None => self.declare_spatial(NodeId(*id), *patch),
                    }
                }
            }
            Mutation::Layer { id, owner } => {
                self.host.set_layer(NodeId(*id), *owner);
            }
            Mutation::Paint {
                id,
                fill,
                radius,
                border,
                shadows,
            } => match self.base_mut(*id) {
                Some(b) => {
                    b.fill = fill.unwrap_or(b.fill);
                    b.radius = radius.map_or(b.radius, |r| r.max(0.0));
                    b.border = border.map_or(b.border, |(c, w)| (c, w.max(0.0)));
                    b.shadows = shadows.unwrap_or(b.shadows);
                }
                None => {
                    self.declare_paint(NodeId(*id), *fill, *radius, *border);
                    if let Some(s) = shadows {
                        self.set_shadows(NodeId(*id), *s);
                    }
                }
            },
            Mutation::Color { id, color } => match self.base_mut(*id) {
                Some(b) => b.color = *color,
                None => self.declare_color(NodeId(*id), *color),
            },
            Mutation::States { id, bits } => self.set_app_bits(*id, *bits),
            Mutation::Variants { id, variants } => self.set_variants(*id, variants),
            Mutation::Environment {
                narrow_max,
                compact_max,
            } => self.set_breakpoints(*narrow_max, *compact_max),
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
                // On a tabled node it sets the base and runs; a variant
                // that overrides the property wins at the restyle.
                if let Some(t) = self.states.tables.get_mut(id) {
                    crate::states::set_base(&mut t.base, *prop, *value);
                    crate::states::set_base(&mut t.resolved, *prop, *value);
                    self.states.queue.push(*id);
                }
                if self.start_animation(node, *prop, *value, *timing, true) {
                    // Applied at once: it ends now.
                    self.write_value(node, *prop, *value);
                    self.report_end(node, *prop, crate::animation::end_reason::FINISHED);
                }
            }
            Mutation::Animation {
                id,
                trigger,
                notify,
                animations,
            } => self.declare_animations(NodeId(*id), *trigger, *notify, animations),
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
                let colors_changed = p
                    .spans
                    .iter()
                    .zip(spans)
                    .any(|(a, b)| a.color != b.color || a.inherit_color != b.inherit_color);
                let decorations_changed = p
                    .spans
                    .iter()
                    .zip(spans)
                    .any(|(a, b)| a.decoration != b.decoration);
                // Press marks draw nothing: kept, no repaint.
                let presses_changed = p
                    .spans
                    .iter()
                    .zip(spans)
                    .any(|(a, b)| a.pressable != b.pressable || a.press_joins != b.press_joins);
                if text_changed {
                    p.text.clear();
                    p.text.push_str(text);
                    self.host.copied_bytes += text.len() as u64;
                }
                if metrics_changed || colors_changed || decorations_changed || presses_changed {
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
            Mutation::Lines { id, max } => {
                let node = NodeId(*id);
                let p = &mut self.host.paragraphs[node.index()];
                if p.max_lines != *max {
                    // The ellipsis is shaped with the paragraph.
                    p.max_lines = *max;
                    self.host.revs.text_metrics.bump();
                    self.host.mark_text(node);
                }
            }
            Mutation::InputConfig {
                id,
                font_size,
                placeholder,
                multiline,
                submit,
            } => {
                let metrics = self
                    .inputs
                    .configure(*id, *font_size, placeholder, *multiline);
                if let Some(state) = self.inputs.get_mut(*id) {
                    state.submit = *submit;
                }
                if metrics {
                    self.host.mark_layout(NodeId(*id));
                    self.host.revs.text_metrics.bump();
                    self.host.dirty.content.push(*id);
                    self.host.dirty.semantic.push(*id);
                }
            }
            Mutation::Role { id, role, reported } => {
                let i = &mut self.host.interaction[*id as usize];
                if (i.role, i.reported) != (*role, *reported) {
                    i.role = *role;
                    i.reported = *reported;
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
                flags,
            } => {
                let hovers = self.host.hover_listeners;
                let i = self.host.interaction[*id as usize];
                let focusable = flags & interaction_flag::FOCUSABLE != 0;
                let selectable = flags & interaction_flag::SELECTABLE != 0;
                let auto_focus = flags & interaction_flag::AUTO_FOCUS != 0;
                let press = flags >> interaction_flag::PRESS_SHIFT;
                // An exiting node stays inert whatever it declares.
                let inert = flags & interaction_flag::INERT != 0 || self.exiting(NodeId(*id));
                let inert = self.set_inert(NodeId(*id), inert);
                if auto_focus && !i.auto_focus && !self.traps.stack.is_empty() {
                    // It may take the focus of the trap it mounts into.
                    self.traps.auto_focused.push(NodeId(*id));
                }
                if i.listeners != *listeners
                    || i.focusable != focusable
                    || i.selectable != selectable
                    || i.press != press
                    || i.auto_focus != auto_focus
                    || inert
                {
                    self.observe.listeners(*id, i.listeners, *listeners);
                    let i = &mut self.host.interaction[*id as usize];
                    i.focusable = focusable;
                    i.selectable = selectable;
                    i.press = press;
                    i.auto_focus = auto_focus;
                    self.host.set_listeners(*id as usize, *listeners);
                    // A new hover listener: the hover at rest may be
                    // stale (it was not tracked without one).
                    self.hover_stale |= self.host.hover_listeners > hovers;
                    self.host.revs.semantic.bump();
                    self.host.dirty.semantic.push(*id);
                }
            }
            Mutation::Trap { id, flags } => {
                // Takes effect at the end of the transaction
                // (`settle_traps`).
                if self.traps.declared.insert(*id, *flags) != Some(*flags) {
                    self.traps.dirty = true;
                    self.host.revs.semantic.bump();
                    self.host.dirty.semantic.push(*id);
                }
            }
            Mutation::Group { id, flags } => self.set_group(NodeId(*id), *flags),
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
                // Validated: it decodes (a node dropped earlier in the
                // batch may have released its interned copy).
                self.set_vector(
                    *id,
                    bytes,
                    || bytes[..].into(),
                    || {
                        done.take(bytes)
                            .unwrap_or_else(|| craie_vector::asset::decode(bytes).ok())
                    },
                );
            }
            Mutation::Payload { id, bytes }
                if self.host.kind(NodeId(*id)) == Some(NodeKind::Image) =>
            {
                self.set_image(*id, bytes);
            }
            Mutation::ImageConfig { id, fit } => self.set_image_fit(*id, *fit),
            Mutation::Drawing { id, drawing } => {
                // Validated: the key is built; the asset is built, or
                // `None` if it does not parse.
                let key = done.keys.remove(&step).expect("validated drawing");
                let share = key.clone();
                self.set_vector(
                    *id,
                    &key,
                    || share,
                    || done.take(&key).unwrap_or_else(|| drawing.build().ok()),
                );
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
                self.host.lists.rows.remove(id);
                self.set_list_index(NodeId(*id), *index);
            }
            Mutation::ListConfig2 {
                id,
                overscan,
                lookahead,
                retain,
                fallback,
                epoch,
                templates,
            } => {
                self.host.lists.configure2(
                    *id, *overscan, *lookahead, *retain, *fallback, *epoch, templates,
                );
                self.host.revs.layout_input.bump();
                self.host.mark_layout(NodeId(*id));
            }
            Mutation::ListPatch {
                id,
                base,
                next,
                ops,
            } => {
                let cur = self.host.lists.get(*id).map_or(0, |l| l.revision);
                if *base != cur {
                    let mut e = self.event(out_kind::LIST_RESYNC, NodeId(*id));
                    e.revision = cur;
                    e.key = *base;
                    self.pending_events.push(e);
                    return;
                }
                self.host.copied_bytes += ops.len() as u64;
                let ops = ListOp::parse(ops).expect("validated list patch");
                // The anchor is chosen against the geometry before the
                // batch, once the batch applied.
                let before = self.batch_before(*id, &ops);
                self.host.lists.patch(&mut self.text, *id, *next, &ops);
                if let Some(b) = before {
                    self.batch_anchor(*id, b);
                }
                self.host.revs.layout_input.bump();
                self.host.mark_layout(NodeId(*id));
                // Tagged rows follow their items, placed yet or not.
                let rows: Vec<u32> = (self.host.lists.rows.iter())
                    .filter(|(_, t)| t.list == *id)
                    .map(|(&r, _)| r)
                    .collect();
                for row in rows {
                    let index = self.host.lists.tagged_index(row).unwrap_or(NIL);
                    self.set_list_index(NodeId(row), index);
                }
            }
            Mutation::ListRow {
                id,
                list,
                item,
                version,
            } => {
                let index = if *item == NIL {
                    self.host.lists.rows.remove(id);
                    NIL
                } else {
                    let tag = crate::list::RowTag {
                        list: *list,
                        item: *item,
                        version: *version,
                    };
                    if self.host.lists.rows.insert(*id, tag) != Some(tag) {
                        // Measuring may start or stop with the version.
                        self.host.mark_layout(NodeId(*id));
                    }
                    self.host.lists.tagged_index(*id).unwrap_or(NIL)
                };
                self.set_list_index(NodeId(*id), index);
            }
            Mutation::ListPolicy { id, policy } => {
                let before = self.host.lists.policy(*id);
                self.host.lists.set_policy(*id, *policy);
                // End padding is scroll range; a covered band moves what
                // the viewport is.
                if before.padding_end != policy.padding_end
                    || before.start_inset != policy.start_inset
                {
                    self.host.mark_layout(NodeId(*id));
                }
                self.force_paint = true;
            }
            Mutation::ListCommand {
                id, revision, jump, ..
            } => {
                // An index made for another item order is skipped.
                let current = self.host.lists.get(*id).map_or(0, |l| l.revision);
                if matches!(jump, crate::mutation::Jump::Index(..)) && *revision != current {
                    return;
                }
                self.host.lists.jumps.push((*id, *jump));
                self.force_paint = true;
            }
            Mutation::ScrollAnchor { id, anchor } => {
                let policy = crate::mutation::ListPolicy {
                    mode: *anchor,
                    ..self.host.lists.policy(*id)
                };
                self.host.lists.set_policy(*id, policy);
            }
        }
    }

    /// `node` leaves its place in the tree: if the pointer is over it or
    /// inside it, the hover falls back to its parent. Its ancestors stay
    /// hovered (no second enter on the next move); the subtree gets no
    /// leave, as a removed DOM element gets no `mouseleave`.
    pub(crate) fn unhover(&mut self, node: NodeId) {
        if let Some(h) = self.hover
            && self.ancestors(h).any(|n| n == node)
        {
            self.hover = Some(self.host.parent(node)).filter(|p| p.is_node());
        }
    }

    /// `node` leaves the tree: a press inside it ends (and its pointer
    /// capture), with no click on release. Focus stays: a node that
    /// moves keeps it, and its scopes' bits follow it when it is placed
    /// again.
    pub(crate) fn unpress(&mut self, node: NodeId) {
        if let Some(p) = self.pressed
            && self.ancestors(p).any(|n| n == node)
        {
            self.pressed = None;
            self.pressed_primary = false;
            self.selecting = false;
        }
        // A pressable's press cancels, with an event while it is still
        // there (a moved node's listener hears it).
        if let Some(p) = self.press
            && self.ancestors(p.node).any(|n| n == node)
        {
            self.cancel_press();
        }
        if self
            .key_press
            .is_some_and(|k| self.ancestors(k).any(|n| n == node))
        {
            self.key_press = None;
        }
    }

    /// Sets a vector node's source (asset bytes or a drawing's key),
    /// shared with other nodes of the same source; `share` makes the
    /// source's shared copy and `build` decodes it when no node has it
    /// (`None`: it draws nothing).
    fn set_vector(
        &mut self,
        id: u32,
        source: &[u8],
        share: impl FnOnce() -> Arc<[u8]>,
        build: impl FnOnce() -> Option<craie_vector::asset::Asset>,
    ) {
        if self
            .host
            .vectors
            .get(&id)
            .is_some_and(|v| v.bytes[..] == source[..])
        {
            return;
        }
        let (bytes, asset) = self.host.vector_source(source, share, build);
        let inherits = asset.as_ref().is_some_and(|a| a.inherits_color());
        let v = self.host.vectors.entry(id).or_default();
        v.bytes = bytes;
        v.asset = asset;
        // Starting or stopping `currentColor` changes the inheritors.
        if std::mem::replace(&mut v.inherits, inherits) != inherits {
            self.color_bounds += 1;
        }
        self.host.copied_bytes += source.len() as u64;
        self.host.revs.resource.bump();
        self.host.dirty.content.push(id);
        // The view box is its intrinsic size.
        self.host.revs.layout_input.bump();
        self.host.mark_layout(NodeId(id));
        self.vector_meshes.forget(id);
    }

    /// Drops per-node state held outside the host when a slot is created
    /// or freed: a recycled id must start clean.
    /// Removes `node` for good; its children stay, detached.
    pub(crate) fn remove_node(&mut self, node: NodeId) {
        // Its tweens end before the slot's generation moves.
        self.end_animations_of(node, crate::animation::end_reason::REMOVED);
        self.end_keyframes_of(node);
        self.unhover(node);
        self.unpress(node);
        self.focus_leaves(node);
        self.set_inert(node, false);
        if self.traps.declared.remove(&node.0).is_some() {
            self.traps.dirty = true;
        }
        self.groups.remove(&node.0);
        self.host.remove(node);
        self.forget_node_state(node);
        if self.pressed == Some(node) {
            self.pressed = None;
        }
        self.pending_scrolls.retain(|(n, _, _)| *n != node);
    }

    /// Row `node` stands for item `index` of its list (NIL: none).
    fn set_list_index(&mut self, node: NodeId, index: u32) {
        if self.host.list_index[node.index()] != index {
            self.host.list_index[node.index()] = index;
            self.host.revs.layout_input.bump();
            // Invalidation walks up to the list, which places it.
            self.host.mark_layout(node);
            self.host.dirty.semantic.push(node.0);
        }
    }

    fn forget_node_state(&mut self, node: NodeId) {
        if let Some(t) = self.texts.get_mut(node.index()) {
            *t = None;
        }
        self.inputs.remove(node.0);
        self.vector_meshes.forget(node.0);
        self.images.forget(node.0, &mut self.scene.atlas);
        self.animations.forget(node);
        self.motion.forget(node);
        self.layouts.forget(node);
        self.forget_states(node);
        self.inheritors.remove(&node.0);
        self.observe.forget(node.0);
    }

    /// Declares a node's layout (a full style): transitions intercept
    /// the animated fields, then the row writer.
    pub(crate) fn declare_layout(&mut self, node: NodeId, mut new: taffy::Style) {
        // Animated fields: a transition tweens to the new value, so the
        // row keeps the value on screen.
        for prop in [Prop::Width, Prop::Height, Prop::Padding, Prop::Gap] {
            let next = layout_field(&new, prop);
            // Under an exit's size: the value goes under, the row keeps
            // the sample.
            if self.absorb(node, prop, Some(next)) {
                let row = self.host.layout[node.index()].size();
                let d = if prop == Prop::Width {
                    row.width
                } else {
                    row.height
                };
                set_layout_field(&mut new, prop, Value::Size(d));
                continue;
            }
            if !self.intercept(node, prop, next) {
                let current = self.row_value(node, prop);
                set_layout_field(&mut new, prop, current);
            }
        }
        self.set_layout(node, LayoutRow::from(&new));
    }

    /// Declares a node's spatial fields (those given). Transitions
    /// intercept them; a keyframe animation takes them under it.
    pub(crate) fn declare_spatial(&mut self, node: NodeId, p: SpatialPatch) {
        let now = |ui: &mut Ui, prop: Prop, v: Value| {
            ui.intercept(node, prop, v) && !ui.absorb(node, prop, Some(v))
        };
        let patch = SpatialPatch {
            translate: p
                .translate
                .filter(|t| now(self, Prop::Translate, Value::Translate(*t))),
            rotate: p
                .rotate
                .filter(|r| now(self, Prop::Rotate, Value::Rotate(*r))),
            scale: p.scale.filter(|s| now(self, Prop::Scale, Value::Scale(*s))),
            matrix: p
                .matrix
                .filter(|t| now(self, Prop::Transform, Value::Transform(*t))),
            opacity: p
                .opacity
                .filter(|o| now(self, Prop::Opacity, Value::Opacity(*o))),
        };
        self.set_spatial(node, patch);
    }

    /// Declares a node's box paint (the fields given).
    pub(crate) fn declare_paint(
        &mut self,
        node: NodeId,
        fill: Option<u32>,
        radius: Option<f32>,
        border: Option<(u32, f32)>,
    ) {
        let now = |ui: &mut Ui, prop: Prop, c: u32| {
            ui.intercept(node, prop, Value::Color(c))
                && !ui.absorb(node, prop, Some(Value::Color(c)))
        };
        let fill = fill.filter(|c| now(self, Prop::Fill, *c));
        let border_color = border
            .map(|(c, _)| c)
            .filter(|c| now(self, Prop::BorderColor, *c));
        self.set_paint(node, fill, radius, border_color, border.map(|(_, w)| w));
    }

    /// Declares a node's inherited color. Between two colors a
    /// transition tweens; setting or clearing one jumps.
    pub(crate) fn declare_color(&mut self, node: NodeId, color: Option<u32>) {
        match color {
            Some(c) => {
                if self.intercept(node, Prop::Color, Value::Color(c))
                    && !self.absorb(node, Prop::Color, Some(Value::Color(c)))
                {
                    self.set_color(node, Some(c));
                }
            }
            None => {
                if let Some(i) = self.animations.find(node, Prop::Color) {
                    self.end_animation(i, crate::animation::end_reason::CANCELLED);
                }
                if !self.absorb(node, Prop::Color, None) {
                    self.set_color(node, None);
                }
            }
        }
    }

    /// Writes a node's inherited color; the spans, inputs and
    /// `currentColor` drawings inheriting it repaint.
    pub(crate) fn set_color(&mut self, node: NodeId, color: Option<u32>) {
        let old = match color {
            Some(c) => self.host.colors.insert(node.0, c),
            None => self.host.colors.remove(&node.0),
        };
        if old == color {
            return;
        }
        self.host.revs.paint.bump();
        if old.is_some() != color.is_some() {
            self.color_bounds += 1;
        }
        // The inheritors hold while the tree, the text, the color roots
        // and the drawings using `currentColor` do: a tween reuses them
        // every frame.
        let key = (
            self.host.revs.structure,
            self.host.revs.text_content,
            self.color_bounds,
        );
        let mut entry = self.inheritors.remove(&node.0).unwrap_or_default();
        if entry.0 == key {
            for &n in &entry.1 {
                self.host.dirty.paint.push(n);
            }
        } else {
            entry.1.clear();
            self.repaint_inheritors(node, Some(&mut entry.1));
            entry.0 = key;
        }
        self.inheritors.insert(node.0, entry);
    }

    /// Queues a paint patch for the nodes whose nearest inherited color
    /// comes from `node` (or would): its subtree, stopping at descendants
    /// with a color of their own. Lists them in `found`. The inheritors
    /// are text with an inheriting span, inputs, and vectors whose
    /// drawing uses `currentColor`.
    fn repaint_inheritors(&mut self, node: NodeId, mut found: Option<&mut Vec<u32>>) {
        let mut stack = std::mem::take(&mut self.node_scratch);
        stack.clear();
        stack.push(node);
        while let Some(n) = stack.pop() {
            let inherits = match self.host.kind(n) {
                Some(NodeKind::Text) => self.host.paragraphs[n.index()]
                    .spans
                    .iter()
                    .any(|s| s.inherit_color),
                Some(NodeKind::Input) => true,
                Some(NodeKind::Vector) => self.host.vector_inherits(n),
                _ => false,
            };
            if inherits {
                self.host.dirty.paint.push(n.0);
                if let Some(f) = found.as_deref_mut() {
                    f.push(n.0);
                }
            }
            for &c in self.host.children(n) {
                if !self.host.colors.contains_key(&c.0) {
                    stack.push(c);
                }
            }
        }
        self.node_scratch = stack;
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

    /// Writes a node's spatial row (the fields given), composing the
    /// transform parts when one changed: the one place they compose. A
    /// composition that overflows keeps the row as it was (the row only:
    /// the base values keep the part, and each resolve drops it again).
    pub(crate) fn set_spatial(&mut self, node: NodeId, patch: SpatialPatch) {
        let s = &mut self.host.spatial[node.index()];
        let before = (s.transformed(), s.layered());
        let mut changed = false;
        let mut moved = false;
        let mut parts = s.parts;
        patch.apply(&mut parts);
        if parts != s.parts {
            let composed = parts.compose();
            if composed.0.iter().all(|v| v.is_finite()) {
                s.parts = parts;
                s.composed = composed;
                changed = true;
                moved = true;
            }
        }
        if let Some(o) = patch.opacity
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

    /// Sets a node's spatial pin (`Spatial::PIN_*`).
    pub(crate) fn pin_spatial(&mut self, node: NodeId, pin: u8) {
        let s = &mut self.host.spatial[node.index()];
        if s.pin == pin {
            return;
        }
        let before = (s.transformed(), s.layered());
        s.pin = pin;
        if (s.transformed(), s.layered()) != before {
            self.host.revs.structure.bump();
            self.host.dirty.spatial.push(node.0);
        }
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

impl Ui {
    /// Replaces a node's box shadows: its chunk rebuilds (shadows are
    /// geometry in it). Shadows don't tween: a transition snaps them.
    pub(crate) fn set_shadows(&mut self, node: NodeId, shadows: crate::shadow::Shadows) {
        let before = self.host.shadows.get(&node.0).copied().unwrap_or_default();
        if before == shadows {
            return;
        }
        if shadows.is_empty() {
            self.host.shadows.remove(&node.0);
        } else {
            self.host.shadows.insert(node.0, shadows);
        }
        self.host.dirty.content.push(node.0);
        self.host.revs.paint.bump();
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
