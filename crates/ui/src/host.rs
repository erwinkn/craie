//! The retained host tree.
//!
//! React is authoritative for composition; the host stores only what
//! native execution needs. Every node is a 16-byte header in a dense
//! arena indexed by the bridge-assigned id. Per-usage stores sit beside
//! the arena, indexed by the same id:
//!
//! ```text
//! nodes[]        header: parent, child span, kind, flags, generation
//! layout[]       the node's own layout inputs (no shared records)
//! spatial[]      local transform, opacity, scroll offset, z
//! paint[]        fill, border, radius
//! paragraphs[]   UTF-8 text + style span list (text nodes)
//! interaction[]  listener mask, focusable, role
//! list_index[]   item index of a list row
//! ```
//!
//! Children are a `Span` into one capacity-classed pool, so a leaf pays
//! nothing for children. Truly sparse facts (labels, surface payloads,
//! list states, paint orders, layer owners) live in id-keyed maps.
//!
//! Every mutation advances the revisions it can invalidate and queues the
//! dirty work it creates. Queues schedule work; revisions prove validity.

use std::collections::HashMap;

use craie_core::dirty::DirtyQueue;
use craie_core::geom::{Affine, Point, Size};
use craie_core::rev::Rev;
use craie_core::span::{Span, SpanPool};
use craie_layout::LayoutRow;

use crate::mutation::{NIL, NodeKind, Role, TextSpan};

/// Stable node identity, assigned by the React side of the bridge and
/// recycled there immediately after removal. Natively a `NodeId` is a
/// direct index into the arena; the header's `generation` tells a new
/// occupant from an old one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct NodeId(pub u32);

impl NodeId {
    /// Nil / "root list" sentinel. Real ids stay below `DETACHED`.
    pub const NIL: NodeId = NodeId(u32::MAX);
    /// Parent link of a node that exists but is not attached.
    pub const DETACHED: NodeId = NodeId(u32::MAX - 1);

    pub fn is_nil(self) -> bool {
        self == NodeId::NIL
    }

    /// Neither NIL nor DETACHED.
    pub fn is_node(self) -> bool {
        self.0 < NodeId::DETACHED.0
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Root sentinel: children of ROOT are top-level nodes.
pub const ROOT: NodeId = NodeId::NIL;

/// Layout inputs of a node that sends none, and the base every style
/// decodes over: React Native defaults (flex, column, stretch, no
/// shrink), so a cross-platform kit needs no normalization
/// (ARCHITECTURE.md §4).
pub fn default_style() -> LayoutRow {
    LayoutRow::default()
}

/// What inherits where no node sets a color: white, the fallback the
/// facade gives a span (`Host::current_color`).
pub const DEFAULT_COLOR: u32 = 0xFFFF_FFFF;

/// Node ids index dense stores, so they are bounded: 2^24 slots. The
/// bridge allocates ids densely from zero and recycles them.
pub const MAX_NODES: u32 = 1 << 24;

/// Per-node flag bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct NodeFlags(pub u8);

impl NodeFlags {
    pub const NONE: NodeFlags = NodeFlags(0);
    /// The slot holds a live node.
    pub const LIVE: NodeFlags = NodeFlags(1 << 0);
    /// Layout inputs changed; the layout cache up to the root is stale.
    pub const LAYOUT: NodeFlags = NodeFlags(1 << 1);
    /// Text content or metrics changed; the shaped paragraph is stale.
    pub const TEXT: NodeFlags = NodeFlags(1 << 2);
    /// The hit-test reach is stale (`reach.rs`); so is every ancestor's.
    pub const REACH: NodeFlags = NodeFlags(1 << 3);
    /// Its children paint in a sorted order, held in `Host::orders`
    /// (`order.rs`); without it they paint in tree order.
    pub const SORTED: NodeFlags = NodeFlags(1 << 4);
    /// Its paint order is queued for re-sorting.
    pub const ORDER: NodeFlags = NodeFlags(1 << 5);
    /// A layer container: never a hit target itself (`box-none`), and
    /// never sorts below the sibling that holds its owner.
    pub const LAYER: NodeFlags = NodeFlags(1 << 6);
    /// Inert (`interaction_flag::INERT`): no hit testing, focus or
    /// accessibility for it and its subtree (`trap.rs`).
    pub const INERT: NodeFlags = NodeFlags(1 << 7);

    pub fn contains(self, other: NodeFlags) -> bool {
        self.0 & other.0 != 0
    }

    pub fn set(&mut self, other: NodeFlags) {
        self.0 |= other.0;
    }

    pub fn clear(&mut self, other: NodeFlags) {
        self.0 &= !other.0;
    }
}

/// Fixed-size record for every node slot: 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NodeHeader {
    /// Parent id, `NIL` at the root level, `DETACHED` when unattached.
    pub parent: u32,
    /// Ordered children in the host's child pool.
    pub children: Span,
    pub kind: NodeKind,
    pub flags: NodeFlags,
    /// Incremented each time the slot is freed, so a stale
    /// (id, generation) pair never reaches a later occupant.
    pub generation: u16,
}

impl NodeHeader {
    const EMPTY: NodeHeader = NodeHeader {
        parent: NodeId::DETACHED.0,
        children: Span::EMPTY,
        kind: NodeKind::View,
        flags: NodeFlags::NONE,
        generation: 0,
    };

    pub fn is_live(&self) -> bool {
        self.flags.contains(NodeFlags::LIVE)
    }

    pub fn parent(&self) -> NodeId {
        NodeId(self.parent)
    }
}

/// A node's transform as CSS's individual transform properties: the
/// matrix is `translate · rotate · scale · matrix`, applied about the
/// border-box center. Each part is set, tweened and overridden by a
/// variant on its own, so a hover `scale` keeps a base `rotate`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Parts {
    /// x and y in points, then x and y as fractions of the border box
    /// (`calc(x + 50%)` is `[x, 0, 0.5, 0]`).
    pub translate: [f32; 4],
    /// Radians, clockwise on screen, unbounded: 2π is a full turn.
    pub rotate: f32,
    pub scale: [f32; 2],
    /// The free matrix (`transform`), applied first.
    pub matrix: Affine,
}

impl Parts {
    pub const IDENTITY: Parts = Parts {
        translate: [0.0; 4],
        rotate: 0.0,
        scale: [1.0; 2],
        matrix: Affine::IDENTITY,
    };

    /// The parts in CSS order, the percent translate aside (it follows
    /// the size: `Spatial::local`). Rotate's sine and cosine snap to 0
    /// and ±1 within 1e-6, so quarter turns compose exactly: f32
    /// `sin(τ)` is 1.7e-7, which would keep a 360deg turn transformed
    /// (and unsnapped) forever.
    pub fn compose(&self) -> Affine {
        let [tx, ty, _, _] = self.translate;
        let [sx, sy] = self.scale;
        let mut m = Affine::scale(sx, sy).mul(&self.matrix);
        if self.rotate != 0.0 {
            let snap = |v: f32| {
                if v.abs() < 1e-6 {
                    0.0
                } else if (v.abs() - 1.0).abs() < 1e-6 {
                    v.signum()
                } else {
                    v
                }
            };
            let (s, c) = self.rotate.sin_cos();
            let (s, c) = (snap(s), snap(c));
            m = Affine([c, s, -s, c, 0.0, 0.0]).mul(&m);
        }
        m.0[4] += tx;
        m.0[5] += ty;
        m
    }

    /// Every value finite.
    pub fn is_finite(&self) -> bool {
        self.translate.iter().all(|v| v.is_finite())
            && self.rotate.is_finite()
            && self.scale.iter().all(|v| v.is_finite())
            && self.matrix.0.iter().all(|v| v.is_finite())
    }
}

impl Default for Parts {
    fn default() -> Parts {
        Parts::IDENTITY
    }
}

/// Spatial values to write; `None` keeps the row's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpatialPatch {
    pub translate: Option<[f32; 4]>,
    pub rotate: Option<f32>,
    pub scale: Option<[f32; 2]>,
    /// The free matrix.
    pub matrix: Option<Affine>,
    pub opacity: Option<f32>,
}

impl SpatialPatch {
    pub fn is_empty(&self) -> bool {
        *self == SpatialPatch::default()
    }

    /// Writes the given parts into `p`.
    pub fn apply(&self, p: &mut Parts) {
        p.translate = self.translate.unwrap_or(p.translate);
        p.rotate = self.rotate.unwrap_or(p.rotate);
        p.scale = self.scale.unwrap_or(p.scale);
        p.matrix = self.matrix.unwrap_or(p.matrix);
    }
}

/// Spatial state: applied after layout, never an input to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spatial {
    /// The transform as declared (or tweened), part by part.
    pub parts: Parts,
    /// `parts` composed, kept in step by the row writer: readers never
    /// compose.
    pub composed: Affine,
    pub opacity: f32,
    /// Content offset of a scroll container (logical points).
    pub scroll: [f32; 2],
    /// Order among its siblings (`order.rs`): higher paints later.
    pub z: i32,
    /// Held by a looping keyframe animation (`PIN_*`): the transform
    /// record or opacity layer stays while the loop passes identity or
    /// opacity 1, so its frames change no draw topology.
    pub pin: u8,
}

impl Default for Spatial {
    fn default() -> Spatial {
        Spatial {
            parts: Parts::IDENTITY,
            composed: Affine::IDENTITY,
            opacity: 1.0,
            scroll: [0.0; 2],
            z: 0,
            pin: 0,
        }
    }
}

impl Spatial {
    pub const PIN_TRANSFORM: u8 = 1;
    pub const PIN_LAYER: u8 = 2;

    /// Owns a transform record in the scene (drawn in its own space).
    pub fn transformed(&self) -> bool {
        self.pin & Spatial::PIN_TRANSFORM != 0
            || self.composed != Affine::IDENTITY
            || self.parts.translate[2..] != [0.0; 2]
    }

    /// The node's transform for a border box of `size`, about its
    /// center, with the percent translate resolved.
    pub fn local(&self, size: Size) -> Affine {
        if !self.transformed() {
            return Affine::IDENTITY;
        }
        let mut m = self.composed;
        m.0[4] += self.parts.translate[2] * size.width;
        m.0[5] += self.parts.translate[3] * size.height;
        m.about(Point::new(size.width / 2.0, size.height / 2.0))
    }

    /// Composites as an isolated group.
    pub fn layered(&self) -> bool {
        self.pin & Spatial::PIN_LAYER != 0 || self.opacity < 1.0
    }
}

/// Box paint of a view, input, or surface.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoxPaint {
    /// Background fill, 0xRRGGBBAA. Alpha 0 paints nothing.
    pub fill: u32,
    /// Border ring color, 0xRRGGBBAA.
    pub border_color: u32,
    /// Border ring width, logical points.
    pub border_width: f32,
    /// Corner radius, logical points.
    pub radius: f32,
}

/// A text node's paragraph: UTF-8 plus style spans over byte ranges.
/// Span zero starts at byte 0 and carries the base style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Paragraph {
    pub text: String,
    /// Span zero is the base style. `family` indexes `Host::families`.
    pub spans: Vec<TextSpan>,
    /// Each span's primary font, resolved from its family and style once,
    /// when the span was applied.
    pub fonts: Vec<Option<crate::text::fonts::FontInstanceId>>,
    /// Paragraph ops applied to this node, wrapping at 2^32: pointer events
    /// carry it with their span, so JS routes a span by the span table
    /// it came from (a nested Text replaced since routes to the root).
    pub revision: u32,
}

/// Which events a node subscribes to, whether it takes focus, and what
/// it is for assistive technology.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interaction {
    /// Event subscription mask (see `events::mask`).
    pub listeners: u32,
    /// Participates in Tab traversal and accepts click focus.
    pub focusable: bool,
    /// Its text descendants (itself included) form one selection domain.
    pub selectable: bool,
    /// Press flags (`mutation::press`).
    pub press: u8,
    /// A trap's first pick for focus (`trap.rs`).
    pub auto_focus: bool,
    pub role: Role,
    /// States reported while clear (`mutation::reported`).
    pub reported: u8,
    /// A focus group (`group.rs`; its state is in `Ui::groups`): a bit
    /// here spares the walks a map lookup per node.
    pub group: bool,
}

/// A surface node's retained data: kind, parameters, and payload bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceData {
    pub kind: u32,
    pub params: [u32; 4],
    pub payload: Vec<u8>,
}

/// A vector node's asset: its source (payload bytes as sent, or a
/// drawing's key), shared with the source table and other nodes, and
/// their decoding.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VectorData {
    pub bytes: std::sync::Arc<[u8]>,
    /// `None` until a source arrives, and for a drawing that does not
    /// parse (it draws nothing).
    pub asset: Option<std::sync::Arc<craie_vector::asset::Asset>>,
    /// Whether `asset` paints with the inherited color, found once per
    /// source (`Asset::inherits_color` walks the items).
    pub inherits: bool,
}

/// An image node's source. Decode state lives in `Ui::images`.
#[derive(Default)]
pub struct ImageData {
    /// The encoded bytes (the node's payload), shared with decode
    /// requests.
    pub bytes: std::sync::Arc<[u8]>,
    pub fit: crate::image::Fit,
    /// Pixels, once the decoder read the header: the intrinsic size.
    pub natural: Option<[u32; 2]>,
}

/// Revisions. A derived value records the revisions it was built from
/// and is valid while they are unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Revs {
    /// Tree shape, visibility (`display`), and draw topology (transform
    /// records, opacity layers).
    pub structure: Rev,
    pub layout_input: Rev,
    pub text_content: Rev,
    pub text_metrics: Rev,
    pub paint: Rev,
    pub transform: Rev,
    /// Clip topology (`overflow`).
    pub clip: Rev,
    pub semantic: Rev,
    /// Surface payloads and other retained resources.
    pub resource: Rev,
}

/// Dirty work queues, drained by the frame pipeline.
#[derive(Default)]
pub struct DirtyQueues {
    /// Nodes whose layout cache (and every ancestor's) is stale.
    pub layout: DirtyQueue,
    /// Nodes whose scene chunk must be regenerated.
    pub content: DirtyQueue,
    /// Nodes whose chunk paint records need patching (color only).
    pub paint: DirtyQueue,
    /// Nodes whose transform, opacity, or scroll offset changed.
    pub spatial: DirtyQueue,
    /// Nodes whose accessibility projection changed.
    pub semantic: DirtyQueue,
}

pub struct Host {
    nodes: Vec<NodeHeader>,
    child_pool: SpanPool<NodeId>,
    roots: Span,
    live: usize,
    pub layout: Vec<LayoutRow>,
    pub spatial: Vec<Spatial>,
    pub paint: Vec<BoxPaint>,
    pub paragraphs: Vec<Paragraph>,
    /// Font family names spans refer to (interned, grow-only).
    pub families: Vec<String>,
    pub interaction: Vec<Interaction>,
    /// Nodes listening for pointer enter and leave: without them (and
    /// hover variants), hover at rest need not hit-test.
    pub hover_listeners: usize,
    pub labels: HashMap<u32, Box<str>>,
    /// Declared transitions per node (`animation.rs`), id-keyed: few
    /// nodes have any.
    pub transitions: HashMap<u32, Vec<crate::animation::Transition>>,
    /// Declared exits per node (`exit.rs`): what a detach starts.
    pub exits: HashMap<u32, Box<[crate::keyframes::Animation]>>,
    /// The roots whose exit runs (`exit.rs`): detached for the app,
    /// still in the tree. Sorted: membership is a search, and a batch
    /// that cuts thousands stays linear.
    pub exiting: std::collections::BTreeSet<NodeId>,
    pub surfaces: HashMap<u32, SurfaceData>,
    /// Vector nodes' assets, id-keyed.
    pub vectors: HashMap<u32, VectorData>,
    /// Image nodes' encoded bytes and fit, id-keyed (`image.rs`).
    pub images: HashMap<u32, ImageData>,
    /// Decoded vector sources by source bytes: nodes with the same
    /// source share one asset (and so its tessellation).
    vector_sources: HashMap<std::sync::Arc<[u8]>, std::sync::Weak<craie_vector::asset::Asset>>,
    /// `vector_sources` entries after the last sweep of dead ones.
    vector_sources_swept: usize,
    /// Claim sets (`claims.rs`), id-keyed; NIL keys the window list.
    pub claims: HashMap<u32, crate::claims::ClaimSet>,
    /// Inherited colors (`COLOR`) of text, inputs and `currentColor`
    /// drawings, id-keyed: few nodes set one.
    pub colors: HashMap<u32, u32>,
    /// Box shadows (`shadow.rs`), id-keyed: most boxes have none.
    pub shadows: HashMap<u32, crate::shadow::Shadows>,
    /// Item index of a list row (a child of a List node); NIL otherwise.
    pub list_index: Vec<u32>,
    /// List states and scroll anchors (§7).
    pub lists: crate::list::Lists,
    /// Paint orders of the parents flagged `SORTED`, NIL keying the root
    /// level (`order.rs`).
    pub(crate) orders: HashMap<u32, Vec<NodeId>>,
    /// Layer containers' owners (NIL: none), id-keyed.
    pub owners: HashMap<u32, u32>,
    /// `SORTED` and `ORDER` for the root level, which has no header.
    pub(crate) root_flags: NodeFlags,
    /// Parents whose paint order is queued (flagged `ORDER`).
    pub(crate) order_queue: Vec<u32>,
    /// `revs.structure` at the last `refresh_orders`: owners resolve
    /// through the tree, so any structure change re-sorts their parents.
    pub(crate) order_rev: Rev,
    pub revs: Revs,
    pub dirty: DirtyQueues,
    /// Bytes copied from transactions into host stores: paragraph text
    /// and span lists, labels, surface payloads (cost counter).
    pub copied_bytes: u64,
}

impl Default for Host {
    fn default() -> Host {
        Host::new()
    }
}

impl Host {
    /// The host index of family `name` (interned).
    pub fn family(&mut self, name: &str) -> u32 {
        match self.families.iter().position(|f| f == name) {
            Some(i) => i as u32,
            None => {
                self.families.push(name.to_string());
                self.families.len() as u32 - 1
            }
        }
    }

    /// The family name of a span (`""`: the default family).
    pub fn family_name(&self, family: u32) -> &str {
        self.families
            .get(family as usize)
            .map_or("", String::as_str)
    }

    pub fn new() -> Host {
        Host {
            nodes: Vec::new(),
            child_pool: SpanPool::new(NodeId::NIL),
            roots: Span::EMPTY,
            live: 0,
            layout: Vec::new(),
            spatial: Vec::new(),
            paint: Vec::new(),
            paragraphs: Vec::new(),
            families: Vec::new(),
            interaction: Vec::new(),
            hover_listeners: 0,
            labels: HashMap::new(),
            transitions: HashMap::new(),
            exits: HashMap::new(),
            exiting: Default::default(),
            surfaces: HashMap::new(),
            vectors: HashMap::new(),
            images: HashMap::new(),
            vector_sources: HashMap::new(),
            vector_sources_swept: 0,
            claims: HashMap::new(),
            colors: HashMap::new(),
            shadows: HashMap::new(),
            list_index: Vec::new(),
            lists: crate::list::Lists::default(),
            orders: HashMap::new(),
            owners: HashMap::new(),
            root_flags: NodeFlags::NONE,
            order_queue: Vec::new(),
            order_rev: Rev::ZERO,
            revs: Revs::default(),
            dirty: DirtyQueues::default(),
            copied_bytes: 0,
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&NodeHeader> {
        self.nodes.get(id.index()).filter(|n| n.is_live())
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut NodeHeader> {
        self.nodes.get_mut(id.index()).filter(|n| n.is_live())
    }

    /// Whether a vector source is decoded and in use (validation skips
    /// parsing it again).
    pub fn has_vector_source(&self, source: &[u8]) -> bool {
        self.vector_sources
            .get(source)
            .is_some_and(|w| w.strong_count() > 0)
    }

    /// Whether a vector node's asset paints with its inherited color.
    pub fn vector_inherits(&self, id: NodeId) -> bool {
        self.vectors.get(&id.0).is_some_and(|v| v.inherits)
    }

    /// The table's copy of a source, if it holds one (nodes of the same
    /// source share one copy).
    pub fn vector_source_key(&self, source: &[u8]) -> Option<std::sync::Arc<[u8]>> {
        self.vector_sources
            .get_key_value(source)
            .map(|(k, _)| k.clone())
    }

    /// Vector source table entries, live or not yet swept (tests).
    pub fn vector_sources_len(&self) -> usize {
        self.vector_sources.len()
    }

    /// The shared copy and decoding of a vector source: the table's, or
    /// `share()` and `build()` on a miss (`None`: it does not build, and
    /// is not kept). Dead entries are swept when the table has doubled
    /// since the last sweep.
    pub fn vector_source(
        &mut self,
        source: &[u8],
        share: impl FnOnce() -> std::sync::Arc<[u8]>,
        build: impl FnOnce() -> Option<craie_vector::asset::Asset>,
    ) -> (
        std::sync::Arc<[u8]>,
        Option<std::sync::Arc<craie_vector::asset::Asset>>,
    ) {
        if let Some((k, w)) = self.vector_sources.get_key_value(source)
            && let Some(a) = w.upgrade()
        {
            return (k.clone(), Some(a));
        }
        let key = share();
        let Some(a) = build().map(std::sync::Arc::new) else {
            return (key, None);
        };
        self.vector_sources.remove(source);
        self.vector_sources
            .insert(key.clone(), std::sync::Arc::downgrade(&a));
        if self.vector_sources.len() > 2 * self.vector_sources_swept + 64 {
            self.vector_sources.retain(|_, w| w.strong_count() > 0);
            self.vector_sources_swept = self.vector_sources.len();
        }
        (key, Some(a))
    }

    /// The generation of slot `id`, live or free (a free slot's next
    /// occupant takes it).
    pub fn slot_generation(&self, id: NodeId) -> u16 {
        self.nodes.get(id.index()).map_or(0, |n| n.generation)
    }

    pub fn kind(&self, id: NodeId) -> Option<NodeKind> {
        self.node(id).map(|n| n.kind)
    }

    pub fn is_live(&self, id: NodeId) -> bool {
        self.node(id).is_some()
    }

    /// Number of slots ever allocated, including free ones.
    pub fn slot_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Parent link of `id`: a live parent, `DETACHED`, or `NIL` (root
    /// level or absent). Ancestor walks stop on both sentinels.
    pub fn parent(&self, id: NodeId) -> NodeId {
        self.node(id).map_or(NodeId::NIL, |n| n.parent())
    }

    /// Ordered children of `parent`; ROOT yields the top-level list.
    pub fn children(&self, parent: NodeId) -> &[NodeId] {
        let span = if parent.is_nil() {
            self.roots
        } else {
            match self.nodes.get(parent.index()) {
                Some(n) => n.children,
                None => return &[],
            }
        };
        self.child_pool.get(span)
    }

    pub fn child_count(&self, parent: NodeId) -> usize {
        self.children(parent).len()
    }

    /// The `index`-th child of `parent`, NIL when out of range.
    pub fn child_at(&self, parent: NodeId, index: usize) -> NodeId {
        self.children(parent)
            .get(index)
            .copied()
            .unwrap_or(NodeId::NIL)
    }

    /// Child-pool allocation behavior (E10).
    pub fn child_pool_stats(&self) -> craie_core::span::SpanStats {
        self.child_pool.stats
    }

    /// Bytes held by the child pool's backing store.
    pub fn child_pool_bytes(&self) -> usize {
        self.child_pool.backing_bytes()
    }

    fn ensure(&mut self, id: NodeId) {
        let n = id.index() + 1;
        if self.nodes.len() < n {
            self.nodes.resize(n, NodeHeader::EMPTY);
            self.layout.resize_with(n, default_style);
            self.spatial.resize(n, Spatial::default());
            self.paint.resize(n, BoxPaint::default());
            self.paragraphs.resize_with(n, Paragraph::default);
            self.interaction.resize(n, Interaction::default());
            self.list_index.resize(n, NIL);
        }
    }

    /// Creates a node at the bridge-assigned id with default stores.
    /// Returns false when the slot is live.
    pub fn create(&mut self, id: NodeId, kind: NodeKind) -> bool {
        self.ensure(id);
        let i = id.index();
        if self.nodes[i].is_live() {
            return false;
        }
        let generation = self.nodes[i].generation;
        self.nodes[i] = NodeHeader {
            kind,
            flags: NodeFlags::LIVE,
            generation,
            ..NodeHeader::EMPTY
        };
        // A recycled slot starts from defaults: nothing of the previous
        // occupant may leak into the new node.
        self.layout[i] = default_style();
        self.spatial[i] = Spatial::default();
        self.paint[i] = BoxPaint::default();
        let p = &mut self.paragraphs[i];
        p.text.clear();
        p.spans.clear();
        // Fonts resolve at the first paragraph op (`executor`).
        p.fonts.clear();
        p.revision = 0;
        if kind == NodeKind::Text {
            p.spans.push(TextSpan::default());
        }
        self.set_listeners(i, 0);
        self.interaction[i] = Interaction::default();
        self.labels.remove(&id.0);
        self.transitions.remove(&id.0);
        self.exits.remove(&id.0);
        self.surfaces.remove(&id.0);
        self.vectors.remove(&id.0);
        self.images.remove(&id.0);
        self.claims.remove(&id.0);
        self.colors.remove(&id.0);
        self.shadows.remove(&id.0);
        self.orders.remove(&id.0);
        self.owners.remove(&id.0);
        self.list_index[i] = NIL;
        self.lists.forget(id.0);
        if kind == NodeKind::Surface {
            self.surfaces.insert(id.0, SurfaceData::default());
        }
        if kind == NodeKind::Vector {
            self.vectors.insert(id.0, VectorData::default());
        }
        if kind == NodeKind::Image {
            self.images.insert(id.0, ImageData::default());
        }
        self.live += 1;
        self.revs.structure.bump();
        self.dirty.content.push(id.0);
        self.dirty.semantic.push(id.0);
        true
    }

    /// Inserts `child` before `before` under `parent` (NIL `before`
    /// appends, NIL `parent` is the root level). Moves the child if it is
    /// attached elsewhere.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: NodeId) {
        self.detach(child);
        let mut span = if parent.is_nil() {
            self.roots
        } else {
            self.nodes[parent.index()].children
        };
        let list = self.child_pool.get(span);
        let pos = if before.is_nil() {
            list.len()
        } else {
            list.iter().position(|&c| c == before).unwrap_or(list.len())
        };
        self.child_pool.insert(&mut span, pos, child);
        if parent.is_nil() {
            self.roots = span;
        } else {
            self.nodes[parent.index()].children = span;
        }
        self.nodes[child.index()].parent = parent.0;
        self.revs.structure.bump();
        // A child with a z or an owner sorts; so does any child of a
        // parent that already does.
        if self.spatial[child.index()].z != 0
            || self.nodes[child.index()].flags.contains(NodeFlags::LAYER)
            || self.order_flags(parent).contains(NodeFlags::SORTED)
        {
            self.queue_order(parent);
        }
        self.mark_layout(child);
        self.dirty.semantic.push(child.0);
        if parent.is_node() {
            self.dirty.semantic.push(parent.0);
        }
    }

    /// Unlinks `id` from its parent without freeing the slot.
    pub fn detach(&mut self, id: NodeId) {
        let Some(node) = self.node(id) else { return };
        let parent = node.parent();
        if parent == NodeId::DETACHED {
            return;
        }
        let mut span = if parent.is_nil() {
            self.roots
        } else {
            self.nodes[parent.index()].children
        };
        if let Some(pos) = self.child_pool.get(span).iter().position(|&c| c == id) {
            self.child_pool.remove(&mut span, pos);
        }
        if parent.is_nil() {
            self.roots = span;
        } else {
            self.nodes[parent.index()].children = span;
        }
        self.nodes[id.index()].parent = NodeId::DETACHED.0;
        self.revs.structure.bump();
        if self.order_flags(parent).contains(NodeFlags::SORTED) {
            self.queue_order(parent);
        }
        if parent.is_node() {
            // The parent's layout depended on this child.
            self.mark_layout(parent);
            self.dirty.semantic.push(parent.0);
        }
    }

    /// Detaches `id`, orphans its children, and frees the slot. The
    /// bridge removes every node of a deleted subtree with its own op;
    /// children removed later find themselves already detached.
    pub fn remove(&mut self, id: NodeId) {
        if !self.is_live(id) {
            return;
        }
        self.detach(id);
        let i = id.index();
        let mut kids = self.nodes[i].children;
        for k in 0..kids.len() {
            let c = self.child_pool.get(kids)[k];
            if let Some(n) = self.nodes.get_mut(c.index()) {
                n.parent = NodeId::DETACHED.0;
            }
        }
        self.child_pool.free(&mut kids);
        let p = &mut self.paragraphs[i];
        p.text = String::new();
        p.spans = Vec::new();
        self.set_listeners(i, 0);
        self.labels.remove(&id.0);
        self.transitions.remove(&id.0);
        self.exits.remove(&id.0);
        self.surfaces.remove(&id.0);
        self.vectors.remove(&id.0);
        self.images.remove(&id.0);
        self.claims.remove(&id.0);
        self.colors.remove(&id.0);
        self.shadows.remove(&id.0);
        self.orders.remove(&id.0);
        self.owners.remove(&id.0);
        // Its layers lose their owner: a reuse of the id must not adopt
        // them.
        for owner in self.owners.values_mut() {
            if *owner == id.0 {
                *owner = NIL;
            }
        }
        self.list_index[i] = NIL;
        self.lists.forget(id.0);
        let generation = self.nodes[i].generation.wrapping_add(1);
        self.nodes[i] = NodeHeader {
            generation,
            ..NodeHeader::EMPTY
        };
        self.live -= 1;
        self.revs.structure.bump();
        // The frame pipeline frees the node's chunk.
        self.dirty.content.push(id.0);
    }

    /// Flags `id` layout-dirty and queues it once for cache invalidation.
    /// Its reach is stale too: a layout input (display, overflow) or its
    /// children changed.
    pub fn mark_layout(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get_mut(id.index()).filter(|n| n.is_live()) {
            n.flags.set(NodeFlags::LAYOUT);
            self.dirty.layout.push(id.0);
            self.touch(id);
        }
    }

    /// Marks `id`'s hit-test reach stale (`reach.rs`), with its
    /// ancestors up to the first one already stale.
    pub fn touch(&mut self, id: NodeId) {
        let mut cur = id;
        while let Some(n) = self.node_mut(cur) {
            if cur != id && n.flags.contains(NodeFlags::REACH) {
                break;
            }
            n.flags.set(NodeFlags::REACH);
            cur = n.parent();
        }
    }

    pub fn reach_stale(&self, id: NodeId) -> bool {
        self.node(id)
            .is_some_and(|n| n.flags.contains(NodeFlags::REACH))
    }

    pub(crate) fn reach_fresh(&mut self, id: NodeId) {
        if let Some(n) = self.node_mut(id) {
            n.flags.clear(NodeFlags::REACH);
        }
    }

    /// Flags a text node's shaped paragraph stale and queues relayout
    /// and chunk regeneration.
    pub fn mark_text(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get_mut(id.index()).filter(|n| n.is_live()) {
            n.flags.set(NodeFlags::TEXT);
        }
        self.mark_layout(id);
        self.dirty.content.push(id.0);
    }

    /// Paragraph of a text node.
    pub fn paragraph(&self, id: NodeId) -> Option<&Paragraph> {
        self.node(id)
            .filter(|n| n.kind == NodeKind::Text)
            .map(|_| &self.paragraphs[id.index()])
    }

    /// The nearest inherited color: the node's own `COLOR` or its
    /// nearest ancestor's.
    pub fn inherited_color(&self, id: NodeId) -> Option<u32> {
        if self.colors.is_empty() {
            return None;
        }
        let mut cur = id;
        while cur.is_node() {
            if let Some(&c) = self.colors.get(&cur.0) {
                return Some(c);
            }
            cur = self.parent(cur);
        }
        None
    }

    /// The color an input's text and a drawing's `currentColor` paint
    /// with: the nearest inherited color, else `DEFAULT_COLOR`.
    pub fn current_color(&self, id: NodeId) -> u32 {
        self.inherited_color(id).unwrap_or(DEFAULT_COLOR)
    }

    /// The color a span of text node `id` draws in.
    pub fn span_color(&self, id: NodeId, s: &TextSpan) -> u32 {
        if s.inherit_color {
            self.inherited_color(id).unwrap_or(s.color)
        } else {
            s.color
        }
    }

    pub fn style(&self, id: NodeId) -> &LayoutRow {
        &self.layout[id.index()]
    }

    /// `display: none` on the node itself.
    pub fn display_none(&self, id: NodeId) -> bool {
        self.layout
            .get(id.index())
            .is_some_and(|s| s.display() == taffy::Display::None)
    }

    pub fn label(&self, id: NodeId) -> Option<&str> {
        self.labels.get(&id.0).map(|s| &**s)
    }

    /// Sets slot `i`'s listener mask, keeping `hover_listeners`.
    pub fn set_listeners(&mut self, i: usize, listeners: u32) {
        let hovers = |l: u32| (l & crate::events::mask::POINTER_ENTER_LEAVE != 0) as usize;
        let l = &mut self.interaction[i].listeners;
        self.hover_listeners = self.hover_listeners + hovers(listeners) - hovers(*l);
        *l = listeners;
    }

    pub fn interaction(&self, id: NodeId) -> Interaction {
        self.interaction
            .get(id.index())
            .copied()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: NodeKind = NodeKind::View;
    const TEXT: NodeKind = NodeKind::Text;

    #[test]
    fn create_and_index() {
        let mut host = Host::new();
        assert!(host.create(NodeId(0), VIEW));
        assert!(host.create(NodeId(5), TEXT));
        assert_eq!(host.len(), 2);
        assert_eq!(host.kind(NodeId(5)), Some(TEXT));
        assert!(host.node(NodeId(3)).is_none());
        assert!(!host.create(NodeId(0), TEXT));
    }

    #[test]
    fn insert_append_and_move() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW);
        for i in 1..=3u32 {
            host.create(NodeId(i), TEXT);
            host.insert_before(NodeId(0), NodeId(i), NodeId::NIL);
        }
        assert_eq!(host.children(NodeId(0)), [NodeId(1), NodeId(2), NodeId(3)]);
        host.create(NodeId(9), TEXT);
        host.insert_before(NodeId(0), NodeId(9), NodeId(2));
        assert_eq!(
            host.children(NodeId(0)),
            [NodeId(1), NodeId(9), NodeId(2), NodeId(3)]
        );
        // Re-inserting moves.
        host.insert_before(NodeId(0), NodeId(1), NodeId::NIL);
        assert_eq!(
            host.children(NodeId(0)),
            [NodeId(9), NodeId(2), NodeId(3), NodeId(1)]
        );
        assert_eq!(host.parent(NodeId(2)), NodeId(0));
    }

    #[test]
    fn remove_recycles_slot_and_bumps_generation() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW);
        host.create(NodeId(1), TEXT);
        host.insert_before(NodeId(0), NodeId(1), NodeId::NIL);
        host.paragraphs[1].text.push_str("old");
        let gen0 = host.node(NodeId(1)).unwrap().generation;
        host.remove(NodeId(1));
        assert_eq!(host.len(), 1);
        assert!(host.node(NodeId(1)).is_none());
        assert!(host.children(NodeId(0)).is_empty());
        host.create(NodeId(1), TEXT);
        assert_eq!(
            host.node(NodeId(1)).unwrap().generation,
            gen0.wrapping_add(1)
        );
        assert_eq!(host.paragraphs[1].text, "");
    }

    #[test]
    fn remove_orphans_children() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW);
        host.create(NodeId(1), VIEW);
        host.insert_before(ROOT, NodeId(0), NodeId::NIL);
        host.insert_before(NodeId(0), NodeId(1), NodeId::NIL);
        host.remove(NodeId(0));
        assert_eq!(host.parent(NodeId(1)), NodeId::DETACHED);
        assert!(host.children(ROOT).is_empty());
        // The slot is reused; the orphan's later remove must not touch it.
        host.create(NodeId(0), VIEW);
        host.remove(NodeId(1));
        assert!(host.is_live(NodeId(0)));
    }

    #[test]
    fn header_stays_compact() {
        assert_eq!(std::mem::size_of::<NodeHeader>(), 16);
    }
}
