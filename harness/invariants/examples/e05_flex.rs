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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::rng::Rng;
use craie_harness::flex_oracle::{FlexCase, Measure, OwnedTree, TaffyHost, layout_diff};
use taffy::{
    AlignItems, AvailableSpace, Dimension, FlexDirection, FlexWrap, LengthPercentage, Rect, Size,
    Style,
};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Counting is on only in the counting runs; timed runs pay one relaxed
/// load per allocation.
static COUNTING: AtomicBool = AtomicBool::new(false);

struct Counting;

fn count() {
    if COUNTING.load(Ordering::Relaxed) {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: AllocLayout) -> *mut u8 {
        count();
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: AllocLayout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: AllocLayout, new: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

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
    warm_allocs: usize,
    warm_allocs_max: usize,
}

fn layout(tree: OwnedTree, available: Size<AvailableSpace>, owned: bool) -> OwnedTree {
    if owned {
        let mut tree = tree;
        tree.layout(available);
        tree
    } else {
        let mut host = TaffyHost(tree);
        host.layout(available);
        host.0
    }
}

/// The warm edits: the same leaves and contents for both engines.
fn edits(case: &FlexCase) -> Vec<(usize, Measure)> {
    let candidates = leaves(case);
    let mut rng = Rng::new(7);
    (0..21)
        .map(|k| {
            let leaf = candidates[rng.below(candidates.len() as u32) as usize];
            let measure = Measure::Text {
                words: 1 + (k % 23),
                word: 24.0,
                line: 18.0,
            };
            (leaf, measure)
        })
        .collect()
}

fn measure(case: &FlexCase, owned: bool) -> (Run, OwnedTree) {
    let runs = if case.styles.len() > 20_000 { 5 } else { 9 };
    let mut cold = Vec::with_capacity(runs);
    let mut last = None;
    for _ in 0..runs {
        let tree = OwnedTree::new(case);
        let t = Instant::now();
        let tree = layout(tree, case.available, owned);
        cold.push(t.elapsed().as_secs_f64() * 1e3);
        last = Some(tree);
    }
    let edits = edits(case);
    // Timed warm relayouts, counting off.
    let mut tree = last.unwrap();
    let mut warm = Vec::with_capacity(edits.len());
    for (leaf, content) in &edits {
        tree.measures[*leaf] = *content;
        tree.mark_dirty(*leaf as u32);
        let t = Instant::now();
        tree = layout(tree, case.available, owned);
        warm.push(t.elapsed().as_secs_f64() * 1e3);
    }
    // Counted warm relayouts on a fresh tree, the same edits.
    let mut counted = layout(OwnedTree::new(case), case.available, owned);
    let mut allocs = Vec::with_capacity(edits.len());
    for (leaf, content) in &edits {
        counted.measures[*leaf] = *content;
        counted.mark_dirty(*leaf as u32);
        let before = ALLOCS.load(Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        counted = layout(counted, case.available, owned);
        COUNTING.store(false, Ordering::Relaxed);
        let n = ALLOCS.load(Ordering::Relaxed) - before;
        allocs.push(n);
    }
    allocs.sort();
    (
        Run {
            cold: median(cold),
            warm: median(warm),
            warm_allocs: allocs[allocs.len() / 2],
            warm_allocs_max: allocs[allocs.len() - 1],
        },
        tree,
    )
}

fn main() {
    println!(
        "| nodes | cold owned | cold Taffy | warm owned | warm Taffy | warm allocs owned (median / max) | warm allocs Taffy (median / max) |"
    );
    println!("|------:|-----------:|-----------:|-----------:|-----------:|------:|------:|");
    for n in [1_000usize, 10_000, 50_000] {
        let case = app_case(n, n as u64);
        let (a, owned_tree) = measure(&case, true);
        let (b, taffy_tree) = measure(&case, false);
        same(&owned_tree, &taffy_tree);
        println!(
            "| {n} | {:.2} ms | {:.2} ms | {:.3} ms | {:.3} ms | {} / {} | {} / {} |",
            a.cold,
            b.cold,
            a.warm,
            b.warm,
            a.warm_allocs,
            a.warm_allocs_max,
            b.warm_allocs,
            b.warm_allocs_max
        );
    }
}
