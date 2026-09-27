//! Dirty tracking: a deduplicating work queue over dense ids, and a
//! merged set of written ranges.
//!
//! Queues schedule work; revisions (`crate::rev`) prove validity. A
//! queue never answers "is this current?".

use std::ops::Range;

/// A queue of dense ids with O(1) dedupe: each id sits in the queue at
/// most once until drained. Marks live in a bitset indexed by id.
#[derive(Clone, Debug, Default)]
pub struct DirtyQueue {
    items: Vec<u32>,
    marks: Vec<u64>,
}

impl DirtyQueue {
    pub fn new() -> DirtyQueue {
        DirtyQueue::default()
    }

    /// Enqueues `id` unless it is already queued. Returns true when added.
    pub fn push(&mut self, id: u32) -> bool {
        let word = (id / 64) as usize;
        let bit = 1u64 << (id % 64);
        if word >= self.marks.len() {
            self.marks.resize(word + 1, 0);
        }
        if self.marks[word] & bit != 0 {
            return false;
        }
        self.marks[word] |= bit;
        self.items.push(id);
        true
    }

    pub fn contains(&self, id: u32) -> bool {
        self.marks
            .get((id / 64) as usize)
            .is_some_and(|w| w & (1u64 << (id % 64)) != 0)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Queued ids in push order.
    pub fn as_slice(&self) -> &[u32] {
        &self.items
    }

    /// Empties the queue into `out` (cleared first). Copies: the queue
    /// and `out` each keep their own buffer, so each grows once to its
    /// own peak (swapping would trade buffers and grow them again).
    pub fn drain_into(&mut self, out: &mut Vec<u32>) {
        out.clear();
        out.extend_from_slice(&self.items);
        for &id in &self.items {
            self.marks[(id / 64) as usize] &= !(1u64 << (id % 64));
        }
        self.items.clear();
    }

    /// Empties the queue into a new vector.
    pub fn take(&mut self) -> Vec<u32> {
        let mut out = Vec::new();
        self.drain_into(&mut out);
        out
    }

    pub fn clear(&mut self) {
        for &id in &self.items {
            self.marks[(id / 64) as usize] &= !(1u64 << (id % 64));
        }
        self.items.clear();
    }
}

/// Written ranges, merged when they touch. Past `MAX_RANGES` entries the
/// set collapses to its union: a few large uploads beat many tiny ones.
#[derive(Clone, Debug, Default)]
pub struct DirtyRanges {
    ranges: Vec<Range<usize>>,
}

const MAX_RANGES: usize = 64;

impl DirtyRanges {
    pub fn add(&mut self, r: Range<usize>) {
        if r.is_empty() {
            return;
        }
        // Fast path: sequential writes extend the last range.
        if let Some(last) = self.ranges.last_mut()
            && r.start <= last.end
            && r.end >= last.start
        {
            last.start = last.start.min(r.start);
            last.end = last.end.max(r.end);
            return;
        }
        self.ranges.push(r);
        if self.ranges.len() > MAX_RANGES {
            self.normalize();
            if self.ranges.len() > MAX_RANGES {
                let start = self.ranges[0].start;
                let end = self.ranges.last().unwrap().end;
                self.ranges.clear();
                self.ranges.push(start..end);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Items covered, counting overlaps twice (no normalization).
    pub fn items_upper(&self) -> usize {
        self.ranges.iter().map(|r| r.len()).sum()
    }

    /// Sorted, non-overlapping, non-touching ranges; clears the set.
    pub fn take(&mut self) -> Vec<Range<usize>> {
        self.normalize();
        std::mem::take(&mut self.ranges)
    }

    /// `take` into `out` (replaced). Copies, so the set keeps its own
    /// buffer and one reused `out` serves many sets without allocating.
    pub fn take_into(&mut self, out: &mut Vec<Range<usize>>) {
        self.normalize();
        out.clear();
        out.extend_from_slice(&self.ranges);
        self.ranges.clear();
    }

    /// Total items covered.
    pub fn covered(&mut self) -> usize {
        self.normalize();
        self.ranges.iter().map(|r| r.len()).sum()
    }

    /// Sorts and merges in place: no allocation (unstable sort, merge by
    /// compaction), and the set keeps its capacity.
    fn normalize(&mut self) {
        if self.ranges.len() < 2 {
            return;
        }
        self.ranges.sort_unstable_by_key(|r| r.start);
        let mut w = 0;
        for i in 1..self.ranges.len() {
            let r = self.ranges[i].clone();
            if r.start <= self.ranges[w].end {
                self.ranges[w].end = self.ranges[w].end.max(r.end);
            } else {
                w += 1;
                self.ranges[w] = r;
            }
        }
        self.ranges.truncate(w + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_dedupes_until_drained() {
        let mut q = DirtyQueue::new();
        assert!(q.push(5));
        assert!(!q.push(5));
        assert!(q.push(700));
        assert!(q.contains(700));
        assert_eq!(q.take(), vec![5, 700]);
        assert!(!q.contains(5));
        assert!(q.push(5));
    }

    #[test]
    fn ranges_merge() {
        let mut d = DirtyRanges::default();
        d.add(10..20);
        d.add(0..5);
        d.add(20..25);
        d.add(3..11);
        assert_eq!(d.take(), vec![0..25]);
        d.add(0..1);
        d.add(5..6);
        assert_eq!(d.take(), vec![0..1, 5..6]);
    }

    /// In-place normalization equals a bitmap union: sorted, disjoint,
    /// non-touching ranges covering exactly the written items. The set
    /// keeps its capacity across takes.
    #[test]
    fn ranges_match_bitmap_oracle() {
        let mut rng = crate::rng::Rng::new(7);
        let mut d = DirtyRanges::default();
        let mut out = Vec::new();
        for _ in 0..200 {
            let mut bits = [false; 300];
            // At most MAX_RANGES writes: the set never collapses.
            for _ in 0..rng.below(MAX_RANGES as u32 + 1) {
                let a = rng.below(280) as usize;
                let r = a..a + 1 + rng.below(20) as usize;
                bits[r.clone()].iter_mut().for_each(|b| *b = true);
                d.add(r);
            }
            d.take_into(&mut out);
            assert!(d.ranges.is_empty());
            for w in out.windows(2) {
                assert!(w[0].end < w[1].start, "{out:?}");
            }
            let mut got = [false; 300];
            for r in &out {
                got[r.clone()].iter_mut().for_each(|b| *b = true);
            }
            assert_eq!(got, bits);
        }
    }
}
