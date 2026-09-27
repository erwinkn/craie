//! E15 (ARCHITECTURE-update topic 4): interaction lookups, walk versus
//! index. Trees of 1k, 10k and 100k nodes in three shapes:
//!
//! - deep: cards 28 pt square, each a chain of 40 nested views, wrapped
//!   in rows across the window (the root does not clip);
//! - wide: groups of up to 5,000 cells 6 pt square, wrapped;
//! - list: a scroller holding every row of a plain column (a row, a
//!   label, a button), scrolled to its middle.
//!
//! Per tree: pointer moves along a path across the window (the hit test
//! alone, walking every node and with the reach index, then the whole
//! dispatch with enter and leave); the index's upkeep (a full refresh,
//! and a refresh after one box grows or one transform changes); key presses
//! with no focus and 0, 50 or 500 key listeners (the keymap proxy until
//! claims land), a key press with a node 40 deep focused, and Tab
//! through 1,000 focusable nodes, then with them in one focus group
//! (Tab, and an arrow moving among them). Times are the mean per event; allocs
//! are allocation calls per event.
//!
//!   cargo run --release -p craie-harness --example e15_lookups

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::geom::{Affine, Size};
use craie_ui::events::{Event, Key, KeyInput, Mods, mask};
use craie_ui::host::NodeId;
use craie_ui::mutation::{NIL, NodeKind, Transaction, group_flag};
use craie_ui::ui::Ui;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn allocs() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

const VIEW: Size = Size {
    width: 1440.0,
    height: 900.0,
};
const POINTER: u32 =
    mask::POINTER_MOVE | mask::POINTER_DOWN | mask::POINTER_UP | mask::POINTER_ENTER_LEAVE;

fn style(f: impl FnOnce(&mut taffy::Style)) -> taffy::Style {
    let mut s = craie_ui::host::default_style().to_taffy();
    f(&mut s);
    s
}

fn len(v: f32) -> taffy::Dimension {
    taffy::Dimension::length(v)
}

fn full() -> taffy::Size<taffy::Dimension> {
    taffy::Size {
        width: taffy::Dimension::percent(1.0),
        height: taffy::Dimension::percent(1.0),
    }
}

fn wrap_row(s: &mut taffy::Style) {
    s.flex_direction = taffy::FlexDirection::Row;
    s.flex_wrap = taffy::FlexWrap::Wrap;
    s.align_content = Some(taffy::AlignContent::FLEX_START);
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Deep,
    Wide,
    List,
}

/// A laid-out tree and the roles its nodes play.
struct Tree {
    ui: Ui,
    nodes: u32,
    /// Nodes with pointer listeners (card tops, every 50th cell, row
    /// buttons).
    widgets: Vec<u32>,
    /// A node about 40 deep (the last card's innermost view, a row's
    /// button).
    deep: u32,
}

fn build(shape: Shape, n: u32) -> Tree {
    let mut ui = Ui::new(2.0);
    let mut t = Transaction::new(1);
    let mut widgets = Vec::new();
    let root = 1;
    t.create(root, NodeKind::View)
        .layout(
            root,
            &style(|s| {
                s.size = full();
                wrap_row(s);
            }),
        )
        .append(NIL, root);
    let mut next = 2;
    let mut id = || {
        next += 1;
        next - 1
    };
    let mut deep = root;
    let mut scroller = None;
    match shape {
        Shape::Deep => {
            let card = style(|s| {
                s.size = taffy::Size {
                    width: len(28.0),
                    height: len(28.0),
                };
            });
            let inner = style(|s| s.size = full());
            for _ in 0..n / 40 {
                let top = id();
                t.create(top, NodeKind::View)
                    .layout(top, &card)
                    .append(root, top);
                widgets.push(top);
                let mut parent = top;
                for _ in 1..40 {
                    let c = id();
                    t.create(c, NodeKind::View)
                        .layout(c, &inner)
                        .append(parent, c);
                    parent = c;
                }
                deep = parent;
            }
        }
        Shape::Wide => {
            let group = style(|s| {
                s.size.width = taffy::Dimension::percent(1.0);
                wrap_row(s);
            });
            let cell = style(|s| {
                s.size = taffy::Size {
                    width: len(6.0),
                    height: len(6.0),
                };
            });
            let mut left = n;
            while left > 0 {
                let g = id();
                t.create(g, NodeKind::View)
                    .layout(g, &group)
                    .append(root, g);
                for k in 0..left.min(5000) - 1 {
                    let c = id();
                    t.create(c, NodeKind::View).layout(c, &cell).append(g, c);
                    if k % 50 == 0 {
                        widgets.push(c);
                    }
                    deep = c;
                }
                left -= left.min(5000);
            }
        }
        Shape::List => {
            let s = id();
            t.create(s, NodeKind::View)
                .layout(
                    s,
                    &style(|s| {
                        s.size = full();
                        s.overflow.y = taffy::Overflow::Scroll;
                    }),
                )
                .append(root, s);
            scroller = Some(s);
            let row = style(|s| {
                s.flex_direction = taffy::FlexDirection::Row;
                s.size.height = len(24.0);
                s.flex_shrink = 0.0;
                s.padding.left = taffy::LengthPercentage::length(8.0);
                s.align_items = Some(taffy::AlignItems::CENTER);
            });
            let label = style(|s| {
                s.flex_grow = 1.0;
                s.size.height = len(14.0);
            });
            let button = style(|s| {
                s.size = taffy::Size {
                    width: len(40.0),
                    height: len(16.0),
                };
            });
            for _ in 0..(n - 2) / 3 {
                let r = id();
                let l = id();
                let b = id();
                t.create(r, NodeKind::View).layout(r, &row).append(s, r);
                t.create(l, NodeKind::View).layout(l, &label).append(r, l);
                t.create(b, NodeKind::View).layout(b, &button).append(r, b);
                widgets.push(b);
                deep = b;
            }
        }
    }
    for &w in &widgets {
        t.interaction(w, POINTER, false);
    }
    ui.apply_txn(&t).unwrap();
    let nodes = next - 1;
    ui.layout(VIEW);
    if let Some(s) = scroller {
        let rows = (nodes - 2) / 3;
        ui.scroll_to(NodeId(s), 0.0, rows as f32 * 12.0);
        ui.layout(VIEW);
    }
    Tree {
        ui,
        nodes,
        widgets,
        deep,
    }
}

/// Every `n / k`-th node of `1..=n`, `k` of them.
fn spread(n: u32, k: u32) -> Vec<u32> {
    (0..k.min(n)).map(|i| 1 + i * (n / k.min(n))).collect()
}

/// A pointer path across the window: a Lissajous curve, one point per
/// frame at 120 Hz for about 8 s.
fn path() -> Vec<(f32, f32)> {
    (0..1000)
        .map(|i| {
            let t = i as f32 / 1000.0 * std::f32::consts::TAU;
            let x = (0.5 + 0.49 * (3.0 * t).sin()) * VIEW.width;
            let y = (0.5 + 0.49 * (4.0 * t + 0.7).sin()) * VIEW.height;
            (x, y)
        })
        .collect()
}

struct Cost {
    us: f64,
    allocs: f64,
}

/// Mean time and allocation calls of `f` over `runs` calls, events
/// drained between calls (off the clock).
fn cost(ui: &mut Ui, runs: usize, mut f: impl FnMut(&mut Ui, usize)) -> Cost {
    // Warm the buffers once.
    f(ui, 0);
    ui.take_events();
    let mut ns = 0u128;
    let mut count = 0;
    for i in 0..runs {
        let a = allocs();
        let t = Instant::now();
        f(ui, i);
        ns += t.elapsed().as_nanos();
        count += allocs() - a;
        ui.take_events();
    }
    Cost {
        us: ns as f64 / runs as f64 / 1e3,
        allocs: count as f64 / runs as f64,
    }
}

/// Mean µs of the index refresh after `edit` and a layout pass (and
/// of the layout pass), over 50 edits.
fn upkeep(ui: &mut Ui, mut edit: impl FnMut(&mut Transaction, usize)) -> (f64, f64) {
    let (mut refresh, mut layout) = (0.0, 0.0);
    for i in 0..50 {
        let mut t = Transaction::new(10 + i as u64);
        edit(&mut t, i);
        ui.apply_txn(&t).unwrap();
        let t0 = Instant::now();
        ui.layout(VIEW);
        let t1 = Instant::now();
        ui.refresh_reach();
        layout += (t1 - t0).as_secs_f64() * 1e6;
        refresh += t1.elapsed().as_secs_f64() * 1e6;
    }
    (refresh / 50.0, layout / 50.0)
}

fn key(key: Key, ch: Option<&str>) -> Event {
    Event::KeyDown(KeyInput {
        key,
        text: ch.map(str::to_owned),
        char: ch.map(str::to_owned),
        mods: Mods::default(),
        ..KeyInput::default()
    })
}

fn row(label: &str, c: &Cost) -> String {
    format!("{label} {:>9.2} µs {:>6.1} allocs", c.us, c.allocs)
}

fn main() {
    let moves = path();
    println!(
        "E15 lookups, {}x{} logical @2x; mean per event",
        VIEW.width, VIEW.height
    );
    for shape in [Shape::Deep, Shape::Wide, Shape::List] {
        for n in [1_000, 10_000, 100_000] {
            let t0 = Instant::now();
            let mut tree = build(shape, n);
            let build_ms = t0.elapsed().as_secs_f64() * 1e3;
            let name = match shape {
                Shape::Deep => "deep",
                Shape::Wide => "wide",
                Shape::List => "list",
            };
            println!(
                "\n{name} {} nodes ({} widgets; built and laid out in {build_ms:.0} ms)",
                tree.nodes,
                tree.widgets.len(),
            );
            let t0 = Instant::now();
            tree.ui.refresh_reach();
            let full_us = t0.elapsed().as_secs_f64() * 1e6;
            let walk = cost(&mut tree.ui, moves.len(), |ui, i| {
                let (x, y) = moves[i];
                std::hint::black_box(ui.hit_test_walk(x, y));
            });
            let hit = cost(&mut tree.ui, moves.len(), |ui, i| {
                let (x, y) = moves[i];
                std::hint::black_box(ui.hit_test(x, y));
            });
            let mut hits = 0;
            for &(x, y) in &moves {
                let h = tree.ui.hit_test(x, y);
                assert_eq!(
                    h,
                    tree.ui.hit_test_walk(x, y),
                    "index and walk disagree at ({x}, {y})"
                );
                hits += h.is_some() as usize;
            }
            println!(
                "  {}  ({hits} of 1000 hit)",
                row("hit test, walk               ", &walk)
            );
            println!(
                "  {}  ({:.0}x)",
                row("hit test, index              ", &hit),
                walk.us / hit.us
            );
            let mv = cost(&mut tree.ui, moves.len(), |ui, i| {
                let (x, y) = moves[i];
                ui.dispatch(&Event::PointerMove { x, y });
            });
            println!("  {}", row("pointer move (index)         ", &mv));

            // Upkeep: a full refresh (every node stale, as after the
            // first frame or a window resize), then the refresh after a
            // box near the start grows (later siblings move) and after a
            // transform changes, each following its layout pass.
            println!("  index refresh, full         {full_us:>9.2} µs");
            let grow = match shape {
                Shape::Deep => tree.widgets[0],
                Shape::Wide => tree.widgets[0],
                Shape::List => tree.widgets[0] - 2,
            };
            let base = tree.ui.host.style(NodeId(grow)).to_taffy();
            let (refresh, layout) = upkeep(&mut tree.ui, |t, i| {
                let mut s = base.clone();
                s.size.height = len(if i % 2 == 0 { 48.0 } else { 24.0 });
                t.layout(grow, &s);
            });
            println!(
                "  index refresh, one box grows {refresh:>9.2} µs  (its layout pass: {layout:.0} µs)"
            );
            let spin = tree.widgets[tree.widgets.len() / 2];
            let (refresh, _) = upkeep(&mut tree.ui, |t, i| {
                t.transform(spin, Affine::rotate(i as f32 * 0.1));
            });
            println!("  index refresh, one transform {refresh:>9.2} µs");

            // Keys with no focus: k listeners spread through the tree.
            tree.ui.set_focus(None);
            for k in [0, 50, 500] {
                let keyed = spread(tree.nodes, k);
                let mut t = Transaction::new(2);
                for &id in &keyed {
                    let base = if tree.widgets.binary_search(&id).is_ok() {
                        POINTER
                    } else {
                        0
                    };
                    t.interaction(id, base | mask::KEY, false);
                }
                tree.ui.apply_txn(&t).unwrap();
                let c = cost(&mut tree.ui, 200, |ui, _| {
                    ui.dispatch(&key(Key::Unknown, Some("k")))
                });
                println!("  {}", row(&format!("key, no focus, {k:>3} listeners"), &c));
                let mut t = Transaction::new(3);
                for &id in &keyed {
                    let base = if tree.widgets.binary_search(&id).is_ok() {
                        POINTER
                    } else {
                        0
                    };
                    t.interaction(id, base, false);
                }
                tree.ui.apply_txn(&t).unwrap();
            }

            // A focused node about 40 deep, with a key listener at the
            // root (an app-level handler) and on the node.
            let mut t = Transaction::new(4);
            t.interaction(tree.deep, mask::KEY | mask::FOCUS, true);
            t.interaction(1, mask::KEY, false);
            tree.ui.apply_txn(&t).unwrap();
            tree.ui.set_focus(Some(NodeId(tree.deep)));
            let c = cost(&mut tree.ui, 200, |ui, _| {
                ui.dispatch(&key(Key::Unknown, Some("k")))
            });
            println!("  {}", row("key, focused (40 deep)       ", &c));
            let mut t = Transaction::new(5);
            t.interaction(tree.deep, 0, false);
            t.interaction(1, 0, false);
            tree.ui.apply_txn(&t).unwrap();

            // Tab through 1,000 focusable nodes.
            let focusable = spread(tree.nodes, 1000);
            let mut t = Transaction::new(6);
            for &id in &focusable {
                let base = if tree.widgets.binary_search(&id).is_ok() {
                    POINTER
                } else {
                    0
                };
                t.interaction(id, base, true);
            }
            tree.ui.apply_txn(&t).unwrap();
            tree.ui.set_focus(None);
            let c = cost(&mut tree.ui, 1000, |ui, _| {
                ui.dispatch(&key(Key::Tab, None))
            });
            println!("  {}", row("Tab, 1000 focusable          ", &c));

            // The same focusables but the root in one focus group at the
            // root: Tab (the group is one stop), and → moving among the
            // members.
            let mut t = Transaction::new(7);
            t.interaction(1, 0, false)
                .group(1, group_flag::HORIZONTAL | group_flag::LOOP);
            tree.ui.apply_txn(&t).unwrap();
            tree.ui.set_focus(None);
            let c = cost(&mut tree.ui, 1000, |ui, _| {
                ui.dispatch(&key(Key::Tab, None))
            });
            println!("  {}", row("Tab, 999 in one group        ", &c));
            tree.ui.set_focus(None);
            tree.ui.dispatch(&key(Key::Tab, None));
            assert_ne!(tree.ui.focused(), Some(NodeId(1)));
            let c = cost(&mut tree.ui, 1000, |ui, _| {
                ui.dispatch(&key(Key::Right, None))
            });
            println!("  {}", row("arrow, 999 in one group      ", &c));
        }
    }
}
