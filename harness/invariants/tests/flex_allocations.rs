//! The owned flex engine allocates nothing when it lays out again
//! containers it laid out before (children up to the pool's 256-item
//! limit), whatever the new content: line counts, sizes, and viewports
//! change (S6B-01).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use craie_core::rng::Rng;
use craie_harness::flex_oracle::{
    FlexCase, Measure, OwnedTree, gen_available, gen_case, gen_measure,
};
use taffy::{AvailableSpace, Dimension, Display, FlexDirection, FlexWrap, Size, Style};

thread_local! {
    /// Allocations on this thread (tests run in parallel).
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn count() {
    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn allocs(f: impl FnOnce()) -> usize {
    let a = ALLOCS.with(Cell::get);
    f();
    ALLOCS.with(Cell::get) - a
}

/// The reviewer's case: a wrapping row 100 wide with 32 leaves of 10 x
/// 10; the first leaf grows to 100, so the row needs more lines than
/// its first layout had.
#[test]
fn more_lines_after_a_content_change() {
    let leaf = Style {
        flex_shrink: 0.0,
        ..Style::default()
    };
    let mut styles = vec![Style {
        flex_direction: FlexDirection::Row,
        flex_wrap: FlexWrap::Wrap,
        size: Size {
            width: Dimension::length(100.0),
            height: Dimension::auto(),
        },
        ..Style::default()
    }];
    styles.extend(std::iter::repeat_n(leaf, 32));
    let mut parents = vec![u32::MAX];
    parents.extend(std::iter::repeat_n(0, 32));
    let case = FlexCase {
        measures: vec![Measure::Fixed(10.0, 10.0); 33],
        styles,
        parents,
        available: Size {
            width: AvailableSpace::Definite(100.0),
            height: AvailableSpace::Definite(400.0),
        },
    };
    let mut tree = OwnedTree::new(&case);
    tree.layout(case.available);
    let lines_before = tree.layouts[32].location.y;
    tree.measures[1] = Measure::Fixed(100.0, 10.0);
    tree.mark_dirty(1);
    let n = allocs(|| tree.layout(case.available));
    assert!(tree.layouts[32].location.y > lines_before, "more lines");
    assert_eq!(n, 0, "allocations in the warm relayout");
}

/// Generated trees: after one layout, content and style edits (display
/// kept, so no container is new) and viewport changes lay out again
/// with no allocation.
#[test]
fn warm_relayouts_allocate_nothing() {
    let mut relayouts = 0;
    for seed in 1..=400u64 {
        let mut rng = Rng::new(seed);
        let case = gen_case(&mut rng, 60, 5);
        let mut tree = OwnedTree::new(&case);
        tree.layout(case.available);
        for _ in 0..6 {
            let node = rng.below(case.styles.len() as u32) as usize;
            tree.measures[node] = gen_measure(&mut rng);
            if rng.chance(0.5) {
                let mut style = craie_harness::flex_oracle::gen_style(&mut rng);
                style.display = case.styles[node].display;
                if node == 0 {
                    style.display = Display::Flex;
                    style.position = taffy::Position::Relative;
                }
                tree.rows[node] = craie_layout::LayoutRow::from(&style);
            }
            tree.mark_dirty(node as u32);
            let available = if rng.chance(0.3) {
                gen_available(&mut rng)
            } else {
                case.available
            };
            let n = allocs(|| tree.layout(available));
            assert_eq!(n, 0, "seed {seed}: {n} allocations in a warm relayout");
            relayouts += 1;
        }
    }
    eprintln!("{relayouts} warm relayouts, no allocation");
}
