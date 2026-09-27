//! Transform and clip tables.
//!
//! A transform record is a local affine plus a parent record. The
//! owner decides which nodes get one (scroll content, transformed
//! subtrees, window roots); everything else draws in its nearest
//! ancestor record's space through a per-chunk offset. So a scroll or a
//! transform animation patches one record, and world matrices are a
//! derived cache over the (small) record set, recomputed when any local
//! changes.
//!
//! A clip record is a rounded rect in some transform record's space,
//! chained to a parent clip. The GPU form carries the inverse world
//! matrix so the fragment stage tests clips in their own space, which
//! stays correct under rotation and scale.

use bytemuck::{Pod, Zeroable};
use craie_core::dirty::DirtyRanges;
use craie_core::geom::{Affine, Rect};

/// "No record" marker for parents and clip references.
pub const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub struct TransformRecord {
    pub local: Affine,
    pub parent: u32,
    /// Chunks in this space snap their origin (and rect edges) to the
    /// device-pixel grid. Off for spaces that move by fractions (a
    /// transformed subtree), so motion stays smooth. Inherited: a
    /// record snaps only if its ancestors do.
    pub snap: bool,
}

/// World matrix row as the GPU reads it: 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct WorldGpu {
    pub m: [f32; 6],
    /// Bit 0: axis-aligned. Bit 1: snap chunk origins and rect edges to
    /// device pixels (only meaningful when axis-aligned).
    pub flags: u32,
    pub _pad: u32,
}

#[derive(Default)]
pub struct Transforms {
    records: Vec<TransformRecord>,
    live: Vec<bool>,
    free: Vec<u32>,
    world: Vec<Affine>,
    /// Derived: the record and all its ancestors snap.
    snap_world: Vec<bool>,
    gpu: Vec<WorldGpu>,
    gpu_dirty: DirtyRanges,
    /// Evaluation order: parents before children. Set by the owner.
    order: Vec<u32>,
    stale: bool,
    /// Bumped whenever a world matrix changes.
    pub world_rev: u64,
    /// Local records written (cost counter).
    pub writes: u64,
}

impl Transforms {
    pub fn alloc(&mut self, local: Affine, parent: u32) -> u32 {
        let rec = TransformRecord {
            local,
            parent,
            snap: true,
        };
        self.stale = true;
        self.writes += 1;
        // A NaN world never equals a derived one: the first `derive`
        // writes and uploads the row even when it is the identity.
        let unset = Affine([f32::NAN; 6]);
        if let Some(id) = self.free.pop() {
            self.records[id as usize] = rec;
            self.live[id as usize] = true;
            self.world[id as usize] = unset;
            id
        } else {
            self.records.push(rec);
            self.live.push(true);
            self.world.push(unset);
            self.snap_world.push(true);
            self.gpu.push(WorldGpu::default());
            (self.records.len() - 1) as u32
        }
    }

    /// Sets whether chunks in this space snap to the pixel grid.
    pub fn set_snap(&mut self, id: u32, snap: bool) {
        let r = &mut self.records[id as usize];
        if r.snap != snap {
            r.snap = snap;
            self.stale = true;
            // Force the row out even if the matrix is unchanged.
            self.world[id as usize] = Affine([f32::NAN; 6]);
        }
    }

    /// Whether chunks in this space snap (derived; valid after `derive`).
    pub fn snaps(&self, id: u32) -> bool {
        self.snap_world[id as usize]
    }

    pub fn free(&mut self, id: u32) {
        self.live[id as usize] = false;
        self.free.push(id);
    }

    pub fn get(&self, id: u32) -> &TransformRecord {
        &self.records[id as usize]
    }

    /// Writes a local matrix; no-op when unchanged.
    pub fn set_local(&mut self, id: u32, local: Affine) {
        let r = &mut self.records[id as usize];
        if r.local != local {
            r.local = local;
            self.stale = true;
            self.writes += 1;
        }
    }

    pub fn set_parent(&mut self, id: u32, parent: u32) {
        let r = &mut self.records[id as usize];
        if r.parent != parent {
            r.parent = parent;
            self.stale = true;
        }
    }

    /// Parents-first evaluation order over live records.
    pub fn set_order(&mut self, order: Vec<u32>) {
        self.order = order;
        self.stale = true;
    }

    pub fn world(&self, id: u32) -> Affine {
        self.world[id as usize]
    }

    /// Recomputes world matrices when any local or parent changed.
    /// Returns true when something was recomputed.
    pub fn derive(&mut self) -> bool {
        if !self.stale {
            return false;
        }
        self.stale = false;
        let mut changed = false;
        for &id in &self.order {
            let r = self.records[id as usize];
            let (w, snap) = if r.parent == NONE {
                (r.local, r.snap)
            } else {
                let p = r.parent as usize;
                (self.world[p].mul(&r.local), r.snap && self.snap_world[p])
            };
            if self.world[id as usize] != w || self.snap_world[id as usize] != snap {
                self.world[id as usize] = w;
                self.snap_world[id as usize] = snap;
                self.gpu[id as usize] = WorldGpu {
                    m: w.0,
                    flags: w.is_axis_aligned() as u32 | (snap as u32) << 1,
                    _pad: 0,
                };
                self.gpu_dirty.add(id as usize..id as usize + 1);
                changed = true;
            }
        }
        if changed {
            self.world_rev += 1;
        }
        changed
    }

    pub fn gpu_rows(&self) -> &[WorldGpu] {
        &self.gpu
    }

    pub fn take_gpu_dirty(&mut self) -> Vec<std::ops::Range<usize>> {
        self.gpu_dirty.take()
    }

    /// `take_gpu_dirty` into a reused buffer (replaced).
    pub fn take_gpu_dirty_into(&mut self, out: &mut Vec<std::ops::Range<usize>>) {
        self.gpu_dirty.take_into(out);
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipRecord {
    /// Clip rect in the space of `transform`. Ignored along open axes.
    pub rect: Rect,
    pub radius: f32,
    pub transform: u32,
    pub parent: u32,
    /// Axes the clip leaves unbounded (x, y): overflow visible there.
    pub open: [bool; 2],
}

/// Clip row as the GPU reads it: 52 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct ClipGpu {
    /// Inverse world matrix of the clip's space (device px -> local).
    pub inv: [f32; 6],
    /// x0, y0, x1, y1 in local units.
    pub rect: [f32; 4],
    pub radius: f32,
    pub parent: u32,
    /// Bit 0: x unbounded. Bit 1: y unbounded.
    pub flags: u32,
}

#[derive(Default)]
pub struct Clips {
    records: Vec<ClipRecord>,
    gpu: Vec<ClipGpu>,
    gpu_dirty: DirtyRanges,
    stale: bool,
    world_rev: u64,
    /// Bumped whenever a derived clip row changes.
    pub rev: u64,
}

impl Clips {
    /// Replaces every clip record (the owner rebuilds clips with the
    /// draw order).
    pub fn set_all(&mut self, records: Vec<ClipRecord>) {
        self.records = records;
        self.stale = true;
    }

    /// Patches one record's shape (layout change without a structure
    /// change).
    pub fn set_rect(&mut self, id: u32, rect: Rect, radius: f32, open: [bool; 2]) {
        let r = &mut self.records[id as usize];
        if r.rect != rect || r.radius != radius || r.open != open {
            r.rect = rect;
            r.radius = radius;
            r.open = open;
            self.stale = true;
        }
    }

    pub fn get(&self, id: u32) -> &ClipRecord {
        &self.records[id as usize]
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Recomputes GPU rows when records or world matrices changed.
    pub fn derive(&mut self, transforms: &Transforms) {
        if !self.stale && self.world_rev == transforms.world_rev {
            return;
        }
        self.stale = false;
        self.world_rev = transforms.world_rev;
        if self.gpu.len() != self.records.len() {
            self.gpu.resize(self.records.len(), ClipGpu::default());
            self.rev += 1;
        }
        for (i, r) in self.records.iter().enumerate() {
            let inv = transforms
                .world(r.transform)
                .invert()
                .unwrap_or(Affine([0.0; 6]));
            let row = ClipGpu {
                inv: inv.0,
                rect: [
                    r.rect.origin.x,
                    r.rect.origin.y,
                    r.rect.max_x(),
                    r.rect.max_y(),
                ],
                radius: r.radius,
                parent: r.parent,
                flags: r.open[0] as u32 | (r.open[1] as u32) << 1,
            };
            if self.gpu[i] != row {
                self.gpu[i] = row;
                self.gpu_dirty.add(i..i + 1);
                self.rev += 1;
            }
        }
    }

    /// World-space bounding box of a clip chain (for culling). Records
    /// with an open axis bound nothing here (conservative).
    pub fn world_bounds(&self, transforms: &Transforms, mut id: u32) -> Option<Rect> {
        let mut out: Option<Rect> = None;
        while id != NONE {
            let r = &self.records[id as usize];
            if r.open[0] || r.open[1] {
                id = r.parent;
                continue;
            }
            let b = transforms.world(r.transform).map_rect(&r.rect);
            out = Some(match out {
                Some(o) => o.intersect(&b),
                None => b,
            });
            id = r.parent;
        }
        out
    }

    pub fn gpu_rows(&self) -> &[ClipGpu] {
        &self.gpu
    }

    pub fn take_gpu_dirty(&mut self) -> Vec<std::ops::Range<usize>> {
        self.gpu_dirty.take()
    }

    /// `take_gpu_dirty` into a reused buffer (replaced).
    pub fn take_gpu_dirty_into(&mut self, out: &mut Vec<std::ops::Range<usize>>) {
        self.gpu_dirty.take_into(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craie_core::geom::Point;

    #[test]
    fn world_derives_parent_first() {
        let mut t = Transforms::default();
        let root = t.alloc(Affine::scale(2.0, 2.0), NONE);
        let child = t.alloc(Affine::translate(10.0, 0.0), root);
        t.set_order(vec![root, child]);
        assert!(t.derive());
        assert_eq!(
            t.world(child).apply(Point::new(1.0, 1.0)),
            Point::new(22.0, 2.0)
        );
        // Unchanged: nothing recomputed, nothing dirty.
        t.take_gpu_dirty();
        assert!(!t.derive());
        // Patch the child only: one dirty row.
        t.set_local(child, Affine::translate(20.0, 0.0));
        assert!(t.derive());
        assert_eq!(t.take_gpu_dirty(), vec![child as usize..child as usize + 1]);
    }

    #[test]
    fn clip_bounds_intersect_chain() {
        let mut t = Transforms::default();
        let root = t.alloc(Affine::IDENTITY, NONE);
        t.set_order(vec![root]);
        t.derive();
        let mut c = Clips::default();
        c.set_all(vec![
            ClipRecord {
                rect: Rect::new(0.0, 0.0, 100.0, 100.0),
                radius: 0.0,
                transform: root,
                parent: NONE,
                open: [false; 2],
            },
            ClipRecord {
                rect: Rect::new(50.0, 50.0, 100.0, 100.0),
                radius: 4.0,
                transform: root,
                parent: 0,
                open: [false; 2],
            },
        ]);
        c.derive(&t);
        assert_eq!(
            c.world_bounds(&t, 1),
            Some(Rect::new(50.0, 50.0, 50.0, 50.0))
        );
    }
}
