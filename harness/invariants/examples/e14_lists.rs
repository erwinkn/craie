//! E14 (layout-aware virtualization): virtualized list versus a plain column of every row, and the
//! native estimator's accuracy (ARCHITECTURE.md §7).
//!
//! For each item count: mount (one splice + the rows of the first
//! range, until the range is stable), a scroll inside the rendered
//! range, a jump that renders a new range, live heap bytes, and layout
//! visits. The plain column mounts every row as a text node, as the
//! non-virtualized path does. Estimates: mean and 95th-percentile error
//! of the native estimate against the measured row height, and the
//! first-frame anchoring error after a jump into unmeasured items.
//!
//!   cargo run --release -p craie-harness --example e14_lists

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::geom::Size;
use craie_harness::ListDriver;
use craie_ui::host::NodeId;
use craie_ui::mutation::{ItemDesc, ItemTemplate, NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;

static BYTES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        BYTES.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        BYTES.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        BYTES.fetch_sub(l.size(), Ordering::Relaxed);
        BYTES.fetch_add(new, Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn live() -> usize {
    BYTES.load(Ordering::Relaxed)
}

const VIEW: Size = Size {
    width: 480.0,
    height: 720.0,
};
const FONT: f32 = 14.0;

/// Chat-like texts: 2 to 60 words.
fn text(i: u32) -> String {
    const W: [&str; 8] = [
        "the",
        "list",
        "lays",
        "out",
        "rows",
        "native",
        "estimates",
        "measured",
    ];
    let n = 2 + (i * 7919) % 59;
    (0..n)
        .map(|k| W[((i + k * 3) % 8) as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

fn scroller() -> taffy::Style {
    let mut s = craie_ui::host::default_style();
    s.size = taffy::Size {
        width: taffy::Dimension::percent(1.0),
        height: taffy::Dimension::percent(1.0),
    };
    s.overflow.y = taffy::Overflow::Scroll;
    s
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

struct Virtual {
    mount_ms: f64,
    scroll_ms: f64,
    jump_ms: f64,
    live: usize,
    rows: usize,
    layout_nodes: u64,
    anchor_err: f32,
}

fn virtualized(n: u32) -> Virtual {
    // Item descriptions exist before the clock starts, as app data does.
    let items: Vec<ItemDesc> = (0..n)
        .map(|i| ItemDesc {
            template: 0,
            text_len: text(i).chars().count() as u32,
            id: i,
            unchanged: false,
        })
        .collect();
    let base = live();
    let t0 = Instant::now();
    let mut ui = Ui::new(2.0);
    let mut d = ListDriver::new(1, 10, FONT);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &scroller())
        .append(NIL, 0);
    t.create(1, NodeKind::List)
        .list_config(1, 400.0, 40.0, &[d.template()])
        .list_splice(1, 0, 0, &items)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    let before = ui.counters();
    d.settle(&mut ui, VIEW, &text, 8);
    let mount_ms = ms(t0);
    let layout_nodes = ui.counters().since(&before).layout_nodes;
    let rows = d.rows.len();
    drop(t);
    let live_bytes = live() - base;
    drop(items);

    // Inside the rendered range: no new rows.
    let t1 = Instant::now();
    for k in 1..=20 {
        ui.scroll_to(NodeId(0), 0.0, k as f32 * 5.0);
        ui.render(VIEW);
    }
    let scroll_ms = ms(t1) / 20.0;
    assert!(!d.pump(&mut ui, &text));

    // A jump into unmeasured items: the new range renders and measures.
    let extent = ui.layouts.data(NodeId(0)).scroll_extent[1];
    let target = extent * 0.61;
    let t2 = Instant::now();
    ui.scroll_to(NodeId(0), 0.0, target);
    ui.render(VIEW);
    // First-frame anchoring error: the top item holds its place, so the
    // estimate error shows at the bottom of the viewport. Where the item
    // there sat on the estimate frame versus after measuring.
    let bottom = ui
        .host
        .lists
        .get(1)
        .unwrap()
        .extents
        .index_at(target + VIEW.height * 0.9) as u32;
    let y_est = ui
        .host
        .lists
        .get(1)
        .unwrap()
        .extents
        .offset(bottom as usize)
        - target;
    d.settle(&mut ui, VIEW, &text, 8);
    let jump_ms = ms(t2);
    let l = ui.host.lists.get(1).unwrap();
    let y_measured = l.offset(bottom as usize) - ui.scroll_offset(NodeId(0))[1];
    Virtual {
        mount_ms,
        scroll_ms,
        jump_ms,
        live: live_bytes,
        rows,
        layout_nodes,
        anchor_err: (y_measured - y_est).abs(),
    }
}

struct Plain {
    mount_ms: f64,
    scroll_ms: f64,
    live: usize,
    layout_nodes: u64,
}

fn plain(n: u32) -> Plain {
    let base = live();
    let t0 = Instant::now();
    let mut ui = Ui::new(2.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &scroller())
        .append(NIL, 0);
    for i in 0..n {
        t.create(10 + i, NodeKind::Text)
            .text(10 + i, text(i), FONT, 0xFFFF_FFFF)
            .append(0, 10 + i);
    }
    ui.apply_txn(&t).unwrap();
    drop(t);
    let before = ui.counters();
    ui.render(VIEW);
    let mount_ms = ms(t0);
    let layout_nodes = ui.counters().since(&before).layout_nodes;
    let live_bytes = live() - base;
    let t1 = Instant::now();
    for k in 1..=20 {
        ui.scroll_to(NodeId(0), 0.0, k as f32 * 5.0);
        ui.render(VIEW);
    }
    Plain {
        mount_ms,
        scroll_ms: ms(t1) / 20.0,
        live: live_bytes,
        layout_nodes,
    }
}

/// Estimate error against measured heights for 2,000 items at the view
/// width, with the list's own estimator.
fn estimate_error() -> (f32, f32) {
    let mut ui = Ui::new(2.0);
    let n = 2_000u32;
    let tpl = ItemTemplate {
        base: 0.0,
        inset: 0.0,
        font_size: FONT,
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &scroller())
        .append(NIL, 0);
    t.create(1, NodeKind::List)
        .list_config(1, 0.0, 40.0, &[tpl])
        .list_splice(
            1,
            0,
            0,
            &(0..n)
                .map(|i| ItemDesc {
                    template: 0,
                    text_len: text(i).chars().count() as u32,
                    id: i,
                    unchanged: false,
                })
                .collect::<Vec<_>>(),
        )
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let est: Vec<f32> = {
        let l = ui.host.lists.get(1).unwrap();
        (0..n as usize).map(|i| l.extents.size(i)).collect()
    };
    // Measure every item: render all rows at once.
    let mut t = Transaction::new(2);
    for i in 0..n {
        t.create(10 + i, NodeKind::Text)
            .text(10 + i, text(i), FONT, 0xFFFF_FFFF)
            .list_index(10 + i, i)
            .append(1, 10 + i);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let l = ui.host.lists.get(1).unwrap();
    let mut errs: Vec<f32> = (0..n as usize)
        .map(|i| (est[i] - l.extents.size(i)).abs() / l.extents.size(i))
        .collect();
    let mean = errs.iter().sum::<f32>() / errs.len() as f32;
    errs.sort_by(f32::total_cmp);
    (mean, errs[errs.len() * 95 / 100])
}

/// Identity patterns: (name, ids). Ids reach native through the public
/// wire, so they include chosen, adversarial patterns.
fn id_patterns(n: u32) -> Vec<(&'static str, Vec<u32>)> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut random = || {
        // splitmix64
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as u32
    };
    let mut rnd: Vec<u32> = Vec::with_capacity(n as usize);
    let mut seen = std::collections::BTreeSet::new();
    while rnd.len() < n as usize {
        let v = random();
        if v != u32::MAX && seen.insert(v) {
            rnd.push(v);
        }
    }
    vec![
        ("sequential", (0..n).collect()),
        ("stride 2^11", (0..n).map(|k| k << 11).collect()),
        ("random", rnd),
    ]
}

/// A multiplicative hasher (rejected in review round 3): keeps the low
/// zero bits of strided ids, so they share buckets.
#[derive(Default)]
struct MulHasher(u64);
impl std::hash::Hasher for MulHasher {
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
    fn write_u32(&mut self, i: u32) {
        self.0 = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

/// Builds a set of `ids` and queries each once: ms.
fn set_cost(ids: &[u32], kind: &str) -> f64 {
    use std::collections::HashSet;
    let t = Instant::now();
    match kind {
        "sorted vec" => {
            let ix = craie_ui::list::IdIndex::build(ids.iter().copied());
            assert!(ids.iter().all(|&i| ix.contains(i)));
        }
        "std HashSet (SipHash, keyed)" => {
            let set: HashSet<u32> = ids.iter().copied().collect();
            assert!(ids.iter().all(|i| set.contains(i)));
        }
        _ => {
            let set: HashSet<u32, std::hash::BuildHasherDefault<MulHasher>> =
                ids.iter().copied().collect();
            assert!(ids.iter().all(|i| set.contains(i)));
        }
    }
    ms(t)
}

/// The native path with `ids`: one splice mounting the list
/// (validation + apply), then 100 single-item appends. (mount ms,
/// append µs each)
fn native_cost(ids: &[u32]) -> (f64, f64) {
    let items: Vec<ItemDesc> = ids
        .iter()
        .map(|&id| ItemDesc {
            template: 0,
            text_len: 20,
            id,
            unchanged: false,
        })
        .collect();
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::List).append(NIL, 1);
    ui.apply_txn(&t).unwrap();
    let mut t = Transaction::new(2);
    t.list_splice(1, 0, 0, &items);
    let t0 = Instant::now();
    ui.apply_txn(&t).unwrap();
    let mount = ms(t0);
    let n = ids.len() as u32;
    // Fresh ids for appends, outside every pattern (they are all even
    // or below 2^31 except random, which the check below guards).
    let fresh: Vec<u32> = (0..100u32)
        .map(|k| 0xFFFF_0000 + 2 * k + 1)
        .filter(|id| !ids.contains(id))
        .collect();
    let t1 = Instant::now();
    for (k, &id) in fresh.iter().enumerate() {
        let mut t = Transaction::new(3 + k as u64);
        t.list_splice(
            1,
            n + k as u32,
            0,
            &[ItemDesc {
                template: 0,
                text_len: 20,
                id,
                unchanged: false,
            }],
        );
        ui.apply_txn(&t).unwrap();
    }
    (mount, ms(t1) * 1000.0 / fresh.len() as f64)
}

/// A 1M-item list (sequential ids): bulk replacement of half the items
/// (one splice, the linear index merge), and single appends with a fresh
/// id above every existing one (the bridge's case) or below them (an
/// O(n) index memmove). (bulk ms, high append us, low append us)
fn update_costs() -> (f64, f64, f64) {
    let n = 1_000_000u32;
    let desc = |id: u32| ItemDesc {
        template: 0,
        text_len: 20,
        id,
        unchanged: false,
    };
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::List).append(NIL, 1).list_splice(
        1,
        0,
        0,
        &(0..n).map(|i| desc(2 * i + 2)).collect::<Vec<_>>(),
    );
    ui.apply_txn(&t).unwrap();
    let fresh: Vec<ItemDesc> = (0..n / 2).map(|i| desc(3_000_000 + i)).collect();
    let mut t = Transaction::new(2);
    t.list_splice(1, n / 4, n / 2, &fresh);
    let t0 = Instant::now();
    ui.apply_txn(&t).unwrap();
    let bulk = ms(t0);
    let append = |ui: &mut Ui, ids: &[u32], seq: u64| {
        let t0 = Instant::now();
        for (k, &id) in ids.iter().enumerate() {
            let len = ui.host.lists.get(1).unwrap().len();
            let mut t = Transaction::new(seq + k as u64);
            t.list_splice(1, len, 0, &[desc(id)]);
            ui.apply_txn(&t).unwrap();
        }
        ms(t0) * 1000.0 / ids.len() as f64
    };
    let high: Vec<u32> = (0..100).map(|k| 4_000_000 + k).collect();
    // Odd ids below every even id: each lands at the index's front.
    let low: Vec<u32> = (0..100).map(|k| 2 * k + 1).rev().collect();
    let h = append(&mut ui, &high, 10);
    let l = append(&mut ui, &low, 1_000);
    (bulk, h, l)
}

fn identity_costs() {
    println!();
    println!("identity index: build + query all (ms), and the native splice path");
    let adversarial: Vec<u32> = (0..4096u32).map(|k| k << 20).collect();
    for (name, ids) in
        std::iter::once(("k << 20 (4,096)", adversarial)).chain(id_patterns(1_000_000))
    {
        let kinds = [
            "sorted vec",
            "std HashSet (SipHash, keyed)",
            "multiplicative",
        ];
        let costs: Vec<String> = kinds
            .iter()
            .map(|k| format!("{}: {:.2}", k, set_cost(&ids, k)))
            .collect();
        let (mount, append) = native_cost(&ids);
        println!(
            "  {:>16} ({:>9} ids) | {} | native mount {:.1} ms, append {:.0} us",
            name,
            ids.len(),
            costs.join(", "),
            mount,
            append
        );
    }
}

fn main() {
    println!("E14 virtualized list vs plain column (480x720 @2x, chat texts)");
    println!(
        "{:>9} | {:>10} {:>9} {:>9} {:>10} {:>6} {:>7} | {:>10} {:>9} {:>10} {:>8}",
        "items",
        "mount ms",
        "scroll ms",
        "jump ms",
        "live KiB",
        "rows",
        "visits",
        "plain mnt",
        "plain scr",
        "plain KiB",
        "visits"
    );
    // Warm-up: the first Ui loads the system font collection.
    virtualized(100);
    plain(100);
    for n in [1_000u32, 10_000, 100_000, 1_000_000] {
        let v = virtualized(n);
        let p = if n <= 10_000 { Some(plain(n)) } else { None };
        let (pm, ps, pl, pv) = match &p {
            Some(p) => (
                format!("{:.1}", p.mount_ms),
                format!("{:.3}", p.scroll_ms),
                format!("{}", p.live / 1024),
                format!("{}", p.layout_nodes),
            ),
            None => ("-".into(), "-".into(), "-".into(), "-".into()),
        };
        println!(
            "{:>9} | {:>10.1} {:>9.3} {:>9.2} {:>10} {:>6} {:>7} | {:>10} {:>9} {:>10} {:>8}",
            n,
            v.mount_ms,
            v.scroll_ms,
            v.jump_ms,
            v.live / 1024,
            v.rows,
            v.layout_nodes,
            pm,
            ps,
            pl,
            pv
        );
        if n == 100_000 {
            println!(
                "          first-frame error at the viewport bottom after a jump: {:.1} pt",
                v.anchor_err
            );
        }
    }
    let (mean, p95) = estimate_error();
    println!(
        "estimate error vs measured (2,000 items): mean {:.1}%, p95 {:.1}%",
        mean * 100.0,
        p95 * 100.0
    );
    println!("bridge bytes per item: 11 (template u16, text length u32, id u32, flags u8)");
    println!(
        "element bytes per item: ItemDesc {} + extents {} (f32 size, bool measured, f64 tree) + index 4 = {}",
        std::mem::size_of::<ItemDesc>(),
        4 + 1 + 8,
        std::mem::size_of::<ItemDesc>() + 13 + 4
    );
    identity_costs();
    let (bulk, high, low) = update_costs();
    println!(
        "1M list: replace 500k items in one splice {bulk:.1} ms; append with a high id {high:.0} us, with a low id {low:.0} us"
    );
}
