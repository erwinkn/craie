//! State styles (ARCHITECTURE.md §13): scopes with state bits, variant
//! tables, and the restyle pass.
//!
//! A scope is a node whose state bits variants read: app bits (`STATES`)
//! and the input bits native owns (hover, pressed, focus). A node with a
//! variant table (`VARIANTS`) keeps a base, what its own ops declare,
//! and variants: values that apply while their conditions hold. Its
//! resolved style is the base with every active variant overlaid, least
//! specific first. When a scope's bits, the environment, or a base
//! change, the node is queued; `restyle` resolves the queue and declares
//! what changed through the animation driver, so transitions tween state
//! changes. None of it waits for JS.

use std::collections::HashMap;

use craie_core::dirty::DirtyQueue;
use craie_core::geom::Affine;
use craie_layout::LayoutRow;
use taffy::Style;

use crate::animation::{Prop, Value};
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::NodeKind;
use crate::ui::Ui;
use crate::wire::field;

/// State bits of a scope. Bit index = rank: at equal depth, the variant
/// on the later rank wins.
pub mod state_bit {
    /// Bits 0..54: custom states, in declaration order.
    pub const CUSTOM: u64 = (1 << 54) - 1;
    pub const HOVER: u64 = 1 << 54;
    pub const FOCUS_WITHIN: u64 = 1 << 55;
    pub const FOCUS_VISIBLE: u64 = 1 << 56;
    pub const FOCUS_VISIBLE_WITHIN: u64 = 1 << 57;
    pub const EXPANDED: u64 = 1 << 58;
    pub const SELECTED: u64 = 1 << 59;
    pub const CHECKED: u64 = 1 << 60;
    pub const HIGHLIGHTED: u64 = 1 << 61;
    pub const PRESSED: u64 = 1 << 62;
    pub const DISABLED: u64 = 1 << 63;
    /// Owned by native: `STATES` may not set them.
    pub const INPUT: u64 = HOVER | FOCUS_WITHIN | FOCUS_VISIBLE | FOCUS_VISIBLE_WITHIN | PRESSED;
}

/// Environment bits (the window's). Their rank is 64 + bit index.
pub mod env_bit {
    /// Logical width at most the narrow breakpoint (default 1023).
    pub const NARROW: u8 = 1 << 0;
    /// Logical width at most the compact breakpoint (default 639).
    pub const COMPACT: u8 = 1 << 1;
    /// A touch-first device: no hover.
    pub const TOUCH: u8 = 1 << 2;
    pub const REDUCED_MOTION: u8 = 1 << 3;
    pub const ALL: u8 = 0xF;
}

/// `Values` presence bits, in wire order.
pub mod value_field {
    pub const FILL: u8 = 1 << 0;
    /// Color and width, together.
    pub const BORDER: u8 = 1 << 1;
    pub const RADIUS: u8 = 1 << 2;
    /// The inherited text color (set or cleared).
    pub const COLOR: u8 = 1 << 3;
    pub const OPACITY: u8 = 1 << 4;
    /// The whole matrix.
    pub const TRANSFORM: u8 = 1 << 5;
    /// The layout fields in `Values::layout_mask`.
    pub const LAYOUT: u8 = 1 << 6;
    /// Values only nodes with a box hold.
    pub const BOX: u8 = FILL | BORDER | RADIUS;
    pub const ALL: u8 = (1 << 7) - 1;
}

/// A set of style values: a base or a variant's. Fields outside `mask`
/// are ignored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Values {
    pub mask: u8,
    pub fill: u32,
    /// Border color and width.
    pub border: (u32, f32),
    pub radius: f32,
    /// `None`: no inherited color of its own.
    pub color: Option<u32>,
    pub opacity: f32,
    pub transform: Affine,
    /// Wire style field bits (`wire::field`) of `layout` that apply.
    pub layout_mask: u64,
    pub layout: LayoutRow,
}

impl Default for Values {
    fn default() -> Values {
        Values {
            mask: 0,
            fill: 0,
            border: (0, 0.0),
            radius: 0.0,
            color: None,
            opacity: 1.0,
            transform: Affine::IDENTITY,
            layout_mask: 0,
            layout: crate::host::default_style(),
        }
    }
}

impl Values {
    /// In range: finite, opacity in [0, 1], known bits.
    pub fn valid(&self) -> bool {
        self.mask & !value_field::ALL == 0
            && self.layout_mask & !field::ALL == 0
            && self.border.1.is_finite()
            && self.radius.is_finite()
            && (0.0..=1.0).contains(&self.opacity)
            && self.transform.0.iter().all(|v| v.is_finite())
    }

    /// Overlays `v`'s present values, property by property. Layout goes
    /// through `layout` (made from this base on first use).
    fn overlay(&mut self, v: &Variant, layout: &mut Option<Style>) {
        use value_field::*;
        let (m, x) = (v.values.mask, &v.values);
        if m & FILL != 0 {
            self.fill = x.fill;
        }
        if m & BORDER != 0 {
            self.border = x.border;
        }
        if m & RADIUS != 0 {
            self.radius = x.radius;
        }
        if m & COLOR != 0 {
            self.color = x.color;
        }
        if m & OPACITY != 0 {
            self.opacity = x.opacity;
        }
        if m & TRANSFORM != 0 {
            self.transform = x.transform;
        }
        if let Some(src) = &v.layout {
            let dst = layout.get_or_insert_with(|| self.layout.to_taffy());
            copy_fields(dst, src, x.layout_mask);
        }
    }
}

/// Copies the wire style fields in `mask` from `src` to `dst`.
fn copy_fields(dst: &mut Style, src: &Style, mask: u64) {
    macro_rules! copy {
        ($($bit:ident => $($f:ident),+;)*) => {
            $(if mask & field::$bit != 0 { $(dst.$f = src.$f.clone();)+ })*
        };
    }
    copy! {
        DISPLAY => display;
        POSITION => position;
        FLEX_DIRECTION => flex_direction;
        FLEX_WRAP => flex_wrap;
        JUSTIFY_CONTENT => justify_content;
        ALIGN_ITEMS => align_items;
        ALIGN_CONTENT => align_content;
        ALIGN_SELF => align_self;
        GAP => gap;
        SIZE => size;
        MIN_SIZE => min_size;
        MAX_SIZE => max_size;
        PADDING => padding;
        MARGIN => margin;
        BORDER => border;
        INSET => inset;
        FLEX_BASIS => flex_basis;
        FLEX_GROW => flex_grow;
        FLEX_SHRINK => flex_shrink;
        ASPECT_RATIO => aspect_ratio;
        OVERFLOW => overflow;
    }
}

/// One condition of a variant as sent: scope `scope` holds every bit of
/// `mask`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermDecl {
    pub scope: u32,
    pub mask: u64,
}

/// A variant as sent: its values apply while every term holds and the
/// environment has every bit of `env`.
#[derive(Clone, Debug, PartialEq)]
pub struct VariantDecl {
    pub terms: Vec<TermDecl>,
    pub env: u8,
    pub values: Values,
}

/// A term as stored: the scope's generation when the table was applied,
/// so a later occupant of the id never satisfies it.
#[derive(Clone, Copy, Debug)]
struct Term {
    scope: u32,
    generation: u16,
    mask: u64,
}

#[derive(Clone, Debug)]
struct Variant {
    terms: Vec<Term>,
    env: u8,
    values: Values,
    /// The layout values as a style, when there are any.
    layout: Option<Style>,
}

/// A node's variant table.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    /// What the node's own ops declare. Always full: every property the
    /// node has.
    pub base: Values,
    /// The last resolution declared: restyle declares only what differs.
    pub(crate) resolved: Values,
    /// Ascending specificity: (depth, ranks, declaration order).
    variants: Vec<Variant>,
    /// Some variant reads the environment.
    uses_env: bool,
}

/// A scope: its bits, and the nodes whose tables read them (exact).
#[derive(Clone, Debug)]
pub(crate) struct Scope {
    generation: u16,
    app: u64,
    input: u64,
    pub dependents: Vec<u32>,
}

impl Scope {
    fn new(generation: u16) -> Scope {
        Scope {
            generation,
            app: 0,
            input: 0,
            dependents: Vec::new(),
        }
    }

    /// The bits variants see: disabled stops hover and pressed, touch
    /// stops hover.
    fn effective(&self, env: u8) -> u64 {
        let mut b = self.app | self.input;
        if b & state_bit::DISABLED != 0 {
            b &= !(state_bit::HOVER | state_bit::PRESSED);
        }
        if env & env_bit::TOUCH != 0 {
            b &= !state_bit::HOVER;
        }
        b
    }
}

/// State-style stores, id-keyed: few nodes are scopes or have tables.
pub struct States {
    pub(crate) scopes: HashMap<u32, Scope>,
    pub(crate) tables: HashMap<u32, Table>,
    /// Nodes whose table must resolve again.
    pub(crate) queue: DirtyQueue,
    scratch: Vec<u32>,
    /// Scopes holding input bits after the last refresh.
    held: Vec<(u32, u64)>,
    next_held: Vec<(u32, u64)>,
    pub(crate) env: u8,
    narrow_max: f32,
    compact_max: f32,
    /// Keyboard modality: the last key or pointer press was a key.
    pub(crate) keyboard: bool,
}

impl Default for States {
    fn default() -> States {
        States {
            scopes: HashMap::new(),
            tables: HashMap::new(),
            queue: DirtyQueue::new(),
            scratch: Vec::new(),
            held: Vec::new(),
            next_held: Vec::new(),
            env: 0,
            narrow_max: 1023.0,
            compact_max: 639.0,
            keyboard: false,
        }
    }
}

impl States {
    fn active(&self, v: &Variant) -> bool {
        v.env & !self.env == 0
            && v.terms.iter().all(|t| {
                self.scopes.get(&t.scope).is_some_and(|s| {
                    s.generation == t.generation && t.mask & !s.effective(self.env) == 0
                })
            })
    }

    /// The base with every active variant overlaid.
    fn resolve(&self, t: &Table) -> Values {
        let mut out = t.base;
        let mut layout = None;
        for v in t.variants.iter().filter(|v| self.active(v)) {
            out.overlay(v, &mut layout);
        }
        if let Some(s) = layout {
            out.layout = LayoutRow::from(&s);
        }
        out
    }

    /// Removes `id` from the dependents of the scopes `t` reads (those
    /// still the occupant it read).
    fn unlink(&mut self, id: u32, t: &Table) {
        for term in t.variants.iter().flat_map(|v| &v.terms) {
            if let Some(s) = self.scopes.get_mut(&term.scope)
                && s.generation == term.generation
                && let Some(k) = s.dependents.iter().position(|&d| d == id)
            {
                s.dependents.swap_remove(k);
            }
        }
    }

    fn queue_dependents(&mut self, scope: u32) {
        if let Some(s) = self.scopes.get(&scope) {
            for &d in &s.dependents {
                self.queue.push(d);
            }
        }
    }

    /// Queues every table that reads `changed` environment bits (all of
    /// them for touch: it masks hover).
    fn queue_env(&mut self, changed: u8) {
        let all = changed & env_bit::TOUCH != 0;
        for (&id, t) in &self.tables {
            if all || t.uses_env {
                self.queue.push(id);
            }
        }
    }
}

/// Specificity: (depth, ranks). Depth counts the bits a variant needs;
/// ranks is their union with environment bits above the state bits, so
/// comparing it as an integer compares the highest differing rank.
fn specificity(d: &VariantDecl) -> (u32, u128) {
    let depth = d.terms.iter().map(|t| t.mask.count_ones()).sum::<u32>() + d.env.count_ones();
    let ranks = d.terms.iter().fold(0u128, |r, t| r | t.mask as u128) | (d.env as u128) << 64;
    (depth, ranks)
}

impl Ui {
    /// Resolves the queued tables and declares what changed. Runs at the
    /// end of `execute` and `dispatch` and at the top of `render`.
    pub(crate) fn restyle(&mut self) {
        self.refresh_input_bits();
        if self.states.queue.is_empty() {
            return;
        }
        let mut ids = std::mem::take(&mut self.states.scratch);
        self.states.queue.drain_into(&mut ids);
        for &id in &ids {
            let Some(t) = self.states.tables.get(&id) else {
                continue;
            };
            let next = self.states.resolve(t);
            let prev = t.resolved;
            if next == prev {
                continue;
            }
            if let Some(t) = self.states.tables.get_mut(&id) {
                t.resolved = next;
            }
            self.declare_values(NodeId(id), &prev, &next);
        }
        self.states.scratch = ids;
    }

    /// Declares the values of `next` that differ from `prev`.
    fn declare_values(&mut self, node: NodeId, prev: &Values, next: &Values) {
        use value_field::*;
        let m = next.mask;
        if m & LAYOUT != 0 && prev.layout != next.layout {
            self.declare_layout(node, next.layout.to_taffy());
        }
        let changed = |bit: u8, same: bool| m & bit != 0 && !same;
        let transform = changed(TRANSFORM, prev.transform == next.transform);
        let opacity = changed(OPACITY, prev.opacity == next.opacity);
        if transform || opacity {
            self.declare_spatial(
                node,
                transform.then_some(next.transform),
                opacity.then_some(next.opacity),
            );
        }
        let fill = changed(FILL, prev.fill == next.fill);
        let radius = changed(RADIUS, prev.radius == next.radius);
        let border = changed(BORDER, prev.border == next.border);
        if fill || radius || border {
            self.declare_paint(
                node,
                fill.then_some(next.fill),
                radius.then_some(next.radius),
                border.then_some(next.border),
            );
        }
        if changed(COLOR, prev.color == next.color) {
            self.declare_color(node, next.color);
        }
    }

    /// Recomputes the input bits from hover, the primary press, and
    /// focus: O(depth) over their ancestor chains. Queues the dependents
    /// of scopes whose bits changed.
    fn refresh_input_bits(&mut self) {
        if self.states.scopes.is_empty() && self.states.held.is_empty() {
            return;
        }
        let mut next = std::mem::take(&mut self.states.next_held);
        next.clear();
        let mut add = |ui: &Ui, from: Option<NodeId>, own: u64, within: u64| {
            let Some(from) = from else { return };
            for (k, n) in ui.ancestors(from).enumerate() {
                if !ui.states.scopes.contains_key(&n.0) {
                    continue;
                }
                let bits = if k == 0 { own | within } else { within };
                match next.iter_mut().find(|e| e.0 == n.0) {
                    Some(e) => e.1 |= bits,
                    None => next.push((n.0, bits)),
                }
            }
        };
        add(self, self.hover, 0, state_bit::HOVER);
        let pressed = self.pressed.filter(|_| self.pressed_primary);
        add(self, pressed, 0, state_bit::PRESSED);
        let visible = self
            .focus
            .is_some_and(|f| self.states.keyboard || self.host.kind(f) == Some(NodeKind::Input));
        let (own, within) = if visible {
            (
                state_bit::FOCUS_VISIBLE,
                state_bit::FOCUS_WITHIN | state_bit::FOCUS_VISIBLE_WITHIN,
            )
        } else {
            (0, state_bit::FOCUS_WITHIN)
        };
        add(self, self.focus, own, within);
        let st = &mut self.states;
        let held = std::mem::take(&mut st.held);
        for &(id, _) in &held {
            if !next.iter().any(|e| e.0 == id) {
                set_input(st, id, 0);
            }
        }
        for &(id, bits) in &next {
            set_input(st, id, bits);
        }
        st.next_held = held;
        st.held = next;
    }

    /// Sets scope `id`'s app bits (`STATES`); it becomes a scope.
    pub(crate) fn set_app_bits(&mut self, id: u32, bits: u64) {
        let generation = self.host.node(NodeId(id)).map_or(0, |n| n.generation);
        let st = &mut self.states;
        let s = st
            .scopes
            .entry(id)
            .or_insert_with(|| Scope::new(generation));
        if s.app != bits {
            s.app = bits;
            st.queue_dependents(id);
        }
    }

    /// Replaces the node's variant table; an empty list removes it and
    /// declares the base.
    pub(crate) fn set_variants(&mut self, id: u32, decls: &[VariantDecl]) {
        let old = self.states.tables.remove(&id);
        if let Some(t) = &old {
            self.states.unlink(id, t);
        }
        if decls.is_empty() {
            if let Some(t) = old {
                self.declare_values(NodeId(id), &t.resolved, &t.base);
            }
            return;
        }
        let (base, resolved) = match old {
            Some(t) => (t.base, t.resolved),
            None => {
                let b = self.capture_base(NodeId(id));
                (b, b)
            }
        };
        let mut keyed: Vec<((u32, u128), Variant)> = decls
            .iter()
            .map(|d| {
                let terms = d
                    .terms
                    .iter()
                    .map(|t| Term {
                        scope: t.scope,
                        generation: self.host.node(NodeId(t.scope)).map_or(0, |n| n.generation),
                        mask: t.mask,
                    })
                    .collect();
                let layout =
                    (d.values.mask & value_field::LAYOUT != 0).then(|| d.values.layout.to_taffy());
                let v = Variant {
                    terms,
                    env: d.env,
                    values: d.values,
                    layout,
                };
                (specificity(d), v)
            })
            .collect();
        // Stable: declaration order breaks ties.
        keyed.sort_by_key(|(k, _)| *k);
        let variants: Vec<Variant> = keyed.into_iter().map(|(_, v)| v).collect();
        let st = &mut self.states;
        let mut linked: Vec<u32> = Vec::new();
        for t in variants.iter().flat_map(|v| &v.terms) {
            if linked.contains(&t.scope) {
                continue;
            }
            linked.push(t.scope);
            let s = st
                .scopes
                .entry(t.scope)
                .or_insert_with(|| Scope::new(t.generation));
            if s.generation != t.generation {
                *s = Scope::new(t.generation);
            }
            s.dependents.push(id);
        }
        let uses_env = variants.iter().any(|v| v.env != 0);
        st.tables.insert(
            id,
            Table {
                base,
                resolved,
                variants,
                uses_env,
            },
        );
        st.queue.push(id);
    }

    /// The node's values as its own ops declared them: the rows, except
    /// that a running animation's target replaces the value in flight.
    fn capture_base(&self, node: NodeId) -> Values {
        use value_field::*;
        let i = node.index();
        let declared = |prop: Prop| {
            self.animations
                .find(node, prop)
                .map(|k| self.animations.active[k].declared)
        };
        let s = self.host.spatial[i];
        let mut v = Values {
            mask: TRANSFORM | OPACITY | COLOR | LAYOUT,
            layout_mask: field::ALL,
            transform: s.transform,
            opacity: s.opacity,
            color: self.host.colors.get(&node.0).copied(),
            layout: self.host.layout[i],
            ..Values::default()
        };
        if self.host.kind(node).is_some_and(NodeKind::has_box) {
            let p = self.host.paint[i];
            v.mask |= BOX;
            v.fill = p.fill;
            v.border = (p.border_color, p.border_width);
            v.radius = p.radius;
        }
        for prop in Prop::ALL {
            if let Some(d) = declared(prop) {
                set_base(&mut v, prop, d);
            }
        }
        v
    }

    /// Whether the node's styles go through a table: its own ops then
    /// set the base (`base_mut`) instead of the rows.
    pub(crate) fn base_mut(&mut self, id: u32) -> Option<&mut Values> {
        let t = self.states.tables.get_mut(&id)?;
        self.states.queue.push(id);
        Some(&mut t.base)
    }

    /// Drops the node's scope and table (the slot is freed or reused):
    /// terms on it turn false, so its dependents restyle.
    pub(crate) fn forget_states(&mut self, node: NodeId) {
        let st = &mut self.states;
        if let Some(s) = st.scopes.remove(&node.0) {
            for d in s.dependents {
                st.queue.push(d);
            }
            st.held.retain(|e| e.0 != node.0);
        }
        if let Some(t) = st.tables.remove(&node.0) {
            st.unlink(node.0, &t);
        }
    }

    /// Updates the width breakpoints from the frame size (logical).
    pub(crate) fn update_env(&mut self, size: Size) {
        let st = &self.states;
        let mut env = st.env & (env_bit::TOUCH | env_bit::REDUCED_MOTION);
        if size.width <= st.narrow_max {
            env |= env_bit::NARROW;
        }
        if size.width <= st.compact_max {
            env |= env_bit::COMPACT;
        }
        self.set_env(env);
    }

    fn set_env(&mut self, env: u8) {
        let changed = self.states.env ^ env;
        if changed == 0 {
            return;
        }
        self.states.env = env;
        self.states.queue_env(changed);
        self.force_paint = true;
    }

    /// Sets the breakpoints (`ENVIRONMENT`); the next frame applies them.
    pub(crate) fn set_breakpoints(&mut self, narrow_max: f32, compact_max: f32) {
        self.states.narrow_max = narrow_max;
        self.states.compact_max = compact_max;
        self.force_paint = true;
    }

    /// A touch-first device: hover never holds. The platform sets it.
    pub fn set_touch(&mut self, on: bool) {
        let env = self.states.env & !env_bit::TOUCH;
        self.set_env(env | if on { env_bit::TOUCH } else { 0 });
    }

    /// The user asked for reduced motion. The platform sets it.
    pub fn set_reduced_motion(&mut self, on: bool) {
        let env = self.states.env & !env_bit::REDUCED_MOTION;
        self.set_env(env | if on { env_bit::REDUCED_MOTION } else { 0 });
    }

    /// The environment bits.
    pub fn env_bits(&self) -> u8 {
        self.states.env
    }

    /// Scope `id`'s bits as variants see them (0: not a scope).
    pub fn state_bits(&self, id: NodeId) -> u64 {
        self.states
            .scopes
            .get(&id.0)
            .filter(|s| {
                self.host
                    .node(id)
                    .is_some_and(|n| n.generation == s.generation)
            })
            .map_or(0, |s| s.effective(self.states.env))
    }

    /// Whether the node has a variant table.
    pub fn has_variants(&self, id: NodeId) -> bool {
        self.states.tables.contains_key(&id.0)
    }
}

/// Sets scope `id`'s input bits, queueing its dependents on a change.
fn set_input(st: &mut States, id: u32, bits: u64) {
    if let Some(s) = st.scopes.get_mut(&id)
        && s.input != bits
    {
        s.input = bits;
        st.queue_dependents(id);
    }
}

/// Sets the base's value of an animated property.
pub(crate) fn set_base(v: &mut Values, prop: Prop, value: Value) {
    match (prop, value) {
        (Prop::Transform, Value::Transform(t)) => v.transform = t,
        (Prop::Opacity, Value::Opacity(o)) => v.opacity = o,
        (Prop::Fill, Value::Color(c)) => v.fill = c,
        (Prop::BorderColor, Value::Color(c)) => v.border.0 = c,
        (Prop::Color, Value::Color(c)) => v.color = Some(c),
        (p, value) if p.is_layout() => crate::animation::set_row_field(&mut v.layout, p, value),
        _ => {}
    }
}
