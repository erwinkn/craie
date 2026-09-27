//! E05: the owned flex engine (`craie_layout`) against Taffy 0.14.
//!
//! A `FlexCase` is a tree of original `taffy::Style`s with synthetic
//! leaf measures. `FlexCase::compare` lays it out twice: through the
//! owned engine over `LayoutRow`s, and through a `TaffyTree` of the
//! original styles, with the same measure functions. Every node's
//! unrounded `Layout` must be bit-equal.

use craie_core::rng::Rng;
use craie_layout::{
    FlexScratch, LayoutRow, LayoutTree, Node, compute_cached, compute_flex, compute_hidden,
    compute_leaf, compute_root,
};
use taffy::{
    AlignContent, AlignItems, AlignmentSafety, AvailableSpace, BoxSizing, Cache, ClearState,
    Contain, Dimension, Direction, Display, FlexDirection, FlexWrap, Layout, LayoutInput,
    LayoutOutput, LayoutPartialTree, LengthPercentage, LengthPercentageAuto, Line, Overflow, Point,
    Position, Rect, RequestedAxis, RunMode, Size, SizingMode, Style, TaffyTree,
};

/// A leaf's content: the same function on both sides.
#[derive(Clone, Copy, Debug)]
pub enum Measure {
    /// No content.
    Empty,
    /// A fixed content box.
    Fixed(f32, f32),
    /// Text-like: `words` words of `word` px that wrap at the available
    /// width, `line` px per line.
    Text { words: u32, word: f32, line: f32 },
    /// A width-to-height ratio; 40 px wide when nothing constrains it.
    Aspect(f32),
    /// Both axes follow the available space, with a different size for
    /// each kind (definite, min-content, max-content), so a wrong height
    /// constraint changes the result as a wrong width does.
    Space(f32),
}

impl Measure {
    pub fn size(self, known: Size<Option<f32>>, available: Size<AvailableSpace>) -> Size<f32> {
        if let (Some(width), Some(height)) = (known.width, known.height) {
            return Size { width, height };
        }
        match self {
            Measure::Empty => Size {
                width: known.width.unwrap_or(0.0),
                height: known.height.unwrap_or(0.0),
            },
            Measure::Fixed(w, h) => Size {
                width: known.width.unwrap_or(w),
                height: known.height.unwrap_or(h),
            },
            Measure::Text { words, word, line } => {
                let max = words as f32 * word;
                let width = known.width.unwrap_or(match available.width {
                    AvailableSpace::MinContent => word.min(max),
                    AvailableSpace::MaxContent => max,
                    AvailableSpace::Definite(w) => w.max(word.min(max)).min(max),
                });
                let per_line = ((width / word).floor() as u32).max(1);
                let lines = words.div_ceil(per_line);
                Size {
                    width,
                    height: known.height.unwrap_or(lines as f32 * line),
                }
            }
            Measure::Aspect(ratio) => match (known.width, known.height) {
                (Some(width), None) => Size {
                    width,
                    height: width / ratio,
                },
                (None, Some(height)) => Size {
                    width: height * ratio,
                    height,
                },
                _ => {
                    let width = match available.width {
                        AvailableSpace::Definite(w) => w.clamp(0.0, 40.0),
                        _ => 40.0,
                    };
                    Size {
                        width,
                        height: width / ratio,
                    }
                }
            },
            Measure::Space(base) => Size {
                width: known.width.unwrap_or(match available.width {
                    AvailableSpace::Definite(w) => w.max(0.0).min(base * 3.0),
                    AvailableSpace::MinContent => base * 0.5,
                    AvailableSpace::MaxContent => base * 2.0,
                }),
                height: known.height.unwrap_or(match available.height {
                    AvailableSpace::Definite(h) => (h.max(0.0) * 0.5).min(base * 3.0) + 1.0,
                    AvailableSpace::MinContent => base * 0.25,
                    AvailableSpace::MaxContent => base * 1.5,
                }),
            },
        }
    }
}

/// A change between two layouts of a `FlexCase`.
#[derive(Clone, Debug)]
pub enum Edit {
    /// A node's style and content.
    Node(usize, Box<Style>, Measure),
    /// Moves `node` (not the root) under `parent` at `index`.
    Move {
        node: usize,
        parent: usize,
        index: usize,
    },
    /// A new available space for the root.
    Viewport(Size<AvailableSpace>),
}

/// A tree of original styles. Node 0 is the root; `parents[i]` is the
/// parent of node `i` (`parents[0]` is unused) and comes before it.
#[derive(Clone, Debug)]
pub struct FlexCase {
    pub styles: Vec<Style>,
    pub parents: Vec<u32>,
    pub measures: Vec<Measure>,
    pub available: Size<AvailableSpace>,
}

/// The owned engine's host for a `FlexCase`.
pub struct OwnedTree {
    pub rows: Vec<LayoutRow>,
    pub children: Vec<Vec<Node>>,
    pub measures: Vec<Measure>,
    pub layouts: Vec<Layout>,
    pub caches: Vec<Cache>,
    pub parents: Vec<u32>,
    scratch: FlexScratch,
}

impl OwnedTree {
    pub fn new(case: &FlexCase) -> OwnedTree {
        let n = case.styles.len();
        let mut children = vec![Vec::new(); n];
        for (i, parent) in case.parents.iter().enumerate().skip(1) {
            children[*parent as usize].push(i as Node);
        }
        OwnedTree {
            rows: case.styles.iter().map(LayoutRow::from).collect(),
            children,
            measures: case.measures.clone(),
            layouts: vec![Layout::new(); n],
            caches: vec![Cache::new(); n],
            parents: case.parents.clone(),
            scratch: FlexScratch::default(),
        }
    }

    pub fn layout(&mut self, available: Size<AvailableSpace>) {
        compute_root(self, 0, available);
    }

    /// Clears the caches of `node` and its ancestors as `TaffyTree` does
    /// when a node changes: the walk stops at a cache already empty.
    pub fn mark_dirty(&mut self, node: Node) {
        let mut cur = node as usize;
        while let ClearState::Cleared = self.caches[cur].clear() {
            if cur == 0 {
                break;
            }
            cur = self.parents[cur] as usize;
        }
    }
}

impl LayoutTree for OwnedTree {
    fn row(&self, node: Node) -> &LayoutRow {
        &self.rows[node as usize]
    }

    fn child_count(&self, node: Node) -> usize {
        self.children[node as usize].len()
    }

    fn child(&self, node: Node, index: usize) -> Node {
        self.children[node as usize][index]
    }

    fn compute_child(&mut self, node: Node, inputs: LayoutInput) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden(self, node);
        }
        compute_cached(self, node, inputs, |tree, node, inputs| {
            let row = tree.rows[node as usize];
            if row.display() == Display::None {
                compute_hidden(tree, node)
            } else if tree.child_count(node) > 0 {
                compute_flex(tree, node, inputs)
            } else {
                let measure = tree.measures[node as usize];
                compute_leaf(inputs, &row, |known, available| {
                    measure.size(known, available)
                })
            }
        })
    }

    fn set_layout(&mut self, node: Node, layout: &Layout) {
        self.layouts[node as usize] = *layout;
    }

    fn cache_get(&mut self, node: Node, inputs: &LayoutInput) -> Option<LayoutOutput> {
        self.caches[node as usize].get(inputs)
    }

    fn cache_store(&mut self, node: Node, inputs: &LayoutInput, output: LayoutOutput) {
        self.caches[node as usize].store(inputs, output)
    }

    fn cache_clear(&mut self, node: Node) {
        self.caches[node as usize].clear();
    }

    fn scratch(&mut self) -> &mut FlexScratch {
        &mut self.scratch
    }
}

/// The reference: a `TaffyTree` of the original styles.
pub struct TaffyCase {
    pub tree: TaffyTree<Measure>,
    pub nodes: Vec<taffy::NodeId>,
}

impl TaffyCase {
    pub fn new(case: &FlexCase) -> TaffyCase {
        let mut tree: TaffyTree<Measure> = TaffyTree::new();
        tree.disable_rounding();
        let nodes: Vec<taffy::NodeId> = case
            .styles
            .iter()
            .zip(&case.measures)
            .map(|(style, measure)| tree.new_leaf_with_context(style.clone(), *measure).unwrap())
            .collect();
        for (i, parent) in case.parents.iter().enumerate().skip(1) {
            tree.add_child(nodes[*parent as usize], nodes[i]).unwrap();
        }
        TaffyCase { tree, nodes }
    }

    pub fn layout(&mut self, available: Size<AvailableSpace>) {
        self.tree
            .compute_layout_with_measure(self.nodes[0], available, |inputs, _, measure, style| {
                let measure = measure.copied().unwrap_or(Measure::Empty);
                taffy::compute_leaf_layout(
                    inputs,
                    style,
                    |_, _| 0.0,
                    |known, available| measure.size(known, available),
                )
            })
            .unwrap();
    }
}

impl FlexCase {
    /// Lays the case out on both sides; then, for each of `edits`,
    /// applies it to both (the owned host clears caches as `TaffyTree`
    /// does), and lays out again. Both trees persist across the edits,
    /// so cached results meet changed styles, content, structure, and
    /// viewports. Returns the first difference.
    pub fn compare(&self, edits: &[Edit]) -> Result<(), String> {
        let mut owned = OwnedTree::new(self);
        let mut reference = TaffyCase::new(self);
        let mut available = self.available;
        owned.layout(available);
        reference.layout(available);
        self.diff(&owned, &reference, "cold")?;
        for (k, edit) in edits.iter().enumerate() {
            match edit {
                Edit::Node(node, style, measure) => {
                    owned.rows[*node] = LayoutRow::from(&**style);
                    owned.measures[*node] = *measure;
                    owned.mark_dirty(*node as Node);
                    let id = reference.nodes[*node];
                    reference.tree.set_style(id, (**style).clone()).unwrap();
                    reference.tree.set_node_context(id, Some(*measure)).unwrap();
                }
                Edit::Move {
                    node,
                    parent,
                    index,
                } => {
                    let old = owned.parents[*node] as usize;
                    owned.children[old].retain(|c| *c as usize != *node);
                    owned.mark_dirty(old as Node);
                    let index = (*index).min(owned.children[*parent].len());
                    owned.children[*parent].insert(index, *node as Node);
                    owned.parents[*node] = *parent as u32;
                    owned.mark_dirty(*parent as Node);
                    let tree = &mut reference.tree;
                    let ids = &reference.nodes;
                    tree.remove_child(ids[old], ids[*node]).unwrap();
                    tree.insert_child_at_index(ids[*parent], index, ids[*node])
                        .unwrap();
                }
                Edit::Viewport(space) => available = *space,
            }
            owned.layout(available);
            reference.layout(available);
            self.diff(&owned, &reference, &format!("edit {k} ({edit:?})"))?;
        }
        Ok(())
    }

    /// Calls `compute_child` directly with generated inputs (every run
    /// mode, sizing mode, requested axis, definiteness, parent size, and
    /// available space), on the owned engine and on Taffy's algorithm
    /// over the same host, each call twice (the second from the cache).
    /// Compares every `LayoutOutput` field and every node's `Layout`.
    pub fn compare_outputs(&self, rng: &mut Rng, calls: usize) -> Result<(), String> {
        let mut owned = OwnedTree::new(self);
        let mut host = TaffyHost(OwnedTree::new(self));
        owned.layout(self.available);
        host.layout(self.available);
        for k in 0..calls {
            let node = rng.below(self.styles.len() as u32);
            let inputs = gen_input(rng);
            for pass in ["first", "cached"] {
                let got = owned.compute_child(node, inputs);
                let want = host.compute_child_layout(taffy::NodeId::new(node as u64), inputs);
                if let Some(field) = output_diff(&got, &want) {
                    return Err(format!(
                        "call {k} ({pass}) node {node} {field}:\n  inputs {inputs:?}\n  owned  {got:?}\n  taffy  {want:?}"
                    ));
                }
                for (i, (a, b)) in owned.layouts.iter().zip(&host.0.layouts).enumerate() {
                    if let Some(field) = layout_diff(a, b) {
                        return Err(format!(
                            "call {k} ({pass}) node {node}, then node {i} {field}:\n  owned  {a:?}\n  taffy  {b:?}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn diff(&self, owned: &OwnedTree, reference: &TaffyCase, when: &str) -> Result<(), String> {
        for (i, node) in reference.nodes.iter().enumerate() {
            let want = reference.tree.layout(*node).unwrap();
            let got = &owned.layouts[i];
            if let Some(field) = layout_diff(got, want) {
                return Err(format!(
                    "{when}: node {i} {field}:\n  owned  {got:?}\n  taffy  {want:?}"
                ));
            }
        }
        Ok(())
    }
}

/// The first `LayoutOutput` field whose bits differ.
pub fn output_diff(a: &LayoutOutput, b: &LayoutOutput) -> Option<&'static str> {
    let bits = |x: Option<f32>| x.map(f32::to_bits);
    let rect = |a: Rect<f32>, b: Rect<f32>| {
        [a.left, a.right, a.top, a.bottom]
            .iter()
            .zip([b.left, b.right, b.top, b.bottom])
            .all(|(x, y)| x.to_bits() == y.to_bits())
    };
    if a.size.width.to_bits() != b.size.width.to_bits()
        || a.size.height.to_bits() != b.size.height.to_bits()
    {
        return Some("size");
    }
    if !rect(a.scrollable_overflow_rect, b.scrollable_overflow_rect) {
        return Some("scrollable overflow");
    }
    if bits(a.baselines.first) != bits(b.baselines.first)
        || bits(a.baselines.last) != bits(b.baselines.last)
    {
        return Some("baselines");
    }
    if a.top_margin != b.top_margin || a.bottom_margin != b.bottom_margin {
        return Some("collapsible margins");
    }
    if a.margins_can_collapse_through != b.margins_can_collapse_through {
        return Some("margins collapse through");
    }
    None
}

/// The first field whose bits differ.
pub fn layout_diff(a: &Layout, b: &Layout) -> Option<&'static str> {
    let rect = |a: Rect<f32>, b: Rect<f32>| {
        [a.left, a.right, a.top, a.bottom]
            .iter()
            .zip([b.left, b.right, b.top, b.bottom])
            .all(|(x, y)| x.to_bits() == y.to_bits())
    };
    let size = |a: Size<f32>, b: Size<f32>| {
        a.width.to_bits() == b.width.to_bits() && a.height.to_bits() == b.height.to_bits()
    };
    if a.order != b.order {
        return Some("order");
    }
    if a.location.x.to_bits() != b.location.x.to_bits()
        || a.location.y.to_bits() != b.location.y.to_bits()
    {
        return Some("location");
    }
    if !size(a.size, b.size) {
        return Some("size");
    }
    if !rect(a.scrollable_overflow_rect, b.scrollable_overflow_rect) {
        return Some("scrollable overflow");
    }
    if !size(a.scrollbar_size, b.scrollbar_size) {
        return Some("scrollbar size");
    }
    if !rect(a.border, b.border) {
        return Some("border");
    }
    if !rect(a.padding, b.padding) {
        return Some("padding");
    }
    if !rect(a.margin, b.margin) {
        return Some("margin");
    }
    None
}

// The generator.

fn pick<T: Clone>(rng: &mut Rng, values: &[T]) -> T {
    values[rng.below(values.len() as u32) as usize].clone()
}

fn gen_length(rng: &mut Rng) -> f32 {
    match rng.below(10) {
        0 => 0.0,
        1 => -(rng.unit() * 30.0),
        2 => rng.unit() * 3.0,
        3 => pick(rng, &[1e-4, 0.5, 1e5, 3.25e6]),
        _ => (rng.unit() * 160.0).round() / pick(rng, &[1.0, 1.0, 2.0, 3.0]),
    }
}

fn gen_percent(rng: &mut Rng) -> f32 {
    pick(rng, &[0.0, 0.1, 0.25, 1.0 / 3.0, 0.5, 1.0, 1.5, -0.2])
}

fn gen_lp(rng: &mut Rng) -> LengthPercentage {
    match rng.below(5) {
        0 => LengthPercentage::percent(gen_percent(rng).max(0.0)),
        1 | 2 => LengthPercentage::length(0.0),
        _ => LengthPercentage::length(gen_length(rng).max(0.0)),
    }
}

fn gen_lpa(rng: &mut Rng) -> LengthPercentageAuto {
    match rng.below(6) {
        0 => LengthPercentageAuto::percent(gen_percent(rng)),
        1 | 2 => LengthPercentageAuto::length(gen_length(rng)),
        _ => LengthPercentageAuto::auto(),
    }
}

fn gen_margin(rng: &mut Rng) -> LengthPercentageAuto {
    match rng.below(8) {
        0 => LengthPercentageAuto::auto(),
        1 => LengthPercentageAuto::percent(gen_percent(rng)),
        2 | 3 => LengthPercentageAuto::length(gen_length(rng)),
        _ => LengthPercentageAuto::length(0.0),
    }
}

fn gen_dim(rng: &mut Rng) -> Dimension {
    match rng.below(16) {
        0 | 1 => Dimension::percent(gen_percent(rng)),
        2..=5 => Dimension::length(gen_length(rng).max(0.0)),
        6 => Dimension::min_content(),
        7 => Dimension::max_content(),
        8 => Dimension::fit_content(),
        9 => Dimension::fit_content_px(gen_length(rng).max(0.0)),
        10 => Dimension::fit_content_percent(gen_percent(rng).max(0.0)),
        11 => Dimension::stretch(),
        _ => Dimension::auto(),
    }
}

fn gen_basis(rng: &mut Rng) -> Dimension {
    match rng.below(10) {
        0 => Dimension::content(),
        1..=3 => gen_dim(rng),
        _ => Dimension::auto(),
    }
}

fn gen_safety(rng: &mut Rng) -> AlignmentSafety {
    pick(
        rng,
        &[
            AlignmentSafety::Unsafe,
            AlignmentSafety::Unsafe,
            AlignmentSafety::Safe,
        ],
    )
}

fn gen_items(rng: &mut Rng) -> Option<AlignItems> {
    let value = pick(
        rng,
        &[
            None,
            None,
            Some(AlignItems::START),
            Some(AlignItems::END),
            Some(AlignItems::FLEX_START),
            Some(AlignItems::FLEX_END),
            Some(AlignItems::SELF_START),
            Some(AlignItems::SELF_END),
            Some(AlignItems::CENTER),
            Some(AlignItems::BASELINE),
            Some(AlignItems::BASELINE),
            Some(AlignItems::STRETCH),
        ],
    );
    value.map(|mut value| {
        value.safety = gen_safety(rng);
        value
    })
}

fn gen_content(rng: &mut Rng) -> Option<AlignContent> {
    let value = pick(
        rng,
        &[
            None,
            None,
            Some(AlignContent::START),
            Some(AlignContent::END),
            Some(AlignContent::FLEX_START),
            Some(AlignContent::FLEX_END),
            Some(AlignContent::CENTER),
            Some(AlignContent::STRETCH),
            Some(AlignContent::SPACE_BETWEEN),
            Some(AlignContent::SPACE_EVENLY),
            Some(AlignContent::SPACE_AROUND),
        ],
    );
    value.map(|mut value| {
        value.safety = gen_safety(rng);
        value
    })
}

/// A style with every field `LayoutRow` holds.
pub fn gen_style(rng: &mut Rng) -> Style {
    let rect = |rng: &mut Rng, f: fn(&mut Rng) -> LengthPercentageAuto| Rect {
        left: f(rng),
        right: f(rng),
        top: f(rng),
        bottom: f(rng),
    };
    let lp_rect = |rng: &mut Rng| {
        if rng.chance(0.5) {
            Rect {
                left: LengthPercentage::length(0.0),
                right: LengthPercentage::length(0.0),
                top: LengthPercentage::length(0.0),
                bottom: LengthPercentage::length(0.0),
            }
        } else {
            Rect {
                left: gen_lp(rng),
                right: gen_lp(rng),
                top: gen_lp(rng),
                bottom: gen_lp(rng),
            }
        }
    };
    let inset = if rng.chance(0.6) {
        Rect {
            left: LengthPercentageAuto::auto(),
            right: LengthPercentageAuto::auto(),
            top: LengthPercentageAuto::auto(),
            bottom: LengthPercentageAuto::auto(),
        }
    } else {
        rect(rng, gen_lpa)
    };
    let margin = if rng.chance(0.4) {
        Rect {
            left: LengthPercentageAuto::length(0.0),
            right: LengthPercentageAuto::length(0.0),
            top: LengthPercentageAuto::length(0.0),
            bottom: LengthPercentageAuto::length(0.0),
        }
    } else {
        rect(rng, gen_margin)
    };
    let limits = |rng: &mut Rng| {
        if rng.chance(0.6) {
            LengthPercentageAuto::auto()
        } else {
            gen_lpa(rng)
        }
    };
    Style {
        display: pick(
            rng,
            &[
                Display::Flex,
                Display::Flex,
                Display::Flex,
                Display::Flex,
                Display::None,
            ],
        ),
        position: pick(
            rng,
            &[
                Position::Relative,
                Position::Relative,
                Position::Relative,
                Position::Absolute,
            ],
        ),
        direction: pick(rng, &[Direction::Ltr, Direction::Ltr, Direction::Rtl]),
        box_sizing: pick(
            rng,
            &[
                BoxSizing::BorderBox,
                BoxSizing::BorderBox,
                BoxSizing::ContentBox,
            ],
        ),
        overflow: if rng.chance(0.6) {
            Point {
                x: Overflow::Visible,
                y: Overflow::Visible,
            }
        } else {
            Point {
                x: pick(
                    rng,
                    &[
                        Overflow::Visible,
                        Overflow::Clip,
                        Overflow::Hidden,
                        Overflow::Scroll,
                    ],
                ),
                y: pick(
                    rng,
                    &[
                        Overflow::Visible,
                        Overflow::Clip,
                        Overflow::Hidden,
                        Overflow::Scroll,
                    ],
                ),
            }
        },
        scrollbar_width: pick(rng, &[0.0, 0.0, 0.0, 6.0, 15.0]),
        contain: pick(
            rng,
            &[
                Contain::NONE,
                Contain::NONE,
                Contain::NONE,
                Contain::LAYOUT,
                Contain::PAINT,
                Contain::CONTENT,
            ],
        ),
        flex_direction: pick(
            rng,
            &[
                FlexDirection::Row,
                FlexDirection::Column,
                FlexDirection::RowReverse,
                FlexDirection::ColumnReverse,
            ],
        ),
        flex_wrap: pick(
            rng,
            &[
                FlexWrap::NoWrap,
                FlexWrap::NoWrap,
                FlexWrap::Wrap,
                FlexWrap::WrapReverse,
            ],
        ),
        justify_content: gen_content(rng),
        align_content: gen_content(rng),
        align_items: gen_items(rng),
        align_self: gen_items(rng),
        gap: if rng.chance(0.5) {
            Size {
                width: LengthPercentage::length(0.0),
                height: LengthPercentage::length(0.0),
            }
        } else {
            Size {
                width: gen_lp(rng),
                height: gen_lp(rng),
            }
        },
        size: Size {
            width: gen_dim(rng),
            height: gen_dim(rng),
        },
        min_size: Size {
            width: limits(rng),
            height: limits(rng),
        },
        max_size: Size {
            width: limits(rng),
            height: limits(rng),
        },
        aspect_ratio: pick(
            rng,
            &[
                None,
                None,
                None,
                Some(0.5),
                Some(1.0),
                Some(16.0 / 9.0),
                Some(3.0),
            ],
        ),
        margin,
        padding: lp_rect(rng),
        border: lp_rect(rng),
        inset,
        flex_basis: gen_basis(rng),
        flex_grow: pick(rng, &[0.0, 0.0, 0.5, 1.0, 1.0, 2.0, 3.5]),
        flex_shrink: pick(rng, &[0.0, 0.3, 1.0, 1.0, 2.0]),
        ..Style::default()
    }
}

pub fn gen_measure(rng: &mut Rng) -> Measure {
    match rng.below(8) {
        6 | 7 => Measure::Space(pick(rng, &[10.0, 24.0, 60.0])),
        0 => Measure::Empty,
        1 | 2 => Measure::Fixed(
            (rng.unit() * 80.0).round(),
            (rng.unit() * 40.0).round() / 2.0,
        ),
        3 | 4 => Measure::Text {
            words: 1 + rng.below(20),
            word: 8.0 + (rng.unit() * 40.0).round() / 4.0,
            line: pick(rng, &[12.0, 16.5, 20.0]),
        },
        _ => Measure::Aspect(pick(rng, &[0.5, 1.0, 1.5, 4.0 / 3.0])),
    }
}

pub fn gen_available(rng: &mut Rng) -> Size<AvailableSpace> {
    let axis = |rng: &mut Rng| match rng.below(6) {
        0 => AvailableSpace::MinContent,
        1 => AvailableSpace::MaxContent,
        _ => AvailableSpace::Definite(pick(rng, &[0.0, 90.0, 240.0, 400.0, 1234.5])),
    };
    Size {
        width: axis(rng),
        height: axis(rng),
    }
}

/// A tree of up to `max_nodes` nodes, at most `max_depth` levels deep.
pub fn gen_case(rng: &mut Rng, max_nodes: u32, max_depth: u32) -> FlexCase {
    let count = 1 + rng.below(max_nodes) as usize;
    let mut styles = vec![gen_style(rng)];
    let mut parents = vec![u32::MAX];
    let mut depth = vec![0u32];
    let mut measures = vec![gen_measure(rng)];
    for i in 1..count {
        // Prefer recent nodes, so trees grow deep as well as wide.
        let parent = loop {
            let p = if rng.chance(0.5) {
                i.saturating_sub(1 + rng.below(4) as usize)
            } else {
                rng.below(i as u32) as usize
            };
            if depth[p] + 1 < max_depth {
                break p;
            }
        };
        styles.push(gen_style(rng));
        parents.push(parent as u32);
        depth.push(depth[parent] + 1);
        measures.push(gen_measure(rng));
    }
    // The root is in flow, as a view root is.
    styles[0].display = Display::Flex;
    styles[0].position = Position::Relative;
    FlexCase {
        styles,
        parents,
        measures,
        available: gen_available(rng),
    }
}

/// Edits for warm relayouts: styles and content, moves, viewports.
pub fn gen_edits(rng: &mut Rng, case: &FlexCase, count: usize) -> Vec<Edit> {
    let mut parents = case.parents.clone();
    let mut edits = Vec::new();
    while edits.len() < count {
        let n = case.styles.len();
        match rng.below(10) {
            0..=5 => {
                let node = rng.below(n as u32) as usize;
                let mut style = case.styles[node].clone();
                let measure = if rng.chance(0.5) {
                    gen_measure(rng)
                } else {
                    case.measures[node]
                };
                match rng.below(4) {
                    0 => style = gen_style(rng),
                    1 => style.size.width = gen_dim(rng),
                    2 => style.flex_grow = pick(rng, &[0.0, 1.0, 2.0]),
                    _ => {}
                }
                if node == 0 {
                    style.display = Display::Flex;
                    style.position = Position::Relative;
                }
                edits.push(Edit::Node(node, Box::new(style), measure));
            }
            6..=7 if n > 1 => {
                let node = 1 + rng.below(n as u32 - 1) as usize;
                let parent = rng.below(n as u32) as usize;
                // Not under itself.
                let mut cur = parent;
                let mut inside = false;
                loop {
                    if cur == node {
                        inside = true;
                        break;
                    }
                    if cur == 0 {
                        break;
                    }
                    cur = parents[cur] as usize;
                }
                if inside {
                    continue;
                }
                parents[node] = parent as u32;
                edits.push(Edit::Move {
                    node,
                    parent,
                    index: rng.below(8) as usize,
                });
            }
            _ => edits.push(Edit::Viewport(gen_available(rng))),
        }
    }
    edits
}

/// A `LayoutInput` from every regime a parent can ask for.
pub fn gen_input(rng: &mut Rng) -> LayoutInput {
    let opt = |rng: &mut Rng| {
        if rng.chance(0.5) {
            None
        } else {
            Some(gen_length(rng).max(0.0))
        }
    };
    LayoutInput {
        run_mode: pick(rng, &[RunMode::ComputeSize, RunMode::PerformLayout]),
        sizing_mode: pick(rng, &[SizingMode::ContentSize, SizingMode::InherentSize]),
        axis: pick(
            rng,
            &[
                RequestedAxis::Horizontal,
                RequestedAxis::Vertical,
                RequestedAxis::Both,
            ],
        ),
        known_dimensions: Size {
            width: opt(rng),
            height: opt(rng),
        },
        known_dimensions_are_definite: Size {
            width: rng.chance(0.7),
            height: rng.chance(0.7),
        },
        parent_size: Size {
            width: opt(rng),
            height: opt(rng),
        },
        available_space: gen_available(rng),
        vertical_margins_are_collapsible: Line::FALSE,
    }
}

/// Taffy's algorithm over an `OwnedTree`'s rows, caches, and measures,
/// through Taffy's low-level traits (as `craie-ui` drove Taffy before
/// step 6c).
pub struct TaffyHost(pub OwnedTree);

fn index(node: taffy::NodeId) -> usize {
    u64::from(node) as usize
}

fn to_taffy_id(node: &u32) -> taffy::NodeId {
    taffy::NodeId::new(*node as u64)
}

impl TaffyHost {
    pub fn layout(&mut self, available: Size<AvailableSpace>) {
        taffy::compute_root_layout(self, taffy::NodeId::new(0), available);
    }
}

impl taffy::TraversePartialTree for TaffyHost {
    type ChildIter<'a> = std::iter::Map<std::slice::Iter<'a, u32>, fn(&u32) -> taffy::NodeId>;

    fn child_ids(&self, parent: taffy::NodeId) -> Self::ChildIter<'_> {
        self.0.children[index(parent)].iter().map(to_taffy_id)
    }

    fn child_count(&self, parent: taffy::NodeId) -> usize {
        self.0.children[index(parent)].len()
    }

    fn get_child_id(&self, parent: taffy::NodeId, i: usize) -> taffy::NodeId {
        to_taffy_id(&self.0.children[index(parent)][i])
    }
}

impl taffy::TraverseTree for TaffyHost {}

impl taffy::LayoutPartialTree for TaffyHost {
    type CoreContainerStyle<'a> = &'a LayoutRow;
    type CustomIdent = String;

    fn get_core_container_style(&self, node: taffy::NodeId) -> Self::CoreContainerStyle<'_> {
        &self.0.rows[index(node)]
    }

    fn set_unrounded_layout(&mut self, node: taffy::NodeId, layout: &Layout) {
        self.0.layouts[index(node)] = *layout;
    }

    fn compute_child_layout(&mut self, node: taffy::NodeId, inputs: LayoutInput) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return taffy::compute_hidden_layout(self, node);
        }
        taffy::compute_cached_layout(self, node, inputs, |tree, node, inputs| {
            let row = tree.0.rows[index(node)];
            if row.display() == Display::None {
                taffy::compute_hidden_layout(tree, node)
            } else if !tree.0.children[index(node)].is_empty() {
                taffy::compute_flexbox_layout(tree, node, inputs)
            } else {
                let measure = tree.0.measures[index(node)];
                taffy::compute_leaf_layout(
                    inputs,
                    &row,
                    |_, _| 0.0,
                    |known, available| measure.size(known, available),
                )
            }
        })
    }
}

impl taffy::LayoutFlexboxContainer for TaffyHost {
    type FlexboxContainerStyle<'a> = &'a LayoutRow;
    type FlexboxItemStyle<'a> = &'a LayoutRow;

    fn get_flexbox_container_style(&self, node: taffy::NodeId) -> Self::FlexboxContainerStyle<'_> {
        &self.0.rows[index(node)]
    }

    fn get_flexbox_child_style(&self, node: taffy::NodeId) -> Self::FlexboxItemStyle<'_> {
        &self.0.rows[index(node)]
    }
}

impl taffy::CacheTree for TaffyHost {
    fn cache_get(&mut self, node: taffy::NodeId, input: &LayoutInput) -> Option<LayoutOutput> {
        self.0.caches[index(node)].get(input)
    }

    fn cache_store(&mut self, node: taffy::NodeId, input: &LayoutInput, output: LayoutOutput) {
        self.0.caches[index(node)].store(input, output)
    }

    fn cache_clear(&mut self, node: taffy::NodeId) {
        self.0.caches[index(node)].clear();
    }
}
