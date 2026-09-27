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
use craie_layout::LayoutRow;
use taffy::Style;

use crate::animation::{Prop, Timing, Transition, Value};
use crate::geom::Size;
use crate::host::{NodeId, Parts, SpatialPatch};
use crate::keyframes::{Animation, Trigger};
use crate::mutation::NodeKind;
use crate::ui::Ui;

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
    /// Reported to assistive technology (`a11y.rs`).
    pub const A11Y: u64 = EXPANDED | SELECTED | CHECKED | DISABLED;
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

/// `Values` presence bits, in wire order. Transform parts go per axis,
/// as layout keys do: `_hover` setting `translateY` keeps the x that
/// applies.
pub mod value_field {
    pub const FILL: u16 = 1 << 0;
    pub const BORDER_COLOR: u16 = 1 << 1;
    pub const RADIUS: u16 = 1 << 2;
    /// The inherited color of text, inputs and `currentColor` drawings
    /// (set or cleared).
    pub const COLOR: u16 = 1 << 3;
    pub const OPACITY: u16 = 1 << 4;
    /// The free matrix (`Parts::matrix`).
    pub const TRANSFORM: u16 = 1 << 5;
    /// The layout keys in `Values::layout_keys`.
    pub const LAYOUT: u16 = 1 << 6;
    pub const BORDER_WIDTH: u16 = 1 << 7;
    /// Translate x: points and fraction together.
    pub const TRANSLATE_X: u16 = 1 << 8;
    pub const TRANSLATE_Y: u16 = 1 << 9;
    pub const ROTATE: u16 = 1 << 10;
    pub const SCALE_X: u16 = 1 << 11;
    pub const SCALE_Y: u16 = 1 << 12;
    /// Values only nodes with a box hold.
    pub const BOX: u16 = FILL | BORDER_COLOR | RADIUS | BORDER_WIDTH;
    /// Every transform part.
    pub const PARTS: u16 = TRANSFORM | TRANSLATE_X | TRANSLATE_Y | ROTATE | SCALE_X | SCALE_Y;
    pub const ALL: u16 = BOX | COLOR | OPACITY | LAYOUT | PARTS;
    /// Wire only (`op::VARIANTS`): the variant's transitions follow its
    /// values.
    pub const TRANSITIONS: u16 = 1 << 13;
    /// Wire only: then its animations.
    pub const ANIMATIONS: u16 = 1 << 14;
    pub const MOTION: u16 = TRANSITIONS | ANIMATIONS;
}

/// Layout keys: one per property, axis and side, so two variants that
/// set different parts of one wire field compose (`_narrow` sets
/// `padding.left`, `_compact` sets `padding.top`: both apply). Sides go
/// left, right, top, bottom.
pub mod layout_key {
    use crate::wire::field;

    pub const DISPLAY: u64 = 1 << 0;
    pub const POSITION: u64 = 1 << 1;
    pub const FLEX_DIRECTION: u64 = 1 << 2;
    pub const FLEX_WRAP: u64 = 1 << 3;
    pub const JUSTIFY_CONTENT: u64 = 1 << 4;
    pub const ALIGN_ITEMS: u64 = 1 << 5;
    pub const ALIGN_CONTENT: u64 = 1 << 6;
    pub const ALIGN_SELF: u64 = 1 << 7;
    /// Column gap (`gap.width`), then row gap.
    pub const GAP_WIDTH: u64 = 1 << 8;
    pub const GAP_HEIGHT: u64 = 1 << 9;
    pub const WIDTH: u64 = 1 << 10;
    pub const HEIGHT: u64 = 1 << 11;
    pub const MIN_WIDTH: u64 = 1 << 12;
    pub const MIN_HEIGHT: u64 = 1 << 13;
    pub const MAX_WIDTH: u64 = 1 << 14;
    pub const MAX_HEIGHT: u64 = 1 << 15;
    pub const PADDING_LEFT: u64 = 1 << 16;
    pub const PADDING_RIGHT: u64 = 1 << 17;
    pub const PADDING_TOP: u64 = 1 << 18;
    pub const PADDING_BOTTOM: u64 = 1 << 19;
    pub const MARGIN_LEFT: u64 = 1 << 20;
    pub const MARGIN_RIGHT: u64 = 1 << 21;
    pub const MARGIN_TOP: u64 = 1 << 22;
    pub const MARGIN_BOTTOM: u64 = 1 << 23;
    pub const BORDER_LEFT: u64 = 1 << 24;
    pub const BORDER_RIGHT: u64 = 1 << 25;
    pub const BORDER_TOP: u64 = 1 << 26;
    pub const BORDER_BOTTOM: u64 = 1 << 27;
    pub const INSET_LEFT: u64 = 1 << 28;
    pub const INSET_RIGHT: u64 = 1 << 29;
    pub const INSET_TOP: u64 = 1 << 30;
    pub const INSET_BOTTOM: u64 = 1 << 31;
    pub const FLEX_BASIS: u64 = 1 << 32;
    pub const FLEX_GROW: u64 = 1 << 33;
    pub const FLEX_SHRINK: u64 = 1 << 34;
    pub const ASPECT_RATIO: u64 = 1 << 35;
    pub const OVERFLOW_X: u64 = 1 << 36;
    pub const OVERFLOW_Y: u64 = 1 << 37;
    pub const ALL: u64 = (1 << 38) - 1;

    /// The wire style fields (`wire::field`) that carry `keys`.
    pub fn fields(keys: u64) -> u64 {
        let any = |k: u64, f: u64| if keys & k != 0 { f } else { 0 };
        // The first eight keys are their fields.
        (keys & 0xFF)
            | any(GAP_WIDTH | GAP_HEIGHT, field::GAP)
            | any(WIDTH | HEIGHT, field::SIZE)
            | any(MIN_WIDTH | MIN_HEIGHT, field::MIN_SIZE)
            | any(MAX_WIDTH | MAX_HEIGHT, field::MAX_SIZE)
            | any(0xF << 16, field::PADDING)
            | any(0xF << 20, field::MARGIN)
            | any(0xF << 24, field::BORDER)
            | any(0xF << 28, field::INSET)
            | any(FLEX_BASIS, field::FLEX_BASIS)
            | any(FLEX_GROW, field::FLEX_GROW)
            | any(FLEX_SHRINK, field::FLEX_SHRINK)
            | any(ASPECT_RATIO, field::ASPECT_RATIO)
            | any(OVERFLOW_X | OVERFLOW_Y, field::OVERFLOW)
    }
}

/// A set of style values: a base or a variant's. Fields outside `mask`
/// are ignored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Values {
    pub mask: u16,
    pub fill: u32,
    /// Border color and width.
    pub border: (u32, f32),
    pub radius: f32,
    /// `None`: no inherited color of its own.
    pub color: Option<u32>,
    pub opacity: f32,
    pub parts: Parts,
    /// The keys of `layout` that apply (`layout_key`).
    pub layout_keys: u64,
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
            parts: Parts::IDENTITY,
            layout_keys: 0,
            layout: crate::host::default_style(),
        }
    }
}

impl Values {
    /// In range: finite, opacity in [0, 1], known bits.
    pub fn valid(&self) -> bool {
        self.mask & !value_field::ALL == 0
            && self.layout_keys & !layout_key::ALL == 0
            && self.border.1.is_finite()
            && self.radius.is_finite()
            && (0.0..=1.0).contains(&self.opacity)
            && self.parts.is_finite()
    }

    /// Overlays `v`'s present values, property by property. Layout goes
    /// through `layout` (made from this base on first use).
    fn overlay(&mut self, v: &Variant, layout: &mut Option<Style>) {
        use value_field::*;
        let (m, x) = (v.values.mask, &v.values);
        if m & FILL != 0 {
            self.fill = x.fill;
        }
        if m & BORDER_COLOR != 0 {
            self.border.0 = x.border.0;
        }
        if m & BORDER_WIDTH != 0 {
            self.border.1 = x.border.1;
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
        let (p, xp) = (&mut self.parts, &x.parts);
        if m & TRANSFORM != 0 {
            p.matrix = xp.matrix;
        }
        if m & TRANSLATE_X != 0 {
            [p.translate[0], p.translate[2]] = [xp.translate[0], xp.translate[2]];
        }
        if m & TRANSLATE_Y != 0 {
            [p.translate[1], p.translate[3]] = [xp.translate[1], xp.translate[3]];
        }
        if m & ROTATE != 0 {
            p.rotate = xp.rotate;
        }
        if m & SCALE_X != 0 {
            p.scale[0] = xp.scale[0];
        }
        if m & SCALE_Y != 0 {
            p.scale[1] = xp.scale[1];
        }
        if let Some(src) = &v.layout {
            let dst = layout.get_or_insert_with(|| self.layout.to_taffy());
            copy_keys(dst, src, x.layout_keys);
        }
    }
}

/// Copies the layout keys in `keys` from `src` to `dst`.
fn copy_keys(dst: &mut Style, src: &Style, keys: u64) {
    macro_rules! copy {
        ($($key:ident => $($f:ident).+;)*) => {
            $(if keys & layout_key::$key != 0 { dst$(.$f)+ = src$(.$f)+.clone(); })*
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
        GAP_WIDTH => gap.width;
        GAP_HEIGHT => gap.height;
        WIDTH => size.width;
        HEIGHT => size.height;
        MIN_WIDTH => min_size.width;
        MIN_HEIGHT => min_size.height;
        MAX_WIDTH => max_size.width;
        MAX_HEIGHT => max_size.height;
        PADDING_LEFT => padding.left;
        PADDING_RIGHT => padding.right;
        PADDING_TOP => padding.top;
        PADDING_BOTTOM => padding.bottom;
        MARGIN_LEFT => margin.left;
        MARGIN_RIGHT => margin.right;
        MARGIN_TOP => margin.top;
        MARGIN_BOTTOM => margin.bottom;
        BORDER_LEFT => border.left;
        BORDER_RIGHT => border.right;
        BORDER_TOP => border.top;
        BORDER_BOTTOM => border.bottom;
        INSET_LEFT => inset.left;
        INSET_RIGHT => inset.right;
        INSET_TOP => inset.top;
        INSET_BOTTOM => inset.bottom;
        FLEX_BASIS => flex_basis;
        FLEX_GROW => flex_grow;
        FLEX_SHRINK => flex_shrink;
        ASPECT_RATIO => aspect_ratio;
        OVERFLOW_X => overflow.x;
        OVERFLOW_Y => overflow.y;
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
/// environment has every bit of `env`. Its transitions time the changes
/// into it (the style being entered, as CSS); its animations run while
/// it holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VariantDecl {
    pub terms: Vec<TermDecl>,
    pub env: u8,
    pub values: Values,
    pub transitions: Vec<Transition>,
    pub animations: Vec<Animation>,
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
    /// Its index in the declared list: its animations' identity.
    decl: usize,
    transitions: Vec<Transition>,
    animations: Vec<Animation>,
}

/// A node's variant table.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    /// What the node's own ops declare. Always full: every property the
    /// node has.
    pub base: Values,
    /// The last resolution declared: restyle declares only what differs.
    pub(crate) resolved: Values,
    /// Ascending specificity: (depth, latest rank, declaration order).
    variants: Vec<Variant>,
    /// Some variant reads the environment.
    uses_env: bool,
    /// Some variant reads a hover bit.
    uses_hover: bool,
    /// Not resolved yet: its first values go to the rows directly, with
    /// no transition (nothing was on screen to move from).
    fresh: bool,
    /// Some variant has transitions.
    transitioned: bool,
    /// Some variant has animations.
    animated: bool,
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

    /// The bits variants see: disabled stops hover, pressed and focus
    /// visible; touch stops hover.
    fn effective(&self, env: u8) -> u64 {
        let mut b = self.app | self.input;
        if b & state_bit::DISABLED != 0 {
            b &= !(state_bit::HOVER | state_bit::PRESSED | state_bit::FOCUS_VISIBLE);
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
    /// What the input bits were computed from: they hold while it does.
    input_key: Option<InputKey>,
    /// Tables that read a hover bit: without them (and hover
    /// listeners), hover at rest need not hit-test.
    pub(crate) hover_tables: usize,
    /// Declarations go to the rows directly, not through transitions.
    pub(crate) snapping: bool,
    /// The environment has been set from a window size.
    sized: bool,
    /// The next restyle snaps every table (the first size's).
    snap_next: bool,
    pub(crate) env: u8,
    narrow_max: f32,
    compact_max: f32,
    /// Keyboard modality: the last key or pointer press was a key.
    pub(crate) keyboard: bool,
}

/// Hover, the primary press, focus, keyboard modality, and the tree's
/// shape: the input bits are a function of these.
#[derive(Clone, Copy, PartialEq)]
struct InputKey {
    hover: Option<NodeId>,
    pressed: Option<NodeId>,
    focus: Option<NodeId>,
    keyboard: bool,
    structure: craie_core::rev::Rev,
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
            input_key: None,
            hover_tables: 0,
            snapping: false,
            sized: false,
            snap_next: false,
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

    /// The transition for `prop` of the most specific active variant of
    /// `id`'s table that has one.
    pub(crate) fn variant_transition(&self, id: u32, prop: Prop) -> Option<Timing> {
        let t = self.tables.get(&id).filter(|t| t.transitioned)?;
        t.variants
            .iter()
            .rev()
            .filter(|v| !v.transitions.is_empty() && self.active(v))
            .find_map(|v| v.transitions.iter().find(|t| t.prop == prop))
            .map(|t| t.timing)
    }

    /// The animations of `id`'s active variants, in composite order:
    /// (key, rank, animation).
    pub(crate) fn variant_animations(&self, id: u32) -> Vec<(u32, u32, Animation)> {
        let Some(t) = self.tables.get(&id).filter(|t| t.animated) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (pos, v) in t.variants.iter().enumerate() {
            if v.animations.is_empty() || !self.active(v) {
                continue;
            }
            for (i, a) in v.animations.iter().enumerate() {
                let key = crate::keyframes::key(Trigger::Variant, v.decl, i);
                let rank = crate::keyframes::rank(Trigger::Variant, pos, i);
                out.push((key, rank, a.clone()));
            }
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

    fn insert_table(&mut self, id: u32, t: Table) {
        self.hover_tables += t.uses_hover as usize;
        self.tables.insert(id, t);
    }

    fn remove_table(&mut self, id: u32) -> Option<Table> {
        let t = self.tables.remove(&id)?;
        self.hover_tables -= t.uses_hover as usize;
        Some(t)
    }

    /// A scope for `id`'s occupant of `generation`, new or reset. Input
    /// bits are recomputed at the next restyle: the new scope may hold
    /// some.
    fn scope(&mut self, id: u32, generation: u16) -> &mut Scope {
        let s = self.scopes.entry(id).or_insert_with(|| {
            self.input_key = None;
            Scope::new(generation)
        });
        if s.generation != generation {
            *s = Scope::new(generation);
            self.input_key = None;
        }
        s
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

/// Specificity: (depth, latest rank). Depth counts the bits a variant
/// needs; its latest rank is the highest of them, environment bits
/// ranking above state bits. So `_focusVisible` (bit 56) beats `_hover`
/// (54), and `_narrow._hover` against `_selected._hover` compares only
/// narrow against selected. Declaration order breaks the remaining ties.
fn specificity(d: &VariantDecl) -> (u32, u32) {
    let depth = d.terms.iter().map(|t| t.mask.count_ones()).sum::<u32>() + d.env.count_ones();
    let ranks = d.terms.iter().fold(0u128, |r, t| r | t.mask as u128) | (d.env as u128) << 64;
    (depth, 128 - ranks.leading_zeros())
}

impl Ui {
    /// Resolves the queued tables and declares what changed. Runs at the
    /// end of `execute` and `dispatch` and at the top of `render`.
    pub(crate) fn restyle(&mut self) {
        self.refresh_input_bits();
        let snap = std::mem::take(&mut self.states.snap_next);
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
            let fresh = t.fresh || snap;
            if let Some(t) = self.states.tables.get_mut(&id) {
                t.resolved = next;
                t.fresh = false;
            }
            if next != prev {
                self.states.snapping = fresh;
                self.declare_values(NodeId(id), &prev, &next);
                self.states.snapping = false;
            }
            self.sync_variant_animations(id);
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
        let changed = |bit: u16, same: bool| m & bit != 0 && !same;
        let (p, n) = (&prev.parts, &next.parts);
        let patch = SpatialPatch {
            translate: changed(TRANSLATE_X | TRANSLATE_Y, p.translate == n.translate)
                .then_some(n.translate),
            rotate: changed(ROTATE, p.rotate == n.rotate).then_some(n.rotate),
            scale: changed(SCALE_X | SCALE_Y, p.scale == n.scale).then_some(n.scale),
            matrix: changed(TRANSFORM, p.matrix == n.matrix).then_some(n.matrix),
            opacity: changed(OPACITY, prev.opacity == next.opacity).then_some(next.opacity),
        };
        if !patch.is_empty() {
            self.declare_spatial(node, patch);
        }
        let fill = changed(FILL, prev.fill == next.fill);
        let radius = changed(RADIUS, prev.radius == next.radius);
        let border = changed(BORDER_COLOR, prev.border.0 == next.border.0)
            || changed(BORDER_WIDTH, prev.border.1 == next.border.1);
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
    /// focus: O(depth) over their ancestor chains, only when one of
    /// them, the modality or the tree's shape changed. Queues the
    /// dependents of scopes whose bits changed.
    fn refresh_input_bits(&mut self) {
        if self.states.scopes.is_empty() && self.states.held.is_empty() {
            return;
        }
        let key = InputKey {
            hover: self.hover,
            pressed: self.pressed_node(),
            focus: self.focus,
            keyboard: self.states.keyboard,
            structure: self.host.revs.structure,
        };
        if self.states.input_key == Some(key) {
            return;
        }
        self.states.input_key = Some(key);
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
        add(self, self.pressed_node(), 0, state_bit::PRESSED);
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

    /// What the `pressed` bit follows: the node under a held primary
    /// button, else the pressable a held Space presses.
    fn pressed_node(&self) -> Option<NodeId> {
        self.pressed
            .filter(|_| self.pressed_primary)
            .or(self.key_press)
    }

    /// Sets scope `id`'s app bits (`STATES`); it becomes a scope.
    pub(crate) fn set_app_bits(&mut self, id: u32, bits: u64) {
        let generation = self.host.node(NodeId(id)).map_or(0, |n| n.generation);
        let st = &mut self.states;
        let s = st.scope(id, generation);
        let changed = s.app ^ bits;
        if changed == 0 {
            return;
        }
        s.app = bits;
        st.queue_dependents(id);
        // Assistive technology reports it.
        if changed & state_bit::A11Y != 0 {
            self.host.revs.semantic.bump();
            self.host.dirty.semantic.push(id);
        }
    }

    /// Replaces the node's variant table; an empty list removes it and
    /// declares the base.
    pub(crate) fn set_variants(&mut self, id: u32, decls: &[VariantDecl]) {
        let old = self.states.remove_table(id);
        if let Some(t) = &old {
            self.states.unlink(id, t);
        }
        if decls.is_empty() {
            if let Some(t) = old {
                self.declare_values(NodeId(id), &t.resolved, &t.base);
            }
            self.sync_variant_animations(id);
            return;
        }
        let (base, resolved, fresh) = match old {
            Some(t) => (t.base, t.resolved, t.fresh),
            None => {
                let b = self.capture_base(NodeId(id));
                (b, b, true)
            }
        };
        let mut keyed: Vec<((u32, u32), Variant)> = decls
            .iter()
            .enumerate()
            .map(|(decl, d)| {
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
                    decl,
                    transitions: d.transitions.clone(),
                    animations: d.animations.clone(),
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
            st.scope(t.scope, t.generation).dependents.push(id);
        }
        let uses_env = variants.iter().any(|v| v.env != 0);
        let transitioned = variants.iter().any(|v| !v.transitions.is_empty());
        let animated = variants.iter().any(|v| !v.animations.is_empty());
        let uses_hover = variants
            .iter()
            .flat_map(|v| &v.terms)
            .any(|t| t.mask & state_bit::HOVER != 0);
        // Hover at rest was not tracked without a reader: refresh it.
        self.hover_stale |= uses_hover && st.hover_tables == 0 && self.host.hover_listeners == 0;
        st.insert_table(
            id,
            Table {
                base,
                resolved,
                variants,
                uses_env,
                uses_hover,
                fresh,
                transitioned,
                animated,
            },
        );
        st.queue.push(id);
    }

    /// The node's values as its own ops declared them: the rows, except
    /// that what lies under a keyframe animation replaces its sample and
    /// a running tween's target the value in flight.
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
            mask: PARTS | OPACITY | COLOR | LAYOUT,
            layout_keys: layout_key::ALL,
            parts: s.parts,
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
            if self.motion.under(node, prop).is_some() {
                match prop {
                    Prop::Color => v.color = self.inherited_color(node),
                    p => set_base(&mut v, p, self.row_value(node, p)),
                }
            }
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
        if let Some(t) = st.remove_table(node.0) {
            st.unlink(node.0, &t);
        }
    }

    /// Sets the environment from the window's size (logical) before the
    /// first frame, so the first styles are the window's: the platform
    /// calls it at creation. Without it, the first frame does, and
    /// every table resolved before snaps to it with no transition.
    pub fn set_window_size(&mut self, size: Size) {
        self.update_env(size);
    }

    /// Updates the width breakpoints from the frame size (logical).
    pub(crate) fn update_env(&mut self, size: Size) {
        // The first size: what it restyles was never on screen.
        self.states.snap_next |= !self.states.sized;
        self.states.sized = true;
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
        // JS applies each animation's reduced-motion policy.
        if changed & env_bit::REDUCED_MOTION != 0 {
            let mut e = self.event(crate::events::out_kind::ENVIRONMENT, NodeId::NIL);
            e.key = env as u32;
            self.pending_events.push(e);
        }
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
        (Prop::Transform, Value::Transform(t)) => v.parts.matrix = t,
        (Prop::Translate, Value::Translate(t)) => v.parts.translate = t,
        (Prop::Rotate, Value::Rotate(r)) => v.parts.rotate = r,
        (Prop::Scale, Value::Scale(s)) => v.parts.scale = s,
        (Prop::Opacity, Value::Opacity(o)) => v.opacity = o,
        (Prop::Fill, Value::Color(c)) => v.fill = c,
        (Prop::BorderColor, Value::Color(c)) => v.border.0 = c,
        (Prop::Color, Value::Color(c)) => v.color = Some(c),
        (p, value) if p.is_layout() => crate::animation::set_row_field(&mut v.layout, p, value),
        _ => {}
    }
}
