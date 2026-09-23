//! Per-item extents of a virtualized list: an estimate or a measured
//! size per item, with O(log n) prefix sums (a Fenwick tree), so offset
//! lookups, visible ranges, and updates never walk every item.
//!
//! The list owns this store; nothing else keeps a copy of item sizes.

use std::ops::Range;

/// Extents of `len` items along the list's axis.
#[derive(Clone, Debug, Default)]
pub struct Extents {
    /// Current extent per item (estimate until measured).
    size: Vec<f32>,
    /// Whether `size[i]` is a measurement.
    measured: Vec<bool>,
    /// Fenwick tree over `size`, in f64 so long sums stay exact enough.
    tree: Vec<f64>,
}

impl Extents {
    pub fn new() -> Extents {
        Extents::default()
    }

    pub fn len(&self) -> usize {
        self.size.len()
    }

    pub fn is_empty(&self) -> bool {
        self.size.is_empty()
    }

    /// Replaces items `range` with `estimates` (items after it shift).
    /// Measurements of other items survive. O(n) (one rebuild).
    pub fn splice(&mut self, range: Range<usize>, estimates: impl IntoIterator<Item = f32>) {
        let n = self.size.len();
        let range = range.start.min(n)..range.end.min(n).max(range.start.min(n));
        let before = self.size.len();
        self.size.splice(range.clone(), estimates);
        let inserted = self.size.len() + range.len() - before;
        self.measured
            .splice(range, std::iter::repeat_n(false, inserted));
        self.rebuild();
    }

    /// Sets every unmeasured item's estimate from `f(i)`, and drops all
    /// measurements when `forget` (a width change makes them stale).
    /// O(n) (one rebuild).
    pub fn reestimate(&mut self, forget: bool, mut f: impl FnMut(usize) -> f32) {
        for i in 0..self.size.len() {
            if forget {
                self.measured[i] = false;
            }
            if !self.measured[i] {
                self.size[i] = f(i);
            }
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let n = self.size.len();
        self.tree.clear();
        self.tree.resize(n + 1, 0.0);
        for i in 0..n {
            let j = i + 1;
            self.tree[j] += self.size[i] as f64;
            let parent = j + (j & j.wrapping_neg());
            if parent <= n {
                let v = self.tree[j];
                self.tree[parent] += v;
            }
        }
    }

    fn add(&mut self, i: usize, delta: f64) {
        let mut j = i + 1;
        while j < self.tree.len() {
            self.tree[j] += delta;
            j += j & j.wrapping_neg();
        }
    }

    pub fn size(&self, i: usize) -> f32 {
        self.size[i]
    }

    pub fn is_measured(&self, i: usize) -> bool {
        self.measured[i]
    }

    /// Items with a measurement.
    pub fn measured_count(&self) -> usize {
        self.measured.iter().filter(|m| **m).count()
    }

    /// Records a measurement. Returns the change in extent.
    pub fn measure(&mut self, i: usize, v: f32) -> f32 {
        self.measured[i] = true;
        let d = v - self.size[i];
        if d != 0.0 {
            self.size[i] = v;
            self.add(i, d as f64);
        }
        d
    }

    /// Sum of items `0..i`: the offset of item `i`. O(log n).
    pub fn offset(&self, i: usize) -> f32 {
        let mut j = i.min(self.size.len());
        let mut s = 0.0;
        while j > 0 {
            s += self.tree[j];
            j -= j & j.wrapping_neg();
        }
        s as f32
    }

    pub fn total(&self) -> f32 {
        self.offset(self.size.len())
    }

    /// The item containing offset `y`: the last item for `y` past the
    /// end, 0 for negative `y`. O(log n).
    pub fn index_at(&self, y: f32) -> usize {
        let n = self.size.len();
        if n == 0 {
            return 0;
        }
        let mut pos = 0usize;
        let mut rem = y as f64;
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n && self.tree[next] <= rem {
                pos = next;
                rem -= self.tree[next];
            }
            step >>= 1;
        }
        pos.min(n - 1)
    }

    /// Heap bytes held (capacity), for memory accounting.
    pub fn heap_bytes(&self) -> usize {
        self.size.capacity() * 4 + self.measured.capacity() + self.tree.capacity() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    /// Offsets and lookups equal a linear model under random
    /// measurements and splices.
    #[test]
    fn matches_linear_model() {
        let mut e = Extents::new();
        let mut model: Vec<f32> = vec![20.0; 1000];
        e.splice(0..0, model.iter().copied());
        let mut rng = Rng::new(3);
        for round in 0..400 {
            if rng.chance(0.2) {
                let at = rng.below(model.len() as u32 + 1) as usize;
                let rm = rng.below(5).min((model.len() - at) as u32) as usize;
                let add: Vec<f32> = (0..rng.below(6)).map(|k| 10.0 + k as f32).collect();
                e.splice(at..at + rm, add.iter().copied());
                model.splice(at..at + rm, add);
            } else if !model.is_empty() {
                let i = rng.below(model.len() as u32) as usize;
                let v = 5.0 + rng.below(80) as f32;
                e.measure(i, v);
                model[i] = v;
            }
            if round % 50 == 0 || round == 399 {
                let mut acc = 0.0f32;
                for (i, v) in model.iter().enumerate() {
                    assert!((e.offset(i) - acc).abs() < 1e-2, "offset {i}");
                    assert_eq!(e.index_at(acc + v / 2.0), i);
                    acc += v;
                }
                assert!((e.total() - acc).abs() < 1e-2);
                assert_eq!(e.index_at(-5.0), 0);
                assert_eq!(e.index_at(acc + 100.0), model.len() - 1);
            }
        }
    }

    #[test]
    fn estimates_never_override_measurements() {
        let mut e = Extents::new();
        e.splice(0..0, [10.0; 3]);
        assert_eq!(e.measure(1, 30.0), 20.0);
        e.reestimate(false, |_| 14.0);
        assert_eq!((e.size(0), e.size(1), e.size(2)), (14.0, 30.0, 14.0));
        assert_eq!(e.total(), 58.0);
        e.splice(0..0, [5.0; 2]);
        assert_eq!(e.total(), 68.0);
        assert!(e.is_measured(3));
        e.splice(0..1, []);
        assert_eq!(e.total(), 63.0);
        // A width change forgets measurements.
        e.reestimate(true, |_| 1.0);
        assert_eq!((e.total(), e.measured_count()), (4.0, 0));
    }
}
