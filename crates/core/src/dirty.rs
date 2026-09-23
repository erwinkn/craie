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

    /// Empties the queue into `out` (cleared first), keeping both
    /// allocations.
    pub fn drain_into(&mut self, out: &mut Vec<u32>) {
        out.clear();
        for &id in &self.items {
            self.marks[(id / 64) as usize] &= !(1u64 << (id % 64));
        }
        std::mem::swap(out, &mut self.items);
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

    /// Total items covered.
    pub fn covered(&mut self) -> usize {
        self.normalize();
        self.ranges.iter().map(|r| r.len()).sum()
    }

    fn normalize(&mut self) {
        self.ranges.sort_by_key(|r| r.start);
        let mut out: Vec<Range<usize>> = Vec::with_capacity(self.ranges.len());
        for r in self.ranges.drain(..) {
            match out.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => out.push(r),
            }
        }
        self.ranges = out;
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
}
