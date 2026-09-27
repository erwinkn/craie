//! E10: capacity-classed span pool versus a `Vec` side table for child
//! lists, on the same interface. Measures live bytes per node,
//! allocations, build time, and traversal time.
//!
//!   cargo run --release -p craie-harness --example e10_span_pool

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::rng::Rng;
use craie_core::span::{Span, SpanPool};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        BYTES.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_sub(l.size(), Ordering::Relaxed);
        BYTES.fetch_add(new, Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

trait Children {
    fn new(nodes: usize) -> Self;
    fn insert(&mut self, parent: u32, index: usize, child: u32);
    fn remove(&mut self, parent: u32, index: usize) -> u32;
    fn get(&self, parent: u32) -> &[u32];
}

struct VecTable(Vec<Vec<u32>>);

impl Children for VecTable {
    fn new(nodes: usize) -> Self {
        VecTable(vec![Vec::new(); nodes])
    }
    fn insert(&mut self, parent: u32, index: usize, child: u32) {
        self.0[parent as usize].insert(index, child);
    }
    fn remove(&mut self, parent: u32, index: usize) -> u32 {
        self.0[parent as usize].remove(index)
    }
    fn get(&self, parent: u32) -> &[u32] {
        &self.0[parent as usize]
    }
}

struct PoolTable {
    spans: Vec<Span>,
    pool: SpanPool<u32>,
}

impl Children for PoolTable {
    fn new(nodes: usize) -> Self {
        PoolTable {
            spans: vec![Span::EMPTY; nodes],
            pool: SpanPool::new(u32::MAX),
        }
    }
    fn insert(&mut self, parent: u32, index: usize, child: u32) {
        let mut s = self.spans[parent as usize];
        self.pool.insert(&mut s, index, child);
        self.spans[parent as usize] = s;
    }
    fn remove(&mut self, parent: u32, index: usize) -> u32 {
        let mut s = self.spans[parent as usize];
        let v = self.pool.remove(&mut s, index);
        self.spans[parent as usize] = s;
        v
    }
    fn get(&self, parent: u32) -> &[u32] {
        self.pool.get(self.spans[parent as usize])
    }
}

struct Report {
    bytes_per_node: f64,
    allocs: usize,
    build_ms: f64,
    traverse_ms: f64,
}

/// Depth-first traversal from node 0, summing ids (defeats elision).
fn traverse<C: Children>(c: &C) -> u64 {
    let mut sum = 0u64;
    let mut stack = vec![0u32];
    while let Some(n) = stack.pop() {
        sum += n as u64;
        stack.extend_from_slice(c.get(n));
    }
    sum
}

fn measure<C: Children>(nodes: usize, build: impl Fn(&mut C)) -> Report {
    let b0 = BYTES.load(Ordering::Relaxed);
    let a0 = ALLOCS.load(Ordering::Relaxed);
    let t = Instant::now();
    let mut c = C::new(nodes);
    build(&mut c);
    let build_ms = t.elapsed().as_secs_f64() * 1e3;
    let bytes = BYTES.load(Ordering::Relaxed) - b0;
    let allocs = ALLOCS.load(Ordering::Relaxed) - a0;
    let t = Instant::now();
    let mut s = 0;
    for _ in 0..10 {
        s += traverse(&c);
    }
    let traverse_ms = t.elapsed().as_secs_f64() * 1e3 / 10.0;
    std::hint::black_box(s);
    Report {
        bytes_per_node: bytes as f64 / nodes as f64,
        allocs,
        build_ms,
        traverse_ms,
    }
}

fn wide<C: Children>(c: &mut C, n: u32) {
    for i in 1..n {
        c.insert(0, (i - 1) as usize, i);
    }
}

fn deep<C: Children>(c: &mut C, n: u32) {
    for i in 1..n {
        c.insert(i - 1, 0, i);
    }
}

/// Tiny containers: every node gets 0-3 children, breadth first.
fn tiny<C: Children>(c: &mut C, n: u32) {
    let mut rng = Rng::new(10);
    let mut next = 1;
    let mut parent = 0;
    while next < n {
        let k = rng.below(4).min(n - next);
        for j in 0..k {
            c.insert(parent, j as usize, next);
            next += 1;
        }
        parent += 1;
        if parent >= next {
            break;
        }
    }
}

/// Huge lists: a root with ten lists of n/10 items.
fn huge<C: Children>(c: &mut C, n: u32) {
    let lists = 10;
    for l in 0..lists {
        c.insert(0, l as usize, 1 + l);
    }
    let per = (n - 1 - lists) / lists;
    let mut next = 1 + lists;
    for l in 0..lists {
        for i in 0..per {
            c.insert(1 + l, i as usize, next);
            next += 1;
        }
    }
}

/// Reorder churn: 1k containers of 20 children, then 200k moves between
/// random containers at random positions.
fn churn<C: Children>(c: &mut C, n: u32) {
    let parents = 1000u32;
    for p in 1..=parents {
        c.insert(0, (p - 1) as usize, p);
    }
    let mut next = parents + 1;
    for p in 1..=parents {
        for i in 0..20 {
            if next < n {
                c.insert(p, i, next);
                next += 1;
            }
        }
    }
    let mut rng = Rng::new(11);
    for _ in 0..200_000 {
        let from = 1 + rng.below(parents);
        let len = c.get(from).len();
        if len == 0 {
            continue;
        }
        let v = c.remove(from, rng.below(len as u32) as usize);
        let to = 1 + rng.below(parents);
        let at = rng.below(c.get(to).len() as u32 + 1) as usize;
        c.insert(to, at, v);
    }
}

fn row(name: &str, v: &Report, p: &Report) {
    println!(
        "{name:<8} | vec {:>6.1} B/node {:>7} allocs {:>7.2} ms build {:>6.3} ms walk \
         | pool {:>6.1} B/node {:>7} allocs {:>7.2} ms build {:>6.3} ms walk",
        v.bytes_per_node,
        v.allocs,
        v.build_ms,
        v.traverse_ms,
        p.bytes_per_node,
        p.allocs,
        p.build_ms,
        p.traverse_ms,
    );
}

fn main() {
    const N: u32 = 100_000;
    println!("E10: child lists, {N} nodes (bytes include the per-node slot)");
    macro_rules! case {
        ($name:expr, $f:ident) => {{
            let v = measure::<VecTable>(N as usize, |c| $f(c, N));
            let p = measure::<PoolTable>(N as usize, |c| $f(c, N));
            row($name, &v, &p);
        }};
    }
    case!("wide", wide);
    case!("deep", deep);
    case!("tiny", tiny);
    case!("huge", huge);
    case!("churn", churn);
}
