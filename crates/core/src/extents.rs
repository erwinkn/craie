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
        self.splice_items(range, estimates.into_iter().map(|e| (e, false)));
    }

    /// `splice` with (extent, measured) per new item: an item moved
    /// within one splice keeps its measurement. O(n) (one rebuild),
    /// except a pure append: O(k log n), so streaming appends stay cheap.
    pub fn splice_items(
        &mut self,
        range: Range<usize>,
        items: impl IntoIterator<Item = (f32, bool)>,
    ) {
        let n = self.size.len();
        let range = range.start.min(n)..range.end.min(n).max(range.start.min(n));
        if range.start == n {
            for (e, m) in items {
                self.push(e, m);
            }
            return;
        }
        let (size, measured): (Vec<f32>, Vec<bool>) = items.into_iter().unzip();
        self.size.splice(range.clone(), size);
        self.measured.splice(range, measured);
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

    /// Appends one item: Fenwick node `j` covers items (j - lowbit(j), j],
    /// so its sum is the new size plus the prefix it spans. O(log n).
    fn push(&mut self, e: f32, measured: bool) {
        self.size.push(e);
        self.measured.push(measured);
        let j = self.size.len();
        if self.tree.is_empty() {
            self.tree.push(0.0);
        }
        let span = self.offset_f64(j - 1) - self.offset_f64(j - (j & j.wrapping_neg()));
        self.tree.push(e as f64 + span);
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

    /// Records a measurement. Returns the change in extent. The delta is
    /// taken in f64: an f32 difference of widely differing values rounds
    /// and would leave the tree off until a rebuild.
    pub fn measure(&mut self, i: usize, v: f32) -> f32 {
        self.measured[i] = true;
        let d = v as f64 - self.size[i] as f64;
        if d != 0.0 {
            self.size[i] = v;
            self.add(i, d);
        }
        d as f32
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

    /// Offset of item `i` with `gap` between items: `offset(i) + i·gap`.
    pub fn offset_gap(&self, i: usize, gap: f32) -> f32 {
        let i = i.min(self.size.len());
        (self.offset_f64(i) + i as f64 * gap as f64) as f32
    }

    /// Total extent with `gap` between items (none after the last).
    pub fn total_gap(&self, gap: f32) -> f32 {
        let n = self.size.len();
        (self.offset_f64(n) + n.saturating_sub(1) as f64 * gap as f64) as f32
    }

    /// `index_at` with `gap` between items: the item whose span (with
    /// the gap after it) contains `y`. O(log n): a prefix of `k` items
    /// holds `k` gaps, so the descent adds `step · gap` per step.
    pub fn index_at_gap(&self, y: f32, gap: f32) -> usize {
        let n = self.size.len();
        if n == 0 {
            return 0;
        }
        let g = gap as f64;
        let mut pos = 0usize;
        let mut rem = y as f64;
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n && self.tree[next] + step as f64 * g <= rem {
                pos = next;
                rem -= self.tree[next] + step as f64 * g;
            }
            step >>= 1;
        }
        pos.min(n - 1)
    }

    /// `total` without rounding to f32 (for sums that add corrections).
    pub fn total_f64(&self) -> f64 {
        self.offset_f64(self.size.len())
    }

    fn offset_f64(&self, i: usize) -> f64 {
        let mut j = i.min(self.size.len());
        let mut s = 0.0;
        while j > 0 {
            s += self.tree[j];
            j -= j & j.wrapping_neg();
        }
        s
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
                // Fractional, and sometimes far from the old value.
                let v = match rng.below(4) {
                    0 => 1.0e6 * rng.unit(),
                    1 => 0.01 + rng.unit(),
                    _ => 5.0 + rng.below(80) as f32 + rng.unit(),
                };
                e.measure(i, v);
                model[i] = v;
            }
            if round % 50 == 0 || round == 399 {
                // The model sums in f64; offsets agree to f32 precision.
                let mut acc = 0.0f64;
                for (i, &v) in model.iter().enumerate() {
                    let tol = 1e-6 * acc.max(1.0) + 1e-3;
                    assert!((e.offset(i) as f64 - acc).abs() < tol, "offset {i}");
                    if v > 1.0 {
                        assert_eq!(e.index_at((acc + v as f64 / 2.0) as f32), i);
                    }
                    acc += v as f64;
                }
                assert!((e.total() as f64 - acc).abs() < 1e-6 * acc.max(1.0) + 1e-3);
                assert_eq!(e.index_at(-5.0), 0);
                assert_eq!(e.index_at(acc as f32 + 100.0), model.len() - 1);
            }
        }
    }

    /// Appends grow the Fenwick tree in place: append into a populated,
    /// measured tree across power-of-two sizes, measure old and new items,
    /// splice in the middle (a rebuild), append again. Every step equals
    /// a linear model and a tree rebuilt from scratch.
    #[test]
    fn appends_match_model_across_powers_of_two() {
        let mut e = Extents::new();
        let mut model: Vec<f64> = Vec::new();
        let check = |e: &Extents, model: &[f64], what: &str| {
            let mut acc = 0.0;
            for (i, v) in model.iter().enumerate() {
                assert!(
                    (e.offset(i) as f64 - acc).abs() < 1e-3,
                    "{what}: offset {i}"
                );
                acc += v;
            }
            assert!((e.total() as f64 - acc).abs() < 1e-3, "{what}: total");
            let mut fresh = Extents::new();
            fresh.splice(0..0, model.iter().map(|&v| v as f32));
            assert_eq!(fresh.tree.len(), e.tree.len(), "{what}");
            for (j, (a, b)) in e.tree.iter().zip(&fresh.tree).enumerate() {
                assert!((a - b).abs() < 1e-6, "{what}: node {j}: {a} vs {b}");
            }
        };
        let append = |e: &mut Extents, model: &mut Vec<f64>, to: usize| {
            let from = model.len();
            let vals: Vec<f32> = (from..to).map(|i| 10.0 + (i % 7) as f32 + 0.25).collect();
            e.splice(from..from, vals.iter().copied());
            model.extend(vals.iter().map(|&v| v as f64));
        };
        // Across 1, 2, 4, 8, 16, 32, 64 (one item at a time and in runs).
        for to in [1, 2, 3, 4, 5, 8, 9, 16, 17, 31, 32, 33, 64, 65] {
            append(&mut e, &mut model, to);
            check(&e, &model, &format!("append to {to}"));
        }
        // Measure old and new items.
        for (i, v) in [
            (0usize, 3.5f32),
            (7, 40.0),
            (15, 0.5),
            (16, 22.0),
            (63, 1.0),
            (64, 9.0),
        ] {
            e.measure(i, v);
            model[i] = v as f64;
        }
        check(&e, &model, "measured");
        // A middle splice (rebuild), then appends across the next power.
        e.splice(20..25, [5.0, 6.0]);
        model.splice(20..25, [5.0, 6.0]);
        check(&e, &model, "middle splice");
        e.splice(3..3, [1.0; 4]);
        model.splice(3..3, [1.0; 4]);
        check(&e, &model, "middle insert");
        let n = model.len();
        append(&mut e, &mut model, n + 70);
        e.measure(n + 5, 77.0);
        model[n + 5] = 77.0;
        check(&e, &model, "append after splices, measured");
        // The item measured at 16 moved to 20 with the insert at 3.
        assert!(e.is_measured(20) && e.is_measured(n + 5) && !e.is_measured(n + 6));
    }

    /// Gap-aware offsets and lookup equal a linear model with gaps.
    #[test]
    fn gaps_match_linear_model() {
        let sizes = [10.0f32, 25.5, 3.0, 40.0, 7.25];
        let mut e = Extents::new();
        e.splice(0..0, sizes);
        let g = 6.0;
        let mut acc = 0.0f32;
        for (i, v) in sizes.iter().enumerate() {
            assert_eq!(e.offset_gap(i, g), acc);
            assert_eq!(e.index_at_gap(acc + v / 2.0, g), i);
            // The gap after an item belongs to it.
            assert_eq!(e.index_at_gap(acc + v + g / 2.0, g), i);
            acc += v + g;
        }
        assert_eq!(e.total_gap(g), acc - g);
    }

    /// A measurement far below the old extent updates the sums exactly
    /// (an f32 delta would round 0.01 - 1e6 to -1e6).
    #[test]
    fn wide_measurement_keeps_sums() {
        let mut e = Extents::new();
        e.splice(0..0, [1.0e6, 5.0]);
        e.measure(0, 0.01);
        assert!((e.total() - 5.01).abs() < 1e-4, "{}", e.total());
        assert!((e.offset(1) - 0.01).abs() < 1e-6);
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
