//! Capacity-classed span pool.
//!
//! Many small variable-length lists (child lists, scene chunk ranges)
//! live in one backing `Vec<T>` instead of one heap allocation each. A
//! list is a `Span`: a start offset, a length, and a capacity class.
//! Class `c >= 1` holds `1 << (c - 1)` items; class 0 means "no block"
//! so an empty list costs nothing and `Span::default()` is empty.
//!
//! Blocks are never split or merged. A freed block goes onto its class's
//! free list and serves the next allocation of the same class. A list
//! that outgrows its block moves to the next class; a list that shrinks
//! to a quarter of its block moves down (hysteresis, so a list that
//! oscillates around a class boundary does not move on every edit).
//!
//! The pool can track written ranges (`with_dirty_tracking`) so a GPU
//! mirror uploads only what changed.

use std::ops::Range;

use crate::dirty::DirtyRanges;

const LEN_BITS: u32 = 24;
const LEN_MASK: u32 = (1 << LEN_BITS) - 1;
/// Largest list a span can describe.
pub const MAX_SPAN_LEN: usize = LEN_MASK as usize;

/// A list inside a `SpanPool`: 8 bytes, `Copy`. Length in the low 24
/// bits of `meta`, capacity class in the high 8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    start: u32,
    meta: u32,
}

impl Span {
    pub const EMPTY: Span = Span { start: 0, meta: 0 };

    pub fn len(self) -> usize {
        (self.meta & LEN_MASK) as usize
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Offset of the first item in the pool's backing store. Meaningless
    /// for an empty span without a block.
    pub fn start(self) -> usize {
        self.start as usize
    }

    /// Backing-store range of the live items.
    pub fn range(self) -> Range<usize> {
        self.start()..self.start() + self.len()
    }

    /// Items the current block holds.
    pub fn capacity(self) -> usize {
        class_capacity(self.class())
    }

    fn class(self) -> u8 {
        (self.meta >> LEN_BITS) as u8
    }

    fn new(start: u32, len: usize, class: u8) -> Span {
        debug_assert!(len <= MAX_SPAN_LEN);
        Span {
            start,
            meta: (len as u32) | ((class as u32) << LEN_BITS),
        }
    }

    fn with_len(self, len: usize) -> Span {
        Span::new(self.start, len, self.class())
    }
}

fn class_capacity(class: u8) -> usize {
    if class == 0 { 0 } else { 1 << (class - 1) }
}

/// Smallest class that holds `len` items.
fn class_for(len: usize) -> u8 {
    if len == 0 {
        0
    } else {
        (usize::BITS - (len - 1).leading_zeros()) as u8 + 1
    }
}

/// Allocation behavior, for experiments and cost assertions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpanStats {
    /// Blocks handed out (fresh or recycled).
    pub allocs: u64,
    /// Blocks served from a free list.
    pub reuses: u64,
    /// Blocks returned to a free list.
    pub frees: u64,
    /// Lists copied to a block of another class.
    pub moves: u64,
    /// Items the backing store grew by (fresh blocks only).
    pub grown_items: u64,
    /// Items in live blocks (sum of live span capacities).
    pub live_capacity: u64,
    /// Items in live lists (sum of live span lengths).
    pub live_len: u64,
}

pub struct SpanPool<T: Copy> {
    items: Vec<T>,
    /// Free block starts per class.
    free: Vec<Vec<u32>>,
    /// Value written into fresh backing slots.
    fill: T,
    dirty: Option<DirtyRanges>,
    pub stats: SpanStats,
}

impl<T: Copy> SpanPool<T> {
    pub fn new(fill: T) -> SpanPool<T> {
        SpanPool {
            items: Vec::new(),
            free: Vec::new(),
            fill,
            dirty: None,
            stats: SpanStats::default(),
        }
    }

    /// A pool that records every written backing range (for GPU upload).
    pub fn with_dirty_tracking(fill: T) -> SpanPool<T> {
        SpanPool {
            dirty: Some(DirtyRanges::default()),
            ..SpanPool::new(fill)
        }
    }

    /// The whole backing store, including free blocks. For GPU mirrors.
    pub fn backing(&self) -> &[T] {
        &self.items
    }

    /// Bytes the backing store occupies (capacity, not length).
    pub fn backing_bytes(&self) -> usize {
        self.items.capacity() * size_of::<T>()
    }

    pub fn get(&self, span: Span) -> &[T] {
        &self.items[span.range()]
    }

    pub fn get_mut(&mut self, span: Span) -> &mut [T] {
        self.mark(span.range());
        &mut self.items[span.range()]
    }

    /// Items written since the last `take_dirty` (upper bound).
    pub fn dirty_items(&self) -> usize {
        self.dirty.as_ref().map_or(0, DirtyRanges::items_upper)
    }

    /// Takes written ranges since the last call, merged and sorted.
    pub fn take_dirty(&mut self) -> Vec<Range<usize>> {
        self.dirty.as_mut().map(DirtyRanges::take).unwrap_or_default()
    }

    /// Allocates a list of `len` fill values.
    pub fn alloc(&mut self, len: usize) -> Span {
        if len == 0 {
            return Span::EMPTY;
        }
        let class = class_for(len);
        let start = self.alloc_block(class);
        self.stats.live_len += len as u64;
        let span = Span::new(start, len, class);
        self.mark(span.range());
        span
    }

    /// Returns the list's block to its free list; `span` becomes empty.
    pub fn free(&mut self, span: &mut Span) {
        let class = span.class();
        if class != 0 {
            self.stats.live_len -= span.len() as u64;
            self.free_block(span.start, class);
        }
        *span = Span::EMPTY;
    }

    /// Replaces the list's contents with `values`, moving blocks only when
    /// the class must change.
    pub fn set(&mut self, span: &mut Span, values: &[T]) {
        let want = self.fit_class(span.class(), values.len());
        if want != span.class() {
            let mut old = *span;
            self.free(&mut old);
            *span = Span::EMPTY;
            if want != 0 {
                let start = self.alloc_block(want);
                *span = Span::new(start, 0, want);
            }
        } else {
            self.stats.live_len -= span.len() as u64;
        }
        *span = span.with_len(values.len());
        self.stats.live_len += values.len() as u64;
        if !values.is_empty() {
            self.items[span.range()].copy_from_slice(values);
            self.mark(span.range());
        }
    }

    pub fn push(&mut self, span: &mut Span, value: T) {
        let len = span.len();
        self.insert(span, len, value);
    }

    /// Inserts `value` at `index`, shifting later items up.
    pub fn insert(&mut self, span: &mut Span, index: usize, value: T) {
        let len = span.len();
        assert!(index <= len, "span insert out of bounds");
        if len + 1 > span.capacity() {
            self.relocate(span, class_for(len + 1));
        }
        let start = span.start();
        self.items.copy_within(start + index..start + len, start + index + 1);
        self.items[start + index] = value;
        *span = span.with_len(len + 1);
        self.stats.live_len += 1;
        self.mark(start + index..start + len + 1);
    }

    /// Removes the item at `index`, shifting later items down.
    pub fn remove(&mut self, span: &mut Span, index: usize) -> T {
        let len = span.len();
        assert!(index < len, "span remove out of bounds");
        let start = span.start();
        let value = self.items[start + index];
        self.items.copy_within(start + index + 1..start + len, start + index);
        *span = span.with_len(len - 1);
        self.stats.live_len -= 1;
        self.mark(start + index..start + len - 1);
        let want = self.fit_class(span.class(), len - 1);
        if want != span.class() {
            if want == 0 {
                self.free(span);
            } else {
                self.relocate(span, want);
            }
        }
        value
    }

    /// Class a list of `len` should occupy, given its current class:
    /// grow when it no longer fits, shrink only at a quarter full.
    fn fit_class(&self, class: u8, len: usize) -> u8 {
        let cap = class_capacity(class);
        if len == 0 {
            0
        } else if len > cap || len * 4 <= cap {
            class_for(len)
        } else {
            class
        }
    }

    /// Moves the list into a fresh block of `class`.
    fn relocate(&mut self, span: &mut Span, class: u8) {
        let len = span.len();
        let start = self.alloc_block(class);
        let old = *span;
        if len > 0 {
            self.items
                .copy_within(old.start()..old.start() + len, start as usize);
            self.stats.moves += 1;
        }
        if old.class() != 0 {
            self.free_block(old.start, old.class());
        }
        *span = Span::new(start, len, class);
        self.mark(span.range());
    }

    fn alloc_block(&mut self, class: u8) -> u32 {
        self.stats.allocs += 1;
        self.stats.live_capacity += class_capacity(class) as u64;
        if let Some(start) = self.free.get_mut(class as usize).and_then(Vec::pop) {
            self.stats.reuses += 1;
            return start;
        }
        let cap = class_capacity(class);
        let start = self.items.len();
        assert!(start + cap <= u32::MAX as usize, "span pool exhausted");
        self.items.resize(start + cap, self.fill);
        self.stats.grown_items += cap as u64;
        start as u32
    }

    fn free_block(&mut self, start: u32, class: u8) {
        self.stats.frees += 1;
        self.stats.live_capacity -= class_capacity(class) as u64;
        let class = class as usize;
        if self.free.len() <= class {
            self.free.resize_with(class + 1, Vec::new);
        }
        self.free[class].push(start);
    }

    fn mark(&mut self, range: Range<usize>) {
        if let Some(d) = &mut self.dirty
            && !range.is_empty()
        {
            d.add(range);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert_eq!(class_for(0), 0);
        assert_eq!(class_for(1), 1);
        assert_eq!(class_for(2), 2);
        assert_eq!(class_for(3), 3);
        assert_eq!(class_for(4), 3);
        assert_eq!(class_for(5), 4);
        assert_eq!(class_capacity(class_for(1000)), 1024);
        assert_eq!(size_of::<Span>(), 8);
    }

    #[test]
    fn push_insert_remove_keep_order() {
        let mut pool = SpanPool::new(0u32);
        let mut s = Span::EMPTY;
        for i in 0..10 {
            pool.push(&mut s, i);
        }
        pool.insert(&mut s, 0, 100);
        pool.insert(&mut s, 5, 200);
        assert_eq!(pool.get(s), &[100, 0, 1, 2, 3, 200, 4, 5, 6, 7, 8, 9]);
        assert_eq!(pool.remove(&mut s, 5), 200);
        assert_eq!(pool.remove(&mut s, 0), 100);
        assert_eq!(pool.get(s), &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn freed_blocks_are_reused_and_empty_costs_nothing() {
        let mut pool = SpanPool::new(0u32);
        let mut a = pool.alloc(3);
        let grown = pool.stats.grown_items;
        pool.free(&mut a);
        assert!(a.is_empty());
        let b = pool.alloc(4); // same class as 3
        assert_eq!(pool.stats.grown_items, grown);
        assert_eq!(pool.stats.reuses, 1);
        assert_eq!(b.len(), 4);
        assert_eq!(pool.alloc(0), Span::EMPTY);
    }

    #[test]
    fn shrink_has_hysteresis() {
        let mut pool = SpanPool::new(0u32);
        let mut s = Span::EMPTY;
        for i in 0..8 {
            pool.push(&mut s, i);
        }
        assert_eq!(s.capacity(), 8);
        // 8 -> 5 stays; 5 <-> 4 oscillation does not move.
        for _ in 0..3 {
            pool.remove(&mut s, 0);
        }
        let moves = pool.stats.moves;
        pool.remove(&mut s, 0);
        pool.push(&mut s, 9);
        pool.remove(&mut s, 0);
        pool.push(&mut s, 9);
        assert_eq!(pool.stats.moves, moves);
        // Down to a quarter: moves to a smaller class.
        while s.len() > 2 {
            pool.remove(&mut s, 0);
        }
        assert!(s.capacity() < 8);
        while !s.is_empty() {
            pool.remove(&mut s, 0);
        }
        assert_eq!(s, Span::EMPTY);
        assert_eq!(pool.stats.live_capacity, 0);
        assert_eq!(pool.stats.live_len, 0);
    }

    #[test]
    fn set_replaces_and_tracks_dirty() {
        let mut pool = SpanPool::with_dirty_tracking(0u8);
        let mut a = Span::EMPTY;
        pool.set(&mut a, &[1, 2, 3]);
        let mut b = Span::EMPTY;
        pool.set(&mut b, &[4, 5]);
        pool.take_dirty();
        pool.set(&mut a, &[7, 8, 9]);
        assert_eq!(pool.take_dirty(), vec![a.range()]);
        assert_eq!(pool.get(a), &[7, 8, 9]);
        assert_eq!(pool.get(b), &[4, 5]);
        pool.set(&mut a, &[]);
        assert!(a.is_empty());
    }

    /// Model test: random edits on many lists must match `Vec` lists.
    #[test]
    fn model_matches_vec() {
        let mut rng = crate::rng::Rng::new(0x5eed);
        let mut pool = SpanPool::new(u32::MAX);
        let mut spans = vec![Span::EMPTY; 64];
        let mut model: Vec<Vec<u32>> = vec![Vec::new(); 64];
        for step in 0..20_000u32 {
            let i = rng.below(64) as usize;
            match rng.below(6) {
                0..=2 => {
                    let at = rng.below(model[i].len() as u32 + 1) as usize;
                    pool.insert(&mut spans[i], at, step);
                    model[i].insert(at, step);
                }
                3 | 4 if !model[i].is_empty() => {
                    let at = rng.below(model[i].len() as u32) as usize;
                    assert_eq!(pool.remove(&mut spans[i], at), model[i].remove(at));
                }
                5 => {
                    let n = rng.below(40) as usize;
                    let v: Vec<u32> = (0..n as u32).map(|k| k + step).collect();
                    pool.set(&mut spans[i], &v);
                    model[i] = v;
                }
                _ => {}
            }
            assert_eq!(pool.get(spans[i]), model[i].as_slice());
        }
        for (s, m) in spans.iter().zip(&model) {
            assert_eq!(pool.get(*s), m.as_slice());
        }
        let live: usize = model.iter().map(Vec::len).sum();
        assert_eq!(pool.stats.live_len, live as u64);
        // Live blocks never overlap.
        let mut ranges: Vec<Range<usize>> = spans
            .iter()
            .filter(|s| s.capacity() > 0)
            .map(|s| s.start()..s.start() + s.capacity())
            .collect();
        ranges.sort_by_key(|r| r.start);
        for w in ranges.windows(2) {
            assert!(w[0].end <= w[1].start);
        }
    }
}
