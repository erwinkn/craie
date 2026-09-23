//! E05 (owned flex engine): cold layout and warm relayout of app-like
//! trees, the owned engine against Taffy 0.14's flex algorithm over the
//! same host, rows, caches, and measure functions (ARCHITECTURE.md §4).
//!
//! Cold: every cache empty. Warm: one leaf's content changes, its cache
//! and its ancestors' clear (Taffy's rule), and the tree lays out again.
//! Medians of several runs; allocations counted during warm relayouts.
//! Both engines must give bit-equal layouts; the example checks that.
//!
//!   cargo run --release -p craie-harness --example e05_flex

use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::rng::Rng;
use craie_harness::flex_oracle::{FlexCase, Measure, OwnedTree, layout_diff};
use craie_layout::LayoutRow;
use taffy::{
    AlignItems, AvailableSpace, Cache, CacheTree, Dimension, FlexDirection, FlexWrap, Layout,
    LayoutFlexboxContainer, LayoutInput, LayoutOutput, LayoutPartialTree, LengthPercentage,
    NodeId as TaffyId, Rect, RunMode, Size, Style, TraversePartialTree, TraverseTree,
    compute_cached_layout, compute_flexbox_layout, compute_hidden_layout, compute_leaf_layout,
    compute_root_layout,
};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: AllocLayout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: AllocLayout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: AllocLayout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Taffy's algorithm over the owned engine's host, as `craie-ui` drives
/// Taffy today (its low-level traits).
struct TaffyHost(OwnedTree);

fn id(node: TaffyId) -> usize {
    u64::from(node) as usize
}

fn to_taffy(node: &u32) -> TaffyId {
    TaffyId::new(*node as u64)
}

impl TraversePartialTree for TaffyHost {
    type ChildIter<'a> = std::iter::Map<std::slice::Iter<'a, u32>, fn(&u32) -> TaffyId>;

    fn child_ids(&self, parent: TaffyId) -> Self::ChildIter<'_> {
        self.0.children[id(parent)].iter().map(to_taffy)
    }

    fn child_count(&self, parent: TaffyId) -> usize {
        self.0.children[id(parent)].len()
    }

    fn get_child_id(&self, parent: TaffyId, index: usize) -> TaffyId {
        to_taffy(&self.0.children[id(parent)][index])
    }
}

impl TraverseTree for TaffyHost {}

impl LayoutPartialTree for TaffyHost {
    type CoreContainerStyle<'a> = &'a LayoutRow;
    type CustomIdent = String;

    fn get_core_container_style(&self, node: TaffyId) -> Self::CoreContainerStyle<'_> {
        &self.0.rows[id(node)]
    }

    fn set_unrounded_layout(&mut self, node: TaffyId, layout: &Layout) {
        self.0.layouts[id(node)] = *layout;
    }

    fn compute_child_layout(&mut self, node: TaffyId, inputs: LayoutInput) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden_layout(self, node);
        }
        compute_cached_layout(self, node, inputs, |tree, node, inputs| {
            let row = tree.0.rows[id(node)];
            if row.display() == taffy::Display::None {
                compute_hidden_layout(tree, node)
            } else if !tree.0.children[id(node)].is_empty() {
                compute_flexbox_layout(tree, node, inputs)
            } else {
                let measure = tree.0.measures[id(node)];
                compute_leaf_layout(
                    inputs,
                    &row,
                    |_, _| 0.0,
                    |known, available| measure.size(known, available),
                )
            }
        })
    }
}

impl LayoutFlexboxContainer for TaffyHost {
    type FlexboxContainerStyle<'a> = &'a LayoutRow;
    type FlexboxItemStyle<'a> = &'a LayoutRow;

    fn get_flexbox_container_style(&self, node: TaffyId) -> Self::FlexboxContainerStyle<'_> {
        &self.0.rows[id(node)]
    }

    fn get_flexbox_child_style(&self, node: TaffyId) -> Self::FlexboxItemStyle<'_> {
        &self.0.rows[id(node)]
    }
}

impl CacheTree for TaffyHost {
    fn cache_get(&mut self, node: TaffyId, input: &LayoutInput) -> Option<LayoutOutput> {
        self.0.caches[id(node)].get(input)
    }

    fn cache_store(&mut self, node: TaffyId, input: &LayoutInput, output: LayoutOutput) {
        self.0.caches[id(node)].store(input, output)
    }

    fn cache_clear(&mut self, node: TaffyId) {
        self.0.caches[id(node)] = Cache::new();
    }
}

/// An app-like tree of exactly `n` nodes: nested rows and columns with
/// padding and gaps, some wrapping rows, text and image leaves.
fn app_case(n: usize, seed: u64) -> FlexCase {
    let mut rng = Rng::new(seed);
    let mut styles = vec![Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::auto(),
        },
        ..Style::default()
    }];
    let mut parents = vec![u32::MAX];
    let mut measures = vec![Measure::Empty];
    let mut open = vec![(0u32, 0u32)];
    while styles.len() < n {
        let at = rng.below(open.len() as u32) as usize;
        let (parent, depth) = open[at];
        let container = depth < 7 && rng.chance(0.35);
        let index = styles.len() as u32;
        let style = if container {
            let row = rng.chance(0.5);
            Style {
                flex_direction: if row {
                    FlexDirection::Row
                } else {
                    FlexDirection::Column
                },
                flex_wrap: if row && rng.chance(0.3) {
                    FlexWrap::Wrap
                } else {
                    FlexWrap::NoWrap
                },
                align_items: rng.chance(0.3).then_some(AlignItems::CENTER),
                padding: Rect {
                    left: LengthPercentage::length(8.0),
                    right: LengthPercentage::length(8.0),
                    top: LengthPercentage::length(4.0),
                    bottom: LengthPercentage::length(4.0),
                },
                gap: Size {
                    width: LengthPercentage::length(6.0),
                    height: LengthPercentage::length(6.0),
                },
                flex_grow: if rng.chance(0.3) { 1.0 } else { 0.0 },
                flex_shrink: if row { 1.0 } else { 0.0 },
                ..Style::default()
            }
        } else if rng.chance(0.15) {
            Style {
                size: Size {
                    width: Dimension::length(32.0),
                    height: Dimension::length(32.0),
                },
                flex_shrink: 0.0,
                ..Style::default()
            }
        } else {
            Style {
                flex_shrink: 1.0,
                ..Style::default()
            }
        };
        styles.push(style);
        parents.push(parent);
        measures.push(if container {
            Measure::Empty
        } else {
            Measure::Text {
                words: 1 + rng.below(24),
                word: 22.0 + (rng.below(20) as f32) * 0.5,
                line: 18.0,
            }
        });
        if container {
            open.push((index, depth + 1));
        }
    }
    FlexCase {
        styles,
        parents,
        measures,
        available: Size {
            width: AvailableSpace::Definite(1280.0),
            height: AvailableSpace::Definite(800.0),
        },
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn leaves(case: &FlexCase) -> Vec<usize> {
    let mut has_children = vec![false; case.styles.len()];
    for p in case.parents.iter().skip(1) {
        has_children[*p as usize] = true;
    }
    (0..case.styles.len())
        .filter(|i| !has_children[*i])
        .collect()
}

fn same(a: &OwnedTree, b: &OwnedTree) {
    for (i, (x, y)) in a.layouts.iter().zip(&b.layouts).enumerate() {
        assert!(layout_diff(x, y).is_none(), "node {i}: {x:?} {y:?}");
    }
}

struct Run {
    cold: f64,
    warm: f64,
    warm_allocs: f64,
}

fn measure(case: &FlexCase, owned: bool) -> (Run, OwnedTree) {
    let runs = if case.styles.len() > 20_000 { 5 } else { 9 };
    let mut cold = Vec::new();
    let mut last = None;
    for _ in 0..runs {
        let mut tree = OwnedTree::new(case);
        let t = Instant::now();
        if owned {
            tree.layout(case.available);
        } else {
            let mut host = TaffyHost(tree);
            compute_root_layout(&mut host, TaffyId::new(0), case.available);
            tree = host.0;
        }
        cold.push(t.elapsed().as_secs_f64() * 1e3);
        last = Some(tree);
    }
    let mut tree = last.unwrap();
    let candidates = leaves(case);
    let mut rng = Rng::new(7);
    let mut warm = Vec::new();
    let mut allocs = Vec::new();
    for k in 0..21 {
        let leaf = candidates[rng.below(candidates.len() as u32) as usize];
        tree.measures[leaf] = Measure::Text {
            words: 1 + (k % 23),
            word: 24.0,
            line: 18.0,
        };
        tree.mark_dirty(leaf as u32);
        let before = ALLOCS.load(Ordering::Relaxed);
        let t = Instant::now();
        if owned {
            tree.layout(case.available);
        } else {
            let mut host = TaffyHost(tree);
            compute_root_layout(&mut host, TaffyId::new(0), case.available);
            tree = host.0;
        }
        warm.push(t.elapsed().as_secs_f64() * 1e3);
        allocs.push((ALLOCS.load(Ordering::Relaxed) - before) as f64);
    }
    (
        Run {
            cold: median(cold),
            warm: median(warm),
            warm_allocs: median(allocs),
        },
        tree,
    )
}

fn main() {
    println!(
        "| nodes | cold owned | cold Taffy | warm owned | warm Taffy | warm allocs owned | warm allocs Taffy |"
    );
    println!(
        "|------:|-----------:|-----------:|-----------:|-----------:|------------------:|------------------:|"
    );
    for n in [1_000usize, 10_000, 50_000] {
        let case = app_case(n, n as u64);
        let (a, owned_tree) = measure(&case, true);
        let (b, taffy_tree) = measure(&case, false);
        same(&owned_tree, &taffy_tree);
        println!(
            "| {n} | {:.2} ms | {:.2} ms | {:.3} ms | {:.3} ms | {} | {} |",
            a.cold, b.cold, a.warm, b.warm, a.warm_allocs, b.warm_allocs
        );
    }
}
